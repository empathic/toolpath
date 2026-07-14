//! `path p cache sync` — incremental ingestion of artifacts into the
//! document cache — and [`ArtifactType`], the single enum naming the
//! artifact sources the CLI operates over.
//!
//! Sync enumerates artifacts across the requested types (today all six
//! are agent-session providers), compares each against the sync
//! manifest at `$CONFIG_DIR/sync.json`, and derives + caches only what
//! is new or changed. Change detection is stat-level: the fingerprint
//! is the source file's mtime + size (or the database row's updated-at
//! for the SQLite-backed providers), so deciding "nothing changed"
//! never reads session bodies. Artifacts deleted upstream keep both
//! their cache document and their manifest record — the cache is an
//! archive, not a mirror.

/// The kind of artifact an operation ranges over. One enum, used
/// everywhere a command names artifact sources (`p cache sync` types,
/// `share`/`resume` `--harness`, import cache-id prefixes); `name()`
/// doubles as the manifest key and cache-id prefix. Git artifacts are
/// recorded in the manifest when imported but are not *discoverable* —
/// there is no machine-wide registry of repos to enumerate — so sync
/// never re-derives them. Github and pathbase are absent on purpose:
/// they are remote services, not local artifact sources.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ArtifactType {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Cursor,
    Pi,
    Copilot,
    Git,
}

impl ArtifactType {
    /// Every artifact type, in presentation order.
    pub(crate) const ALL: [ArtifactType; 8] = [
        ArtifactType::Claude,
        ArtifactType::Gemini,
        ArtifactType::Codex,
        ArtifactType::Opencode,
        ArtifactType::Cursor,
        ArtifactType::Pi,
        ArtifactType::Copilot,
        ArtifactType::Git,
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            ArtifactType::Claude => "claude",
            ArtifactType::Gemini => "gemini",
            ArtifactType::Codex => "codex",
            ArtifactType::Opencode => "opencode",
            ArtifactType::Cursor => "cursor",
            ArtifactType::Pi => "pi",
            ArtifactType::Copilot => "copilot",
            ArtifactType::Git => "git",
        }
    }

    /// Padded so all symbols line up in the fzf column. Longest is
    /// "opencode" (8); pad shorter names to match.
    pub(crate) fn symbol(&self) -> &'static str {
        match self {
            ArtifactType::Claude => "claude  ",
            ArtifactType::Gemini => "gemini  ",
            ArtifactType::Codex => "codex   ",
            ArtifactType::Opencode => "opencode",
            ArtifactType::Cursor => "cursor  ",
            ArtifactType::Pi => "pi      ",
            ArtifactType::Copilot => "copilot ",
            ArtifactType::Git => "git     ",
        }
    }

    /// True when the underlying provider keys artifacts by a filesystem
    /// path (the project directory). claude/gemini/pi: true.
    /// codex/opencode/cursor: false (sessions store cwd per-row, not as
    /// a directory key — cursor stores it as
    /// `workspaceIdentifier.uri.fsPath` on each composer).
    pub(crate) fn path_keyed(&self) -> bool {
        matches!(
            self,
            ArtifactType::Claude | ArtifactType::Gemini | ArtifactType::Pi | ArtifactType::Git
        )
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        <Self as clap::ValueEnum>::from_str(s, false).ok()
    }
}

/// An artifact's identity plus the stat-level fingerprint of its
/// source. Sync enumerates these for change detection (producing one
/// never parses session bodies), and `p import`/`share` fill one as
/// the provenance of each derived document so the write can be
/// recorded in the manifest.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactStub {
    pub(crate) artifact_type: ArtifactType,
    pub(crate) id: String,
    /// Filesystem path the artifact is keyed under, for path-keyed
    /// providers (the project directory; the repo for git).
    pub(crate) path: Option<String>,
    /// Source mtime (file providers) or updated-at (DB providers).
    pub(crate) modified: Option<chrono::DateTime<chrono::Utc>>,
    /// Source file size; `None` for DB-backed providers.
    pub(crate) size: Option<u64>,
}

/// (mtime, size) of a file, both `None` when the stat fails.
pub(crate) fn stat_stamp(
    path: &std::path::Path,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(md) => (
            md.modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from),
            Some(md.len()),
        ),
        Err(_) => (None, None),
    }
}

/// Stat-level fingerprint of a whole claude session chain: max mtime
/// across the chain's segment files plus the sum of their sizes. Claude
/// Code rotates to a new file on continuation (plan-mode exit, resume,
/// fork) while the chain keeps the *first* segment's id — appends land
/// in the newest segment, so statting the head file alone would freeze
/// the fingerprint at the first rotation and sync would never see the
/// later turns. The chain here is exactly the set of files
/// `read_conversation` merges, so the fingerprint and the derived doc
/// move in lockstep. The chain index is already built (and cached) by
/// the `list_conversations` call every caller makes first.
pub(crate) fn claude_chain_stamp(
    mgr: &toolpath_claude::ClaudeConvo,
    project: &str,
    session: &str,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<u64>) {
    let segments = match mgr.session_chain(project, session) {
        Ok(segments) if !segments.is_empty() => segments,
        _ => vec![session.to_string()],
    };
    let mut modified: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut size: Option<u64> = None;
    for segment in &segments {
        let Ok(file) = mgr.resolver().conversation_file(project, segment) else {
            continue;
        };
        let (m, s) = stat_stamp(&file);
        if let Some(m) = m {
            modified = Some(modified.map_or(m, |cur| cur.max(m)));
        }
        if let Some(s) = s {
            size = Some(size.unwrap_or(0) + s);
        }
    }
    (modified, size)
}

/// The trailing UUID of a codex rollout filename stem
/// (`rollout-<timestamp>-<uuid>`), or the whole stem when it doesn't end
/// in one. Codex's `read_session` resolves either form.
pub(crate) fn codex_artifact_id(stem: &str) -> &str {
    stem.len()
        .checked_sub(36)
        .and_then(|at| stem.get(at..))
        .filter(|tail| tail.bytes().filter(|&b| b == b'-').count() == 4)
        .unwrap_or(stem)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) use engine::*;

#[cfg(not(target_os = "emscripten"))]
mod engine {
    use anyhow::{Context, Result, anyhow};
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::{ArtifactStub, ArtifactType, codex_artifact_id, stat_stamp};
    use crate::cmd_cache::write_cached;
    use crate::cmd_import::DerivedDoc;
    use crate::cmd_share::{
        HarnessBundle, is_not_found_claude, is_not_found_codex, is_not_found_cursor,
        is_not_found_gemini, is_not_found_opencode, is_not_found_pi,
    };
    use crate::config::config_dir;

    const MANIFEST_FILE: &str = "sync.json";

    /// What the manifest remembers about one known artifact. A record
    /// with a `cache_id` is materialized in the cache; one without is
    /// merely known — seen during an out-of-scope sync, or evicted by
    /// `p cache rm`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct SyncRecord {
        /// Filesystem path the artifact is keyed under: the project
        /// directory for path-keyed providers, the recorded cwd /
        /// workspace for the others (when known).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) path: Option<String>,
        /// Cache entry the derived document was written to; `None`
        /// when the artifact is known but not cached.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) cache_id: Option<String>,
        /// Fingerprint: source mtime (file providers) or updated-at
        /// (DB providers) at sync time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) modified: Option<DateTime<Utc>>,
        /// Fingerprint: source file size at sync time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) size: Option<u64>,
        pub(crate) synced_at: DateTime<Utc>,
    }

    /// The sync manifest: artifact type (`"claude"`, `"codex"`, …) →
    /// artifact id → record. Kept as `BTreeMap`s so the JSON on disk is
    /// stably ordered.
    pub(crate) type Manifest = BTreeMap<String, BTreeMap<String, SyncRecord>>;

    /// Per-type tally of what one sync run did.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct SyncOutcome {
        pub(crate) new: usize,
        pub(crate) updated: usize,
        pub(crate) unchanged: usize,
        pub(crate) failed: usize,
        /// Artifacts needing work that a `--parent-dir` constraint excluded.
        pub(crate) out_of_scope: usize,
    }

    impl SyncOutcome {
        fn total(&self) -> usize {
            self.new + self.updated + self.unchanged + self.failed + self.out_of_scope
        }
    }

    pub(crate) fn run(types: Vec<ArtifactType>, parent_dir: Option<PathBuf>) -> Result<()> {
        let explicit = !types.is_empty();
        let types = resolve_types(&types);
        let bundle = HarnessBundle::from_environment();
        let outcomes = sync_bundle(&bundle, &types, parent_dir.as_deref())?;
        eprint!("{}", render_summary(&outcomes, explicit));
        Ok(())
    }

    /// Explicit args → dedup'd type list; no args → every type.
    fn resolve_types(args: &[ArtifactType]) -> Vec<ArtifactType> {
        if args.is_empty() {
            return ArtifactType::ALL.to_vec();
        }
        let mut out: Vec<ArtifactType> = Vec::with_capacity(args.len());
        for &t in args {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    /// Sync the given artifact types from `bundle` into the cache,
    /// newest artifacts first. The manifest is checkpointed every few
    /// writes (see [`CHECKPOINT_EVERY`]), so an interrupted run keeps
    /// nearly everything it derived. Reads come from a point-in-time
    /// snapshot; each checkpoint merges only the records this run
    /// wrote, under the manifest lock, so concurrent invocations
    /// (query auto-syncs, imports) union their records instead of
    /// clobbering each other.
    pub(crate) fn sync_bundle(
        bundle: &HarnessBundle,
        types: &[ArtifactType],
        parent_dir: Option<&Path>,
    ) -> Result<Vec<(ArtifactType, SyncOutcome)>> {
        let manifest = load_manifest()?;
        let mut out = Vec::with_capacity(types.len());
        for &artifact_type in types {
            let stubs = enumerate_stubs(bundle, artifact_type, parent_dir);
            let records = manifest
                .get(artifact_type.name())
                .cloned()
                .unwrap_or_default();
            let outcome = sync_stubs(bundle, &stubs, &records, parent_dir)?;
            out.push((artifact_type, outcome));
        }
        Ok(out)
    }

    /// How many manifest writes accumulate before a mid-run checkpoint.
    /// Small enough that an interrupted first sync loses at most a few
    /// records (the cache docs themselves survive either way); large
    /// enough that manifest serialization stays noise against the
    /// derives it punctuates.
    const CHECKPOINT_EVERY: usize = 10;

    /// The stat gate: a materialized record whose real stamps match the
    /// stub needs nothing — no read, no scope check. All-`None` stamps
    /// mean freshness is unknowable; only a real stamp can vouch
    /// (mirrors `record_is_current`).
    fn is_unchanged(rec: Option<&SyncRecord>, stub: &ArtifactStub) -> bool {
        rec.is_some_and(|rec| {
            (rec.modified.is_some() || rec.size.is_some())
                && rec.modified == stub.modified
                && rec.size == stub.size
                && rec
                    .cache_id
                    .as_deref()
                    .is_some_and(|id| crate::cmd_cache::cache_path(id).is_ok_and(|p| p.exists()))
        })
    }

    /// Stubs newest-first (unstamped last), so an interrupted run has
    /// spent its time on the sessions the user most likely wants.
    fn newest_first(stubs: &[ArtifactStub]) -> Vec<&ArtifactStub> {
        let mut order: Vec<&ArtifactStub> = stubs.iter().collect();
        order.sort_by(|a, b| b.modified.cmp(&a.modified));
        order
    }

    /// Merge staged records into the manifest under the lock and clear
    /// the stage.
    fn flush_writes(
        pending: &mut BTreeMap<&'static str, BTreeMap<String, SyncRecord>>,
    ) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(pending);
        update_manifest(move |manifest| {
            for (name, records) in batch {
                manifest
                    .entry(name.to_string())
                    .or_default()
                    .extend(records);
            }
        })
    }

    /// Sync one type's stubs against a snapshot of its manifest records,
    /// newest first. Records are checkpointed to the manifest every
    /// [`CHECKPOINT_EVERY`] writes (and once more at the end), so an
    /// interrupted run keeps nearly everything it derived. Derivation
    /// failures are warned and tallied, not fatal; cache-write failures
    /// (disk, permissions) abort.
    fn sync_stubs(
        bundle: &HarnessBundle,
        stubs: &[ArtifactStub],
        records: &BTreeMap<String, SyncRecord>,
        parent_dir: Option<&Path>,
    ) -> Result<SyncOutcome> {
        let mut outcome = SyncOutcome::default();
        // Evaluate the stat gate once per stub: the pass feeds both the
        // progress denominator and the loop's skip decision.
        let order: Vec<(&ArtifactStub, bool)> = newest_first(stubs)
            .into_iter()
            .map(|stub| (stub, is_unchanged(records.get(&stub.id), stub)))
            .collect();
        let pending_total = order.iter().filter(|(_, unchanged)| !unchanged).count();
        let mut progress = Progress::start(
            stubs
                .first()
                .map(|s| s.artifact_type.symbol())
                .unwrap_or(""),
            pending_total,
        );
        let mut writes: BTreeMap<&'static str, BTreeMap<String, SyncRecord>> = BTreeMap::new();
        let mut unflushed = 0usize;
        for (stub, unchanged) in order {
            if unchanged {
                outcome.unchanged += 1;
                continue;
            }
            let existing = records.get(&stub.id);
            let is_new = existing.is_none();
            let stage = |writes: &mut BTreeMap<&'static str, BTreeMap<String, SyncRecord>>,
                         record: SyncRecord| {
                writes
                    .entry(stub.artifact_type.name())
                    .or_default()
                    .insert(stub.id.clone(), record);
            };
            // Scope gate: only artifacts that would cost a derive get the
            // constraint check (with a bounded peek for codex, memoized in
            // the record so it happens at most once per artifact).
            if let Some(parent_dir) = parent_dir {
                let dir = stub
                    .path
                    .clone()
                    .or_else(|| existing.and_then(|r| r.path.clone()))
                    .or_else(|| peek_stub_dir(bundle, stub));
                let in_scope = dir.as_deref().is_some_and(|d| match stub.artifact_type {
                    // Claude and pi paths came from lossy dir encodings;
                    // compare in their encoded spaces, like the
                    // enumeration pruning does.
                    ArtifactType::Claude => claude_project_in_scope(d, parent_dir),
                    ArtifactType::Pi => pi_project_in_scope(d, parent_dir),
                    _ => dir_in_scope(d, parent_dir),
                });
                if !in_scope {
                    outcome.out_of_scope += 1;
                    // Remember what we learned — but never touch the stamp
                    // of a materialized record, or its staleness would be
                    // masked from the next in-scope sync.
                    if existing.is_none_or(|r| r.cache_id.is_none()) {
                        stage(
                            &mut writes,
                            SyncRecord {
                                path: dir,
                                cache_id: None,
                                modified: stub.modified,
                                size: stub.size,
                                synced_at: Utc::now(),
                            },
                        );
                        unflushed += 1;
                    }
                    progress.tick();
                    if unflushed >= CHECKPOINT_EVERY {
                        flush_writes(&mut writes)?;
                        unflushed = 0;
                    }
                    continue;
                }
            }
            // A stub without a path must not erase one a previous pass
            // peeked and memoized (codex/copilot cwd).
            let memoized_path = stub
                .path
                .clone()
                .or_else(|| existing.and_then(|r| r.path.clone()));
            match derive_stub(bundle, stub) {
                Ok(derived) => {
                    // force: sync owns refresh semantics — a re-sync or a
                    // prior manual `p import` of the same session must not
                    // error on the existing cache entry.
                    write_cached(&derived.cache_id, &derived.doc, true)?;
                    stage(
                        &mut writes,
                        SyncRecord {
                            path: memoized_path,
                            cache_id: Some(derived.cache_id),
                            // The stamp was taken before the derive read the
                            // source, so a write racing the derive re-syncs
                            // next run instead of going unnoticed.
                            modified: stub.modified,
                            size: stub.size,
                            synced_at: Utc::now(),
                        },
                    );
                    unflushed += 1;
                    if is_new {
                        outcome.new += 1;
                    } else {
                        outcome.updated += 1;
                    }
                }
                Err(e) => {
                    progress.interrupt();
                    eprintln!(
                        "warning: sync {}: {}: {e}",
                        stub.artifact_type.name(),
                        stub.id
                    );
                    outcome.failed += 1;
                }
            }
            progress.tick();
            if unflushed >= CHECKPOINT_EVERY {
                flush_writes(&mut writes)?;
                unflushed = 0;
            }
        }
        flush_writes(&mut writes)?;
        progress.interrupt();
        Ok(outcome)
    }

    /// Live sync progress on stderr: a `\r`-updating `<type> done/total`
    /// line on a terminal, a plain line every 25 items otherwise. Only
    /// artifacts needing work count toward the total — a no-op sync
    /// draws nothing.
    struct Progress {
        label: &'static str,
        total: usize,
        done: usize,
        tty: bool,
    }

    impl Progress {
        fn start(label: &'static str, total: usize) -> Self {
            use std::io::IsTerminal;
            let progress = Self {
                label,
                total,
                done: 0,
                tty: std::io::stderr().is_terminal(),
            };
            progress.draw();
            progress
        }

        fn line(&self) -> String {
            format!("{} {}/{}", self.label, self.done, self.total)
        }

        fn draw(&self) {
            if self.total > 0 && self.tty {
                eprint!("\r{}", self.line());
            }
        }

        fn tick(&mut self) {
            if self.total == 0 {
                return;
            }
            self.done += 1;
            if self.tty {
                self.draw();
            } else if self.done.is_multiple_of(25) {
                eprintln!("{}", self.line());
            }
        }

        /// Clear the live line so a warning or summary prints clean;
        /// the next `tick` redraws in full.
        fn interrupt(&self) {
            if self.total > 0 && self.tty {
                eprint!("\r\x1b[2K");
            }
        }
    }

    /// Derive one artifact through the same manager it was enumerated
    /// from, so listing and derivation always agree on provider roots.
    fn derive_stub(bundle: &HarnessBundle, stub: &ArtifactStub) -> Result<DerivedDoc> {
        use crate::cmd_import as imp;
        let path = || {
            stub.path
                .as_deref()
                .ok_or_else(|| anyhow!("artifact {} has no path", stub.id))
        };
        match stub.artifact_type {
            ArtifactType::Claude => {
                imp::derive_claude_session_with(mgr(&bundle.claude)?, path()?, &stub.id)
            }
            ArtifactType::Gemini => {
                imp::derive_gemini_session_with(mgr(&bundle.gemini)?, path()?, &stub.id)
            }
            ArtifactType::Pi => imp::derive_pi_session_with(mgr(&bundle.pi)?, path()?, &stub.id),
            ArtifactType::Codex => imp::derive_codex_session_with(mgr(&bundle.codex)?, &stub.id),
            ArtifactType::Opencode => {
                imp::derive_opencode_session_with(mgr(&bundle.opencode)?, &stub.id, false)
            }
            ArtifactType::Cursor => imp::derive_cursor_session_with(mgr(&bundle.cursor)?, &stub.id),
            ArtifactType::Copilot => {
                imp::derive_copilot_session_with(mgr(&bundle.copilot)?, &stub.id)
            }
            ArtifactType::Git => Err(anyhow!(
                "git artifacts are recorded by `p import`, not re-derived by sync"
            )),
        }
    }

    fn mgr<T>(slot: &Option<T>) -> Result<&T> {
        slot.as_ref()
            .ok_or_else(|| anyhow!("provider not available"))
    }

    fn canonicalize_or_self(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// Subtree check for a real filesystem path. Canonicalizes both
    /// sides, but also accepts the raw parent so a not-yet-resolvable
    /// constraint (or an unresolvable dir) still matches literally.
    fn dir_in_scope(dir: &str, parent_dir: &Path) -> bool {
        let d = canonicalize_or_self(Path::new(dir));
        d.starts_with(canonicalize_or_self(parent_dir)) || d.starts_with(parent_dir)
    }

    /// Pi's session-dir names encode '/' as '-', and decoding restores
    /// every '-' to '/', so a decoded project string is wrong for any
    /// real path containing a hyphen. Compare in the encoded space,
    /// where '/' boundaries are '-'.
    fn pi_project_in_scope(project: &str, parent_dir: &Path) -> bool {
        fn enc(s: &str) -> String {
            s.replace('/', "-")
        }
        let p = enc(project);
        [
            enc(&parent_dir.to_string_lossy()),
            enc(&canonicalize_or_self(parent_dir).to_string_lossy()),
        ]
        .iter()
        .any(|parent| p == *parent || p.starts_with(&format!("{parent}-")))
    }

    /// Claude's project-dir slugs are lossy — '/', '_', and '.' all
    /// became '-', and un-sanitizing only restores '/'. Comparing real
    /// paths therefore misfilters any project containing '.' or '_';
    /// compare in slug space instead, where '/' boundaries are '-'.
    fn claude_project_in_scope(project: &str, parent_dir: &Path) -> bool {
        fn slug(s: &str) -> String {
            s.replace(['/', '_', '.'], "-")
        }
        let p = slug(project);
        [
            slug(&parent_dir.to_string_lossy()),
            slug(&canonicalize_or_self(parent_dir).to_string_lossy()),
        ]
        .iter()
        .any(|parent| p == *parent || p.starts_with(&format!("{parent}-")))
    }

    /// Where a stub's artifact lives, for providers whose cheap listing
    /// doesn't carry it. Codex is the only case: the rollout's first
    /// line is `session_meta` with the session cwd — one bounded read,
    /// and the result is memoized into the manifest record afterwards.
    fn peek_stub_dir(bundle: &HarnessBundle, stub: &ArtifactStub) -> Option<String> {
        let file = match stub.artifact_type {
            ArtifactType::Codex => bundle
                .codex
                .as_ref()?
                .resolver()
                .find_rollout_file(&stub.id)
                .ok()?,
            ArtifactType::Copilot => bundle
                .copilot
                .as_ref()?
                .resolver()
                .events_file(&stub.id)
                .ok()?,
            _ => return None,
        };
        use std::io::{BufRead, BufReader};
        let f = std::fs::File::open(file).ok()?;
        match stub.artifact_type {
            // Codex: the first line is `session_meta`; its payload
            // carries cwd directly.
            ArtifactType::Codex => {
                let mut line = String::new();
                BufReader::new(f).read_line(&mut line).ok()?;
                let v: serde_json::Value = serde_json::from_str(&line).ok()?;
                Some(v.get("payload")?.get("cwd")?.as_str()?.to_string())
            }
            // Copilot: `session.start` is usually first but the format
            // tolerates it appearing later, and older CLIs store cwd at
            // the top of the payload instead of under `context` — scan a
            // bounded number of lines and accept both shapes, mirroring
            // toolpath-copilot's own tolerance.
            ArtifactType::Copilot => BufReader::new(f).lines().take(10).find_map(|line| {
                let v: serde_json::Value = serde_json::from_str(&line.ok()?).ok()?;
                ["data", "payload"].iter().find_map(|env| {
                    let payload = v.get(env)?;
                    [payload.get("context").unwrap_or(payload), payload]
                        .iter()
                        .find_map(|scope| {
                            ["cwd", "workingDirectory", "working_dir"]
                                .iter()
                                .find_map(|k| Some(scope.get(k)?.as_str()?.to_string()))
                        })
                })
            }),
            _ => None,
        }
    }

    // ── stat-level enumeration ─────────────────────────────────────────

    /// Enumerate one type's artifacts with stat-level fingerprints.
    /// Providers that aren't installed produce no stubs; other listing
    /// errors warn and skip so one broken provider can't block the rest.
    fn enumerate_stubs(
        bundle: &HarnessBundle,
        t: ArtifactType,
        parent_dir: Option<&Path>,
    ) -> Vec<ArtifactStub> {
        let mut out = Vec::new();
        match t {
            ArtifactType::Claude => {
                if let Some(mgr) = &bundle.claude {
                    stubs_claude(mgr, parent_dir, &mut out);
                }
            }
            ArtifactType::Gemini => {
                if let Some(mgr) = &bundle.gemini {
                    stubs_gemini(mgr, parent_dir, &mut out);
                }
            }
            ArtifactType::Codex => {
                if let Some(mgr) = &bundle.codex {
                    stubs_codex(mgr, &mut out);
                }
            }
            ArtifactType::Opencode => {
                if let Some(mgr) = &bundle.opencode {
                    stubs_opencode(mgr, &mut out);
                }
            }
            ArtifactType::Cursor => {
                if let Some(mgr) = &bundle.cursor {
                    stubs_cursor(mgr, &mut out);
                }
            }
            ArtifactType::Pi => {
                if let Some(mgr) = &bundle.pi {
                    stubs_pi(mgr, parent_dir, &mut out);
                }
            }
            ArtifactType::Copilot => {
                if let Some(mgr) = &bundle.copilot {
                    stubs_copilot(mgr, &mut out);
                }
            }
            // Recorded via `p import`, never discovered: there is no
            // machine-wide registry of repos to walk.
            ArtifactType::Git => {}
        }
        out
    }

    /// Chain heads via `list_conversations` (bounded first-lines peek
    /// per file, no full parse). The head is the chain's *oldest*
    /// segment and its id is rotation-stable; the fingerprint covers
    /// the whole chain (see `claude_chain_stamp`) because appends land
    /// in the newest segment, not the head file.
    fn stubs_claude(
        mgr: &toolpath_claude::ClaudeConvo,
        parent_dir: Option<&Path>,
        out: &mut Vec<ArtifactStub>,
    ) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_claude(&e) => return,
            Err(e) => {
                eprintln!("warning: claude enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !claude_project_in_scope(&project, parent_dir)
            {
                continue;
            }
            let heads = match mgr.list_conversations(&project) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("warning: claude project {project} failed: {e}");
                    continue;
                }
            };
            for head in heads {
                let (modified, size) = super::claude_chain_stamp(mgr, &project, &head);
                out.push(ArtifactStub {
                    artifact_type: ArtifactType::Claude,
                    id: head,
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
    }

    /// Session entries via a bounded identity peek (`toolpath-gemini`
    /// reads at most the first 4 KiB of a main file); the fingerprint
    /// stats the main file (or the orphan sub-agent directory).
    fn stubs_gemini(
        mgr: &toolpath_gemini::GeminiConvo,
        parent_dir: Option<&Path>,
        out: &mut Vec<ArtifactStub>,
    ) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_gemini(&e) => return,
            Err(e) => {
                eprintln!("warning: gemini enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !dir_in_scope(&project, parent_dir)
            {
                continue;
            }
            let entries = match mgr.resolver().list_session_entries(&project) {
                Ok(entries) => entries,
                Err(e) => {
                    eprintln!("warning: gemini project {project} failed: {e}");
                    continue;
                }
            };
            for entry in entries {
                let (modified, size) = stat_stamp(&entry.path);
                out.push(ArtifactStub {
                    artifact_type: ArtifactType::Gemini,
                    id: entry.session_uuid.unwrap_or(entry.id),
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
    }

    /// Session-state directories, stat-only: each session is a
    /// `<id>/events.jsonl` under `session-state/` (or its legacy
    /// sibling); the directory name is the id and the events file is
    /// the fingerprint target.
    fn stubs_copilot(mgr: &toolpath_copilot::CopilotConvo, out: &mut Vec<ArtifactStub>) {
        let mut seen = std::collections::HashSet::new();
        let dirs = [
            mgr.resolver().session_state_dir(),
            mgr.resolver().legacy_session_state_dir(),
        ];
        for dir in dirs.into_iter().flatten() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(id) = entry.file_name().to_str().map(String::from) else {
                    continue;
                };
                let events = entry.path().join("events.jsonl");
                if !events.exists() || !seen.insert(id.clone()) {
                    continue;
                }
                let (modified, size) = stat_stamp(&events);
                out.push(ArtifactStub {
                    artifact_type: ArtifactType::Copilot,
                    id,
                    path: None,
                    modified,
                    size,
                });
            }
        }
    }

    /// Rollout files, stat-only. The artifact id is the trailing UUID of
    /// the filename stem (`rollout-<timestamp>-<uuid>`); `read_session`
    /// accepts either the UUID or the full stem, so the fallback is safe.
    fn stubs_codex(mgr: &toolpath_codex::CodexConvo, out: &mut Vec<ArtifactStub>) {
        let files = match mgr.io().list_rollout_files() {
            Ok(f) => f,
            Err(e) if is_not_found_codex(&e) => return,
            Err(e) => {
                eprintln!("warning: codex enumeration failed: {e}");
                return;
            }
        };
        for file in files {
            let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let id = codex_artifact_id(stem).to_string();
            let (modified, size) = stat_stamp(&file);
            out.push(ArtifactStub {
                artifact_type: ArtifactType::Codex,
                id,
                path: None,
                modified,
                size,
            });
        }
    }

    /// One header-only `SELECT` — `time_updated` is the fingerprint; no
    /// message bodies are loaded.
    fn stubs_opencode(mgr: &toolpath_opencode::OpencodeConvo, out: &mut Vec<ArtifactStub>) {
        let sessions = match mgr.io().list_sessions(None) {
            Ok(s) => s,
            Err(e) if is_not_found_opencode(&e) => return,
            Err(e) => {
                eprintln!("warning: opencode enumeration failed: {e}");
                return;
            }
        };
        for s in sessions {
            out.push(ArtifactStub {
                artifact_type: ArtifactType::Opencode,
                modified: s.last_activity(),
                path: Some(s.directory.to_string_lossy().into_owned()),
                id: s.id,
                size: None,
            });
        }
    }

    /// Composer headers (one `SELECT` plus a per-composer bubble-count
    /// check) — `lastUpdatedAt` is the fingerprint. Bubble-less drafts
    /// are skipped; unlike `share`, composers without a workspace are
    /// included, since sync doesn't need to rank them by project.
    fn stubs_cursor(mgr: &toolpath_cursor::CursorConvo, out: &mut Vec<ArtifactStub>) {
        let listings = match mgr.io().list_composers() {
            Ok(l) => l,
            Err(e) if is_not_found_cursor(&e) => return,
            Err(e) => {
                eprintln!("warning: cursor enumeration failed: {e}");
                return;
            }
        };
        for l in listings.into_iter().filter(|l| l.has_bubbles) {
            out.push(ArtifactStub {
                artifact_type: ArtifactType::Cursor,
                modified: l.head.last_updated_at_utc(),
                path: l
                    .head
                    .workspace_path()
                    .map(|p| p.to_string_lossy().into_owned()),
                id: l.head.composer_id,
                size: None,
            });
        }
    }

    /// Session files stat-only; the id comes from a one-line header
    /// peek, falling back to the filename stem's `<timestamp>_<id>`
    /// shape — the same resolution `read_session` accepts.
    fn stubs_pi(
        mgr: &toolpath_pi::PiConvo,
        parent_dir: Option<&Path>,
        out: &mut Vec<ArtifactStub>,
    ) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_pi(&e) => return,
            Err(e) => {
                eprintln!("warning: pi enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
            if let Some(parent_dir) = parent_dir
                && !pi_project_in_scope(&project, parent_dir)
            {
                continue;
            }
            let files = match toolpath_pi::reader::list_session_files(mgr.resolver(), &project) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("warning: pi project {project} failed: {e}");
                    continue;
                }
            };
            for file in files {
                let header_id = toolpath_pi::reader::peek_header(&file)
                    .ok()
                    .map(|h| h.id)
                    .filter(|id| !id.is_empty());
                let stem_id = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.split_once('_'))
                    .map(|(_, rest)| rest.to_string());
                let Some(id) = header_id.or(stem_id) else {
                    continue;
                };
                let (modified, size) = stat_stamp(&file);
                out.push(ArtifactStub {
                    artifact_type: ArtifactType::Pi,
                    id,
                    path: Some(project.clone()),
                    modified,
                    size,
                });
            }
        }
    }

    /// One stderr line per artifact type. Types the user didn't name
    /// are shown only when they had artifacts, so a default run doesn't
    /// list every uninstalled provider.
    fn render_summary(outcomes: &[(ArtifactType, SyncOutcome)], explicit: bool) -> String {
        let mut s = String::new();
        for (artifact_type, o) in outcomes {
            if o.total() == 0 && !explicit {
                continue;
            }
            s.push_str(&format!(
                "{} {} new, {} updated, {} unchanged",
                artifact_type.symbol(),
                o.new,
                o.updated,
                o.unchanged
            ));
            if o.failed > 0 {
                s.push_str(&format!(", {} failed", o.failed));
            }
            if o.out_of_scope > 0 {
                s.push_str(&format!(", {} out of scope", o.out_of_scope));
            }
            s.push('\n');
        }
        if s.is_empty() {
            s.push_str("nothing to sync\n");
        }
        s
    }

    /// Record an externally-derived cache write (`p import`, `share`) in
    /// the manifest, so sync doesn't re-derive what was just written.
    pub(crate) fn record_stub(stub: &ArtifactStub, cache_id: &str) -> Result<()> {
        update_manifest(|manifest| {
            manifest
                .entry(stub.artifact_type.name().to_string())
                .or_default()
                .insert(
                    stub.id.clone(),
                    SyncRecord {
                        path: stub.path.clone(),
                        cache_id: Some(cache_id.to_string()),
                        modified: stub.modified,
                        size: stub.size,
                        synced_at: Utc::now(),
                    },
                );
        })
    }

    /// Whether the manifest already records exactly this artifact state
    /// under exactly this cache entry, with the doc present — i.e. a
    /// write would reproduce what's already there.
    pub(crate) fn record_is_current(stub: &ArtifactStub, cache_id: &str) -> bool {
        let Ok(manifest) = load_manifest() else {
            return false;
        };
        manifest
            .get(stub.artifact_type.name())
            .and_then(|records| records.get(&stub.id))
            .is_some_and(|rec| {
                rec.cache_id.as_deref() == Some(cache_id)
                    // None stamps mean freshness is unknowable (git); only
                    // a real, matching stamp can vouch for the cache entry.
                    && (rec.modified.is_some() || rec.size.is_some())
                    && rec.modified == stub.modified
                    && rec.size == stub.size
                    && crate::cmd_cache::cache_path(cache_id).is_ok_and(|p| p.exists())
            })
    }

    /// The cache entry for an artifact, when the manifest says it is
    /// materialized and a fresh stat shows its source unchanged since —
    /// i.e. re-deriving would reproduce the cached doc byte-for-byte.
    /// Used by `share` to upload straight from the cache. The stat
    /// targets one artifact directly — no enumeration of its siblings.
    pub(crate) fn fresh_cache_id(
        bundle: &HarnessBundle,
        artifact_type: ArtifactType,
        project: Option<&str>,
        id: &str,
    ) -> Option<String> {
        let manifest = load_manifest().ok()?;
        let rec = manifest.get(artifact_type.name())?.get(id)?;
        let cache_id = rec.cache_id.clone()?;
        let (modified, size) = stamp_one(bundle, artifact_type, project, id)?;
        // None stamps mean freshness is unknowable; only a real,
        // matching stamp can vouch for the cache entry.
        ((rec.modified.is_some() || rec.size.is_some())
            && rec.modified == modified
            && rec.size == size
            && crate::cmd_cache::cache_path(&cache_id).is_ok_and(|p| p.exists()))
        .then_some(cache_id)
    }

    /// Stat-level fingerprint for a single artifact, resolved directly.
    /// Uses the same stat targets as `enumerate_stubs` and the
    /// `derive_*_session_with` provenance stamps, so the result compares
    /// against manifest records. `project` is required for the
    /// path-keyed providers (claude/gemini/pi).
    fn stamp_one(
        bundle: &HarnessBundle,
        artifact_type: ArtifactType,
        project: Option<&str>,
        id: &str,
    ) -> Option<(Option<DateTime<Utc>>, Option<u64>)> {
        match artifact_type {
            ArtifactType::Claude => Some(super::claude_chain_stamp(
                bundle.claude.as_ref()?,
                project?,
                id,
            )),
            ArtifactType::Gemini => {
                let entries = bundle
                    .gemini
                    .as_ref()?
                    .resolver()
                    .list_session_entries(project?)
                    .ok()?;
                let entry = entries
                    .into_iter()
                    .find(|e| e.id == id || e.session_uuid.as_deref() == Some(id))?;
                Some(stat_stamp(&entry.path))
            }
            ArtifactType::Pi => {
                let mgr = bundle.pi.as_ref()?;
                let files =
                    toolpath_pi::reader::list_session_files(mgr.resolver(), project?).ok()?;
                let file = files.into_iter().find(|f| {
                    toolpath_pi::reader::peek_header(f).is_ok_and(|h| h.id == id)
                        || f.file_stem()
                            .and_then(|stem| stem.to_str())
                            .and_then(|stem| stem.split_once('_'))
                            .is_some_and(|(_, rest)| rest == id)
                })?;
                Some(stat_stamp(&file))
            }
            ArtifactType::Codex => {
                let file = bundle
                    .codex
                    .as_ref()?
                    .resolver()
                    .find_rollout_file(id)
                    .ok()?;
                Some(stat_stamp(&file))
            }
            ArtifactType::Copilot => {
                let file = bundle.copilot.as_ref()?.resolver().events_file(id).ok()?;
                Some(stat_stamp(&file))
            }
            ArtifactType::Opencode => {
                let sessions = bundle.opencode.as_ref()?.io().list_sessions(None).ok()?;
                let session = sessions.into_iter().find(|s| s.id == id)?;
                Some((session.last_activity(), None))
            }
            ArtifactType::Cursor => {
                let headers = bundle.cursor.as_ref()?.io().read_composer_headers().ok()?;
                let composer = headers
                    .all_composers
                    .into_iter()
                    .find(|c| c.composer_id == id)?;
                Some((composer.last_updated_at_utc(), None))
            }
            ArtifactType::Git => None,
        }
    }

    /// `p cache rm` eviction: the doc is gone, so any record pointing
    /// at it downgrades to known-but-uncached (the artifact itself is
    /// still real; the next in-scope sync re-materializes it).
    pub(crate) fn evict_cache_id(cache_id: &str) -> Result<()> {
        update_manifest(|manifest| {
            for records in manifest.values_mut() {
                for rec in records.values_mut() {
                    if rec.cache_id.as_deref() == Some(cache_id) {
                        rec.cache_id = None;
                    }
                }
            }
        })
    }

    // ── manifest IO ────────────────────────────────────────────────────

    fn manifest_path() -> Result<PathBuf> {
        Ok(config_dir()?.join(MANIFEST_FILE))
    }

    /// Take the exclusive advisory lock serializing manifest writers
    /// across processes (query auto-syncs and imports can run
    /// concurrently). A sibling lock file — never renamed, unlike the
    /// manifest itself — held until the returned handle drops.
    fn lock_manifest() -> Result<std::fs::File> {
        let path = manifest_path()?;
        let dir = path.parent().expect("manifest path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let lock_path = dir.join(format!("{MANIFEST_FILE}.lock"));
        let file = std::fs::File::create(&lock_path)
            .with_context(|| format!("create {}", lock_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
        }
        file.lock()
            .with_context(|| format!("lock {}", lock_path.display()))?;
        Ok(file)
    }

    /// One locked read-modify-write cycle against the manifest. Every
    /// writer goes through here, so concurrent invocations merge their
    /// records instead of clobbering each other's.
    fn update_manifest(mutate: impl FnOnce(&mut Manifest)) -> Result<()> {
        let _lock = lock_manifest()?;
        let mut manifest = load_manifest()?;
        mutate(&mut manifest);
        save_manifest(&manifest)
    }

    pub(crate) fn load_manifest() -> Result<Manifest> {
        let path = manifest_path()?;
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Manifest::default()),
            Err(e) => return Err(anyhow!("read {}: {e}", path.display())),
        };
        serde_json::from_str(&json).with_context(|| {
            format!(
                "parse {}; delete it to re-sync from scratch",
                path.display()
            )
        })
    }

    /// Write the manifest atomically (temp file + rename) with the same
    /// permissions as the rest of `$CONFIG_DIR`.
    fn save_manifest(manifest: &Manifest) -> Result<()> {
        let path = manifest_path()?;
        let dir = path.parent().expect("manifest path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let json = serde_json::to_string_pretty(manifest)?;
        let tmp = dir.join(format!("{MANIFEST_FILE}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};
        use std::path::Path;

        /// Run `f` with `$TOOLPATH_CONFIG_DIR` pinned to `<tempdir>/.toolpath`;
        /// `f` receives the tempdir root for building provider fixtures.
        fn with_cfg<F: FnOnce(&Path) -> R, R>(f: F) -> R {
            let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let temp = tempfile::tempdir().unwrap();
            let prev = std::env::var_os(CONFIG_DIR_ENV);
            unsafe {
                std::env::set_var(CONFIG_DIR_ENV, temp.path().join(".toolpath"));
            }
            let result = f(temp.path());
            unsafe {
                match prev {
                    Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
                    None => std::env::remove_var(CONFIG_DIR_ENV),
                }
            }
            result
        }

        fn write_claude_session(home: &Path, project_slug: &str, session: &str, prompt: &str) {
            let project_dir = home.join(".claude/projects").join(project_slug);
            std::fs::create_dir_all(&project_dir).unwrap();
            let user = format!(
                r#"{{"type":"user","uuid":"u-{session}","timestamp":"2024-01-02T00:00:00Z","cwd":"/test/project","message":{{"role":"user","content":"{prompt}"}}}}"#
            );
            let asst = format!(
                r#"{{"type":"assistant","uuid":"a-{session}","timestamp":"2024-01-02T00:00:01Z","message":{{"role":"assistant","content":"hi"}}}}"#
            );
            std::fs::write(
                project_dir.join(format!("{session}.jsonl")),
                format!("{user}\n{asst}\n"),
            )
            .unwrap();
        }

        fn claude_bundle(home: &Path) -> HarnessBundle {
            let resolver =
                toolpath_claude::PathResolver::new().with_claude_dir(home.join(".claude"));
            HarnessBundle {
                claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
                ..Default::default()
            }
        }

        fn cached_step_count(cache_id: &str) -> usize {
            let path = crate::cmd_cache::cache_path(cache_id).unwrap();
            let json = std::fs::read_to_string(path).unwrap();
            let doc = toolpath::v1::Graph::from_json(&json).unwrap();
            doc.single_path().map(|p| p.steps.len()).unwrap_or(0)
        }

        fn make_stub(artifact_type: ArtifactType, id: &str) -> ArtifactStub {
            ArtifactStub {
                artifact_type,
                id: id.to_string(),
                path: Some("/test/project".to_string()),
                modified: None,
                size: None,
            }
        }

        #[test]
        fn manifest_roundtrips_and_missing_is_empty() {
            with_cfg(|_| {
                assert!(load_manifest().unwrap().is_empty());

                let mut manifest = Manifest::default();
                manifest.entry("claude".to_string()).or_default().insert(
                    "sess-1".to_string(),
                    SyncRecord {
                        path: Some("/test/project".to_string()),
                        cache_id: Some("claude-p1".to_string()),
                        modified: Some("2024-01-02T00:00:01.123456789Z".parse().unwrap()),
                        size: Some(4096),
                        synced_at: "2026-07-09T00:00:00Z".parse().unwrap(),
                    },
                );
                save_manifest(&manifest).unwrap();
                assert_eq!(load_manifest().unwrap(), manifest);
            });
        }

        #[cfg(unix)]
        #[test]
        fn manifest_file_is_0600() {
            use std::os::unix::fs::PermissionsExt;
            with_cfg(|_| {
                save_manifest(&Manifest::default()).unwrap();
                let mode = std::fs::metadata(manifest_path().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600);
            });
        }

        #[test]
        fn corrupt_manifest_errors_with_hint() {
            with_cfg(|_| {
                save_manifest(&Manifest::default()).unwrap();
                std::fs::write(manifest_path().unwrap(), "not json").unwrap();
                let err = load_manifest().unwrap_err();
                assert!(err.to_string().contains("re-sync from scratch"));
            });
        }

        #[test]
        fn enumerate_stubs_stats_claude_sessions() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                let stubs = enumerate_stubs(&bundle, ArtifactType::Claude, None);
                assert_eq!(stubs.len(), 1);
                assert_eq!(stubs[0].id, "sess-aaa");
                assert_eq!(stubs[0].path.as_deref(), Some("/test/project"));
                assert!(stubs[0].modified.is_some(), "file mtime must be stamped");
                assert!(stubs[0].size.unwrap() > 0, "file size must be stamped");
            });
        }

        #[test]
        fn first_sync_ingests_then_second_is_unchanged() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                write_claude_session(home, "-test-project", "sess-bbb", "Fix a bug");
                let bundle = claude_bundle(home);

                let outcomes = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                assert_eq!(outcomes.len(), 1);
                let (_, first) = outcomes[0];
                assert_eq!(
                    (first.new, first.updated, first.unchanged, first.failed),
                    (2, 0, 0, 0)
                );

                let manifest = load_manifest().unwrap();
                let records = manifest.get("claude").unwrap();
                assert_eq!(records.len(), 2);
                let rec = records.get("sess-aaa").unwrap();
                assert_eq!(rec.path.as_deref(), Some("/test/project"));
                assert!(rec.modified.is_some());
                assert!(rec.size.is_some());
                let cache_id = rec
                    .cache_id
                    .as_deref()
                    .expect("synced record is materialized");
                assert!(
                    crate::cmd_cache::cache_path(cache_id).unwrap().exists(),
                    "cache doc must exist for {cache_id}"
                );

                let (_, second) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!(
                    (second.new, second.updated, second.unchanged, second.failed),
                    (0, 0, 2, 0)
                );
            });
        }

        #[test]
        fn changed_session_is_rederived() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();

                let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .clone()
                    .expect("synced record is materialized");
                let steps_before = cached_step_count(&cache_id);

                // Session continues: a later user turn lands in the file,
                // changing its size (and mtime).
                let file = home.join(".claude/projects/-test-project/sess-aaa.jsonl");
                let mut body = std::fs::read_to_string(&file).unwrap();
                body.push_str(
                    r#"{"type":"user","uuid":"u-2","timestamp":"2024-01-02T00:05:00Z","cwd":"/test/project","message":{"role":"user","content":"And another thing"}}"#,
                );
                body.push('\n');
                std::fs::write(&file, body).unwrap();

                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!(
                    (
                        outcome.new,
                        outcome.updated,
                        outcome.unchanged,
                        outcome.failed
                    ),
                    (0, 1, 0, 0)
                );
                assert!(
                    cached_step_count(&cache_id) > steps_before,
                    "re-derived doc must contain the appended turn"
                );
            });
        }

        #[test]
        fn sync_touches_only_requested_types() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);

                let outcomes = sync_bundle(&bundle, &[ArtifactType::Codex], None).unwrap();
                assert_eq!(outcomes[0].1, SyncOutcome::default());
                assert!(
                    load_manifest().unwrap().is_empty(),
                    "codex-only sync must not ingest claude sessions"
                );
            });
        }

        #[test]
        fn sync_overwrites_cache_entry_it_does_not_remember() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();

                // Losing the manifest (or a prior manual `p import`) leaves a
                // cache entry sync doesn't know about; re-syncing must
                // overwrite it, not die on the exists-check.
                std::fs::remove_file(manifest_path().unwrap()).unwrap();
                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!((outcome.new, outcome.failed), (1, 0));
            });
        }

        #[test]
        fn failed_derivation_is_tallied_and_skipped() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                let mut stubs = enumerate_stubs(&bundle, ArtifactType::Claude, None);
                stubs.push(make_stub(ArtifactType::Claude, "does-not-exist"));

                let outcome = sync_stubs(&bundle, &stubs, &BTreeMap::new(), None).unwrap();
                assert_eq!((outcome.new, outcome.failed), (1, 1));
                let records = &load_manifest().unwrap()["claude"];
                assert!(records.contains_key("sess-aaa"));
                assert!(
                    !records.contains_key("does-not-exist"),
                    "failed artifacts must not be recorded as synced"
                );
            });
        }

        #[test]
        fn rotated_session_resyncs_under_its_head_id() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .clone()
                    .unwrap();
                let steps_before = cached_step_count(&cache_id);

                // The session rotates: a successor file whose first entry
                // carries the predecessor's sessionId (the bridge).
                // Appends land here; sess-aaa.jsonl never changes again.
                std::fs::write(
                    home.join(".claude/projects/-test-project/sess-bbb.jsonl"),
                    concat!(
                        r#"{"type":"user","uuid":"u-b0","timestamp":"2024-01-02T01:00:00Z","sessionId":"sess-aaa","cwd":"/test/project","message":{"role":"user","content":"bridge"}}"#,
                        "\n",
                        r#"{"type":"user","uuid":"u-b1","timestamp":"2024-01-02T01:00:01Z","sessionId":"sess-bbb","cwd":"/test/project","message":{"role":"user","content":"after rotation"}}"#,
                        "\n",
                    ),
                )
                .unwrap();

                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!(
                    (outcome.new, outcome.updated, outcome.unchanged),
                    (0, 1, 0),
                    "the chain must re-sync under its head id, not read as unchanged"
                );
                let manifest = load_manifest().unwrap();
                assert!(
                    !manifest["claude"].contains_key("sess-bbb"),
                    "successor segments are not separate artifacts"
                );
                assert!(
                    cached_step_count(&cache_id) > steps_before,
                    "post-rotation turns must reach the cached doc"
                );

                // And the grown chain settles: a third sync is a no-op.
                let (_, again) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!((again.updated, again.unchanged), (0, 1));
            });
        }

        #[test]
        fn all_none_stamps_never_read_as_unchanged() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();

                // A record whose stamps are all None (stat failed when it
                // was written) must not match a stub whose stat also
                // failed — unknowable freshness re-derives.
                let mut records = load_manifest().unwrap()["claude"].clone();
                let rec = records.get_mut("sess-aaa").unwrap();
                rec.modified = None;
                rec.size = None;
                let stub = make_stub(ArtifactType::Claude, "sess-aaa");
                let outcome = sync_stubs(&bundle, &[stub], &records, None).unwrap();
                assert_eq!((outcome.updated, outcome.unchanged), (1, 0));
            });
        }

        #[test]
        fn recorded_import_is_unchanged_to_the_next_sync() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);

                // What `p import` does: derive with provenance, write the
                // cache, record the stub.
                let derived = crate::cmd_import::derive_claude_session_with(
                    bundle.claude.as_ref().unwrap(),
                    "/test/project",
                    "sess-aaa",
                )
                .unwrap();
                let stub = derived.provenance.as_ref().unwrap();
                assert_eq!(stub.id, "sess-aaa");
                assert!(stub.modified.is_some() && stub.size.is_some());
                crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, true).unwrap();
                record_stub(stub, &derived.cache_id).unwrap();

                // The import's stamp must match sync's own enumeration.
                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!(
                    (
                        outcome.new,
                        outcome.updated,
                        outcome.unchanged,
                        outcome.failed
                    ),
                    (0, 0, 1, 0)
                );
            });
        }

        #[test]
        fn parent_dir_scopes_path_keyed_enumeration() {
            with_cfg(|home| {
                write_claude_session(home, "-scope-alpha", "aaaa1111-x", "In alpha");
                write_claude_session(home, "-scope-beta", "bbbb2222-x", "In beta");
                let bundle = claude_bundle(home);

                let (_, scoped) = sync_bundle(
                    &bundle,
                    &[ArtifactType::Claude],
                    Some(Path::new("/scope/alpha")),
                )
                .unwrap()[0];
                assert_eq!((scoped.new, scoped.out_of_scope), (1, 0));
                let manifest = load_manifest().unwrap();
                assert!(
                    !manifest["claude"].contains_key("bbbb2222-x"),
                    "pruned projects must not be enumerated or recorded"
                );

                // Unscoped sync picks up the rest.
                let (_, full) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!((full.new, full.unchanged), (1, 1));
            });
        }

        fn codex_bundle(home: &Path, cwd: &str) -> HarnessBundle {
            let codex_dir = home.join(".codex");
            let dir = codex_dir.join("sessions/2026/05/07");
            std::fs::create_dir_all(&dir).unwrap();
            let meta = format!(
                r#"{{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{{"id":"00000000-0000-0000-0000-0000000000aa","timestamp":"2026-05-07T00:00:00Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"test","source":"cli","model_provider":"openai"}}}}"#
            );
            let user = r#"{"timestamp":"2026-05-07T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
            std::fs::write(
                dir.join("rollout-2026-05-07T00-00-00-00000000-0000-0000-0000-0000000000aa.jsonl"),
                format!("{meta}\n{user}\n"),
            )
            .unwrap();
            let resolver = toolpath_codex::PathResolver::new().with_codex_dir(&codex_dir);
            HarnessBundle {
                codex: Some(toolpath_codex::CodexConvo::with_resolver(resolver)),
                ..Default::default()
            }
        }

        #[test]
        fn out_of_scope_codex_peek_is_memoized_then_scope_match_derives() {
            with_cfg(|home| {
                let bundle = codex_bundle(home, "/work/proj");

                // cwd lives outside the constraint: one bounded peek, a
                // known-but-uncached record, no derive.
                let (_, out) = sync_bundle(
                    &bundle,
                    &[ArtifactType::Codex],
                    Some(Path::new("/elsewhere")),
                )
                .unwrap()[0];
                assert_eq!((out.new, out.out_of_scope), (0, 1));
                let rec = load_manifest().unwrap()["codex"]["00000000-0000-0000-0000-0000000000aa"]
                    .clone();
                assert_eq!(
                    rec.path.as_deref(),
                    Some("/work/proj"),
                    "peeked cwd memoized"
                );
                assert!(rec.cache_id.is_none(), "known, not materialized");

                // Matching constraint: the memoized record answers the scope
                // question and the artifact derives.
                let (_, hit) = sync_bundle(
                    &bundle,
                    &[ArtifactType::Codex],
                    Some(Path::new("/work/proj")),
                )
                .unwrap()[0];
                assert_eq!((hit.new, hit.updated, hit.out_of_scope), (0, 1, 0));
                let rec = load_manifest().unwrap()["codex"]["00000000-0000-0000-0000-0000000000aa"]
                    .clone();
                assert!(rec.cache_id.is_some(), "materialized now");
                assert_eq!(
                    rec.path.as_deref(),
                    Some("/work/proj"),
                    "deriving must not clobber the memoized peeked cwd"
                );
            });
        }

        fn copilot_bundle(home: &Path, id: &str, cwd: &str) -> HarnessBundle {
            let copilot_dir = home.join(".copilot");
            let dir = copilot_dir.join("session-state").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let start = format!(
                r#"{{"type":"session.start","timestamp":"2026-07-01T00:00:00Z","data":{{"copilotVersion":"1.0.67","context":{{"cwd":"{cwd}"}}}}}}"#
            );
            let user = r#"{"type":"user.message","timestamp":"2026-07-01T00:00:01Z","data":{"content":"hi"}}"#;
            std::fs::write(dir.join("events.jsonl"), format!("{start}\n{user}\n")).unwrap();
            let resolver = toolpath_copilot::PathResolver::new().with_copilot_dir(&copilot_dir);
            HarnessBundle {
                copilot: Some(toolpath_copilot::CopilotConvo::with_resolver(resolver)),
                ..Default::default()
            }
        }

        #[test]
        fn copilot_syncs_and_scopes_via_memoized_peek() {
            with_cfg(|home| {
                let bundle = copilot_bundle(home, "sess-cp", "/work/proj");

                // Out-of-scope first: one peek, a known record with the cwd.
                let (_, out) = sync_bundle(
                    &bundle,
                    &[ArtifactType::Copilot],
                    Some(Path::new("/elsewhere")),
                )
                .unwrap()[0];
                assert_eq!((out.new, out.out_of_scope), (0, 1));
                let rec = load_manifest().unwrap()["copilot"]["sess-cp"].clone();
                assert_eq!(rec.path.as_deref(), Some("/work/proj"));
                assert!(rec.cache_id.is_none());

                // In scope: derives; then a plain re-sync is a no-op.
                let (_, hit) =
                    sync_bundle(&bundle, &[ArtifactType::Copilot], Some(Path::new("/work")))
                        .unwrap()[0];
                assert_eq!((hit.updated, hit.out_of_scope), (1, 0));
                let (_, again) = sync_bundle(&bundle, &[ArtifactType::Copilot], None).unwrap()[0];
                assert_eq!(again.unchanged, 1);
            });
        }

        #[test]
        fn evicted_cache_entry_rematerializes_on_next_sync() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .clone()
                    .unwrap();

                // `p cache rm`: doc removed, record downgraded to known.
                crate::cmd_cache::remove_cached(&cache_id).unwrap();
                evict_cache_id(&cache_id).unwrap();
                assert!(
                    load_manifest().unwrap()["claude"]["sess-aaa"]
                        .cache_id
                        .is_none()
                );

                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!((outcome.new, outcome.updated), (0, 1));
                assert!(
                    crate::cmd_cache::cache_path(&cache_id).unwrap().exists(),
                    "evicted artifact re-materializes"
                );
            });
        }

        #[test]
        fn manually_deleted_doc_is_restored_even_with_stale_record() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .clone()
                    .unwrap();

                // Doc deleted behind the CLI's back: the record still claims
                // materialization, but sync verifies the doc exists.
                let doc = crate::cmd_cache::cache_path(&cache_id).unwrap();
                std::fs::remove_file(&doc).unwrap();
                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap()[0];
                assert_eq!((outcome.new, outcome.updated), (0, 1));
                assert!(doc.exists());
            });
        }

        #[test]
        fn fresh_cache_id_tracks_source_and_eviction() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);

                // Nothing synced yet: no fresh copy.
                assert!(
                    fresh_cache_id(
                        &bundle,
                        ArtifactType::Claude,
                        Some("/test/project"),
                        "sess-aaa"
                    )
                    .is_none()
                );

                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                let cache_id = fresh_cache_id(
                    &bundle,
                    ArtifactType::Claude,
                    Some("/test/project"),
                    "sess-aaa",
                )
                .expect("synced artifact is fresh");

                // Source grows: stale until re-synced.
                let file = home.join(".claude/projects/-test-project/sess-aaa.jsonl");
                let mut body = std::fs::read_to_string(&file).unwrap();
                body.push_str(
                    r#"{"type":"user","uuid":"u-2","timestamp":"2024-01-02T00:05:00Z","cwd":"/test/project","message":{"role":"user","content":"more"}}"#,
                );
                body.push('\n');
                std::fs::write(&file, body).unwrap();
                assert!(
                    fresh_cache_id(
                        &bundle,
                        ArtifactType::Claude,
                        Some("/test/project"),
                        "sess-aaa"
                    )
                    .is_none()
                );
                sync_bundle(&bundle, &[ArtifactType::Claude], None).unwrap();
                assert!(
                    fresh_cache_id(
                        &bundle,
                        ArtifactType::Claude,
                        Some("/test/project"),
                        "sess-aaa"
                    )
                    .is_some()
                );

                // Evicted: known but not materialized, so not fresh.
                crate::cmd_cache::remove_cached(&cache_id).unwrap();
                evict_cache_id(&cache_id).unwrap();
                assert!(
                    fresh_cache_id(
                        &bundle,
                        ArtifactType::Claude,
                        Some("/test/project"),
                        "sess-aaa"
                    )
                    .is_none()
                );
            });
        }

        #[test]
        fn pi_scope_matching_survives_hyphenated_paths() {
            // decode_project turns the dir-encoded '-' back into '/', so a
            // real path /Users/b/my-repo arrives as /Users/b/my/repo; the
            // comparator must match it against the real constraint anyway.
            assert!(pi_project_in_scope(
                "/Users/b/my/repo",
                Path::new("/Users/b/my-repo")
            ));
            assert!(pi_project_in_scope(
                "/Users/b/my/repo/sub",
                Path::new("/Users/b/my-repo")
            ));
            assert!(!pi_project_in_scope(
                "/Users/b/other",
                Path::new("/Users/b/my-repo")
            ));
        }

        #[test]
        fn copilot_peek_accepts_top_level_cwd() {
            with_cfg(|home| {
                // Older CLIs store cwd at the payload top level, no
                // `context` object — the peek must still find it.
                let copilot_dir = home.join(".copilot");
                let dir = copilot_dir.join("session-state").join("sess-legacy");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("events.jsonl"),
                    concat!(
                        r#"{"type":"session.start","data":{"cwd":"/work/proj"}}"#,
                        "\n",
                        r#"{"type":"user.message","data":{"content":"hi"}}"#,
                        "\n"
                    ),
                )
                .unwrap();
                let resolver = toolpath_copilot::PathResolver::new().with_copilot_dir(&copilot_dir);
                let bundle = HarnessBundle {
                    copilot: Some(toolpath_copilot::CopilotConvo::with_resolver(resolver)),
                    ..Default::default()
                };
                let (_, out) = sync_bundle(
                    &bundle,
                    &[ArtifactType::Copilot],
                    Some(Path::new("/elsewhere")),
                )
                .unwrap()[0];
                assert_eq!(out.out_of_scope, 1);
                let rec = load_manifest().unwrap()["copilot"]["sess-legacy"].clone();
                assert_eq!(rec.path.as_deref(), Some("/work/proj"));
            });
        }

        #[test]
        fn derive_stub_errors_when_provider_missing() {
            let bundle = HarnessBundle::default();
            let stub = make_stub(ArtifactType::Claude, "sess");
            let Err(err) = derive_stub(&bundle, &stub) else {
                panic!("derive_stub must fail without a claude manager");
            };
            assert!(err.to_string().contains("provider not available"));
        }

        #[test]
        fn newest_first_orders_by_mtime_with_unstamped_last() {
            let mut old = make_stub(ArtifactType::Claude, "old");
            old.modified = Some("2026-01-01T00:00:00Z".parse().unwrap());
            let mut new = make_stub(ArtifactType::Claude, "new");
            new.modified = Some("2026-07-01T00:00:00Z".parse().unwrap());
            let unstamped = make_stub(ArtifactType::Claude, "unstamped");

            let stubs = vec![old, unstamped, new];
            let ids: Vec<&str> = newest_first(&stubs).iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, vec!["new", "old", "unstamped"]);
        }

        #[test]
        fn progress_line_counts_only_pending_work() {
            let mut progress = Progress {
                label: "claude  ",
                total: 3,
                done: 0,
                tty: false,
            };
            progress.tick();
            progress.tick();
            assert_eq!(progress.line(), "claude   2/3");
        }

        #[test]
        fn resolve_types_defaults_to_all_and_dedups() {
            assert_eq!(resolve_types(&[]), ArtifactType::ALL.to_vec());
            assert_eq!(
                resolve_types(&[
                    ArtifactType::Codex,
                    ArtifactType::Claude,
                    ArtifactType::Codex
                ]),
                vec![ArtifactType::Codex, ArtifactType::Claude]
            );
        }

        #[test]
        fn render_summary_hides_empty_types_unless_explicit() {
            let outcomes = vec![
                (
                    ArtifactType::Claude,
                    SyncOutcome {
                        new: 2,
                        updated: 1,
                        unchanged: 3,
                        failed: 0,
                        out_of_scope: 0,
                    },
                ),
                (ArtifactType::Cursor, SyncOutcome::default()),
            ];
            let default_run = render_summary(&outcomes, false);
            assert!(default_run.contains("claude"));
            assert!(!default_run.contains("cursor"));

            let explicit_run = render_summary(&outcomes, true);
            assert!(explicit_run.contains("cursor"));
        }

        #[test]
        fn render_summary_shows_failures_and_empty_case() {
            let outcomes = vec![(
                ArtifactType::Codex,
                SyncOutcome {
                    new: 0,
                    updated: 0,
                    unchanged: 1,
                    failed: 2,
                    out_of_scope: 0,
                },
            )];
            let s = render_summary(&outcomes, false);
            assert!(s.contains("2 failed"));

            assert_eq!(render_summary(&[], false), "nothing to sync\n");
        }
    }
}

#[cfg(test)]
mod type_tests {
    use super::ArtifactType;

    #[test]
    fn names_and_symbols_are_distinct() {
        let names: std::collections::HashSet<&str> =
            ArtifactType::ALL.iter().map(|t| t.name()).collect();
        let symbols: std::collections::HashSet<&str> =
            ArtifactType::ALL.iter().map(|t| t.symbol()).collect();
        assert_eq!(names.len(), ArtifactType::ALL.len());
        assert_eq!(symbols.len(), ArtifactType::ALL.len());
    }

    #[test]
    fn path_keyed_matches_design() {
        assert!(ArtifactType::Claude.path_keyed());
        assert!(ArtifactType::Gemini.path_keyed());
        assert!(ArtifactType::Pi.path_keyed());
        assert!(!ArtifactType::Codex.path_keyed());
        assert!(!ArtifactType::Opencode.path_keyed());
        assert!(!ArtifactType::Cursor.path_keyed());
        assert!(ArtifactType::Git.path_keyed());
    }

    #[test]
    fn parse_roundtrips_every_name() {
        for t in ArtifactType::ALL {
            assert_eq!(ArtifactType::parse(t.name()), Some(t));
        }
        assert_eq!(ArtifactType::parse("frobnicate"), None);
    }
}
