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

    /// True when the parent_dirlying provider keys artifacts by a filesystem
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
        match s {
            "claude" => Some(ArtifactType::Claude),
            "gemini" => Some(ArtifactType::Gemini),
            "codex" => Some(ArtifactType::Codex),
            "opencode" => Some(ArtifactType::Opencode),
            "cursor" => Some(ArtifactType::Cursor),
            "pi" => Some(ArtifactType::Pi),
            "copilot" => Some(ArtifactType::Copilot),
            "git" => Some(ArtifactType::Git),
            _ => None,
        }
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
    /// Filesystem path the artifact is keyed parent_dir, for path-keyed
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
        /// Filesystem path the artifact is keyed parent_dir: the project
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

    /// Sync the given artifact types from `bundle` into the cache. The
    /// manifest is checkpointed after each type so an interrupted first
    /// run doesn't forget the types it already finished.
    pub(crate) fn sync_bundle(
        bundle: &HarnessBundle,
        types: &[ArtifactType],
        parent_dir: Option<&Path>,
    ) -> Result<Vec<(ArtifactType, SyncOutcome)>> {
        let mut manifest = load_manifest()?;
        let mut out = Vec::with_capacity(types.len());
        for &artifact_type in types {
            let stubs = enumerate_stubs(bundle, artifact_type, parent_dir);
            let mut records = manifest
                .get(artifact_type.name())
                .cloned()
                .unwrap_or_default();
            let outcome = sync_stubs(bundle, &stubs, &mut records, parent_dir)?;
            if !records.is_empty() {
                manifest.insert(artifact_type.name().to_string(), records);
                save_manifest(&manifest)?;
            }
            out.push((artifact_type, outcome));
        }
        Ok(out)
    }

    /// Sync one type's stubs against its manifest records. Derivation
    /// failures are warned and tallied, not fatal; cache-write failures
    /// (disk, permissions) abort.
    fn sync_stubs(
        bundle: &HarnessBundle,
        stubs: &[ArtifactStub],
        records: &mut BTreeMap<String, SyncRecord>,
        parent_dir: Option<&Path>,
    ) -> Result<SyncOutcome> {
        let mut outcome = SyncOutcome::default();
        for stub in stubs {
            let existing = records.get(&stub.id);
            let is_new = existing.is_none();
            // Stat gate first, always: a materialized, unchanged artifact
            // needs nothing — no read, no scope check.
            if let Some(rec) = existing
                && rec.modified == stub.modified
                && rec.size == stub.size
                && let Some(cache_id) = &rec.cache_id
                && crate::cmd_cache::cache_path(cache_id).is_ok_and(|p| p.exists())
            {
                outcome.unchanged += 1;
                continue;
            }
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
                    // Claude paths came from lossy dir slugs; compare in
                    // slug space like the enumeration pruning does.
                    ArtifactType::Claude => claude_project_in_scope(d, parent_dir),
                    _ => dir_in_scope(d, parent_dir),
                });
                if !in_scope {
                    outcome.out_of_scope += 1;
                    // Remember what we learned — but never touch the stamp
                    // of a materialized record, or its staleness would be
                    // masked from the next in-scope sync.
                    if existing.is_none_or(|r| r.cache_id.is_none()) {
                        records.insert(
                            stub.id.clone(),
                            SyncRecord {
                                path: dir,
                                cache_id: None,
                                modified: stub.modified,
                                size: stub.size,
                                synced_at: Utc::now(),
                            },
                        );
                    }
                    continue;
                }
            }
            match derive_stub(bundle, stub) {
                Ok(derived) => {
                    // force: sync owns refresh semantics — a re-sync or a
                    // prior manual `p import` of the same session must not
                    // error on the existing cache entry.
                    write_cached(&derived.cache_id, &derived.doc, true)?;
                    records.insert(
                        stub.id.clone(),
                        SyncRecord {
                            path: stub.path.clone(),
                            cache_id: Some(derived.cache_id),
                            // The stamp was taken before the derive read the
                            // source, so a write racing the derive re-syncs
                            // next run instead of going unnoticed.
                            modified: stub.modified,
                            size: stub.size,
                            synced_at: Utc::now(),
                        },
                    );
                    if is_new {
                        outcome.new += 1;
                    } else {
                        outcome.updated += 1;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "warning: sync {}: {}: {e}",
                        stub.artifact_type.name(),
                        stub.id
                    );
                    outcome.failed += 1;
                }
            }
        }
        Ok(outcome)
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
                imp::derive_gemini_session_with(mgr(&bundle.gemini)?, path()?, &stub.id, false)
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
        let mut line = String::new();
        BufReader::new(f).read_line(&mut line).ok()?;
        let v: serde_json::Value = serde_json::from_str(&line).ok()?;
        match stub.artifact_type {
            // Codex: `session_meta` payload carries cwd directly.
            ArtifactType::Codex => Some(v.get("payload")?.get("cwd")?.as_str()?.to_string()),
            // Copilot: `session.start` carries it under `context`, with
            // some key-name variance across CLI versions.
            ArtifactType::Copilot => ["data", "payload"].iter().find_map(|env| {
                let ctx = v.get(env)?.get("context")?;
                ["cwd", "workingDirectory", "working_dir"]
                    .iter()
                    .find_map(|k| Some(ctx.get(k)?.as_str()?.to_string()))
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
    /// per file, no full parse); fingerprint stats the head segment —
    /// appends land there, and a rotation surfaces as a new head id.
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
                let (modified, size) = mgr
                    .resolver()
                    .conversation_file(&project, &head)
                    .map(|p| stat_stamp(&p))
                    .unwrap_or((None, None));
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
                && !dir_in_scope(&project, parent_dir)
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
        let mut manifest = load_manifest()?;
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
        save_manifest(&manifest)
    }

    /// `p cache rm` eviction: the doc is gone, so any record pointing
    /// at it downgrades to known-but-uncached (the artifact itself is
    /// still real; the next in-scope sync re-materializes it).
    pub(crate) fn evict_cache_id(cache_id: &str) -> Result<()> {
        let mut manifest = load_manifest()?;
        let mut changed = false;
        for records in manifest.values_mut() {
            for rec in records.values_mut() {
                if rec.cache_id.as_deref() == Some(cache_id) {
                    rec.cache_id = None;
                    changed = true;
                }
            }
        }
        if changed {
            save_manifest(&manifest)
        } else {
            Ok(())
        }
    }

    // ── manifest IO ────────────────────────────────────────────────────

    fn manifest_path() -> Result<PathBuf> {
        Ok(config_dir()?.join(MANIFEST_FILE))
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

                let mut records = BTreeMap::new();
                let outcome = sync_stubs(&bundle, &stubs, &mut records, None).unwrap();
                assert_eq!((outcome.new, outcome.failed), (1, 1));
                assert!(records.contains_key("sess-aaa"));
                assert!(
                    !records.contains_key("does-not-exist"),
                    "failed artifacts must not be recorded as synced"
                );
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
        fn derive_stub_errors_when_provider_missing() {
            let bundle = HarnessBundle::default();
            let stub = make_stub(ArtifactType::Claude, "sess");
            let Err(err) = derive_stub(&bundle, &stub) else {
                panic!("derive_stub must fail without a claude manager");
            };
            assert!(err.to_string().contains("provider not available"));
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
