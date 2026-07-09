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
/// doubles as the manifest key and cache-id prefix. Today every
/// variant is an agent-session provider; other artifact kinds (git,
/// github, …) join here when sync learns to ingest them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ArtifactType {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Cursor,
    Pi,
}

impl ArtifactType {
    /// Every artifact type, in presentation order.
    pub(crate) const ALL: [ArtifactType; 6] = [
        ArtifactType::Claude,
        ArtifactType::Gemini,
        ArtifactType::Codex,
        ArtifactType::Opencode,
        ArtifactType::Cursor,
        ArtifactType::Pi,
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            ArtifactType::Claude => "claude",
            ArtifactType::Gemini => "gemini",
            ArtifactType::Codex => "codex",
            ArtifactType::Opencode => "opencode",
            ArtifactType::Cursor => "cursor",
            ArtifactType::Pi => "pi",
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
            ArtifactType::Claude | ArtifactType::Gemini | ArtifactType::Pi
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
            _ => None,
        }
    }
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

    use super::ArtifactType;
    use crate::cmd_cache::write_cached;
    use crate::cmd_import::DerivedDoc;
    use crate::cmd_share::{
        HarnessBundle, is_not_found_claude, is_not_found_codex, is_not_found_cursor,
        is_not_found_gemini, is_not_found_opencode, is_not_found_pi,
    };
    use crate::config::config_dir;

    const MANIFEST_FILE: &str = "sync.json";

    /// A cheaply-enumerated artifact: identity plus the stat-level
    /// fingerprint used for change detection. Producing one never
    /// parses session bodies.
    #[derive(Debug, Clone)]
    pub(crate) struct ArtifactStub {
        pub(crate) artifact_type: ArtifactType,
        pub(crate) id: String,
        /// Filesystem path the artifact is keyed under, for path-keyed
        /// providers (the project directory).
        pub(crate) path: Option<String>,
        /// Source mtime (file providers) or updated-at (DB providers).
        pub(crate) modified: Option<DateTime<Utc>>,
        /// Source file size; `None` for DB-backed providers.
        pub(crate) size: Option<u64>,
    }

    /// What the manifest remembers about one synced artifact.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct SyncRecord {
        /// Filesystem path the artifact is keyed under, for path-keyed
        /// providers (claude/gemini/pi: the project directory).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) path: Option<String>,
        /// Cache entry the derived document was written to.
        pub(crate) cache_id: String,
        /// Fingerprint: source mtime (file providers) or updated-at
        /// (DB providers) at sync time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) modified: Option<DateTime<Utc>>,
        /// Fingerprint: source file size at sync time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) size: Option<u64>,
        /// Message count as of the last derivation. Informational only
        /// — never part of change detection — and recorded only for
        /// harness artifact types (non-session kinds have no message
        /// notion).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) message_count: Option<usize>,
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
    }

    impl SyncOutcome {
        fn total(&self) -> usize {
            self.new + self.updated + self.unchanged + self.failed
        }
    }

    pub(crate) fn run(types: Vec<ArtifactType>) -> Result<()> {
        let explicit = !types.is_empty();
        let types = resolve_types(&types);
        let bundle = HarnessBundle::from_environment();
        let outcomes = sync_bundle(&bundle, &types)?;
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
    ) -> Result<Vec<(ArtifactType, SyncOutcome)>> {
        let mut manifest = load_manifest()?;
        let mut out = Vec::with_capacity(types.len());
        for &artifact_type in types {
            let stubs = enumerate_stubs(bundle, artifact_type);
            let mut records = manifest
                .get(artifact_type.name())
                .cloned()
                .unwrap_or_default();
            let outcome = sync_stubs(bundle, &stubs, &mut records)?;
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
    ) -> Result<SyncOutcome> {
        let mut outcome = SyncOutcome::default();
        for stub in stubs {
            let existing = records.get(&stub.id);
            let is_new = existing.is_none();
            if let Some(rec) = existing
                && rec.modified == stub.modified
                && rec.size == stub.size
            {
                outcome.unchanged += 1;
                continue;
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
                            cache_id: derived.cache_id,
                            // The stamp was taken before the derive read the
                            // source, so a write racing the derive re-syncs
                            // next run instead of going unnoticed.
                            modified: stub.modified,
                            size: stub.size,
                            message_count: stub.artifact_type.harness().and(derived.message_count),
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
        }
    }

    fn mgr<T>(slot: &Option<T>) -> Result<&T> {
        slot.as_ref()
            .ok_or_else(|| anyhow!("provider not available"))
    }

    // ── stat-level enumeration ─────────────────────────────────────────

    /// (mtime, size) of a file, both `None` when the stat fails.
    fn stat_stamp(path: &Path) -> (Option<DateTime<Utc>>, Option<u64>) {
        match std::fs::metadata(path) {
            Ok(md) => (
                md.modified().ok().map(DateTime::<Utc>::from),
                Some(md.len()),
            ),
            Err(_) => (None, None),
        }
    }

    /// Enumerate one type's artifacts with stat-level fingerprints.
    /// Providers that aren't installed produce no stubs; other listing
    /// errors warn and skip so one broken provider can't block the rest.
    fn enumerate_stubs(bundle: &HarnessBundle, t: ArtifactType) -> Vec<ArtifactStub> {
        let mut out = Vec::new();
        match t {
            ArtifactType::Claude => {
                if let Some(mgr) = &bundle.claude {
                    stubs_claude(mgr, &mut out);
                }
            }
            ArtifactType::Gemini => {
                if let Some(mgr) = &bundle.gemini {
                    stubs_gemini(mgr, &mut out);
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
                    stubs_pi(mgr, &mut out);
                }
            }
        }
        out
    }

    /// Chain heads via `list_conversations` (bounded first-lines peek
    /// per file, no full parse); fingerprint stats the head segment —
    /// appends land there, and a rotation surfaces as a new head id.
    fn stubs_claude(mgr: &toolpath_claude::ClaudeConvo, out: &mut Vec<ArtifactStub>) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_claude(&e) => return,
            Err(e) => {
                eprintln!("warning: claude enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
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

    /// Gemini main files carry their session id *inside* the JSON, so
    /// enumeration still goes through the metadata listing; the
    /// fingerprint is a stat of the listed file all the same. A
    /// peek-level listing in `toolpath-gemini` would drop the parse.
    fn stubs_gemini(mgr: &toolpath_gemini::GeminiConvo, out: &mut Vec<ArtifactStub>) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_gemini(&e) => return,
            Err(e) => {
                eprintln!("warning: gemini enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
            let metas = match mgr.list_conversation_metadata(&project) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: gemini project {project} failed: {e}");
                    continue;
                }
            };
            for m in metas {
                let (modified, size) = stat_stamp(&m.file_path);
                out.push(ArtifactStub {
                    artifact_type: ArtifactType::Gemini,
                    id: m.session_uuid,
                    path: Some(m.project_path),
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
            let id = stem
                .len()
                .checked_sub(36)
                .and_then(|at| stem.get(at..))
                .filter(|tail| tail.bytes().filter(|&b| b == b'-').count() == 4)
                .unwrap_or(stem)
                .to_string();
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
                id: s.id,
                path: None,
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
                id: l.head.composer_id,
                path: None,
                size: None,
            });
        }
    }

    /// Session files stat-only; the id comes from a one-line header
    /// peek, falling back to the filename stem's `<timestamp>_<id>`
    /// shape — the same resolution `read_session` accepts.
    fn stubs_pi(mgr: &toolpath_pi::PiConvo, out: &mut Vec<ArtifactStub>) {
        let projects = match mgr.list_projects() {
            Ok(ps) => ps,
            Err(e) if is_not_found_pi(&e) => return,
            Err(e) => {
                eprintln!("warning: pi enumeration failed: {e}");
                return;
            }
        };
        for project in projects {
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
            s.push('\n');
        }
        if s.is_empty() {
            s.push_str("nothing to sync\n");
        }
        s
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
                        cache_id: "claude-p1".to_string(),
                        modified: Some("2024-01-02T00:00:01.123456789Z".parse().unwrap()),
                        size: Some(4096),
                        message_count: Some(2),
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
                let stubs = enumerate_stubs(&bundle, ArtifactType::Claude);
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

                let outcomes = sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap();
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
                assert_eq!(
                    rec.message_count,
                    Some(2),
                    "harness records carry the message count from the derive"
                );
                assert!(
                    crate::cmd_cache::cache_path(&rec.cache_id)
                        .unwrap()
                        .exists(),
                    "cache doc must exist for {}",
                    rec.cache_id
                );

                let (_, second) = sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap()[0];
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
                sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap();

                let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .clone();
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

                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap()[0];
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

                let outcomes = sync_bundle(&bundle, &[ArtifactType::Codex]).unwrap();
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
                sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap();

                // Losing the manifest (or a prior manual `p import`) leaves a
                // cache entry sync doesn't know about; re-syncing must
                // overwrite it, not die on the exists-check.
                std::fs::remove_file(manifest_path().unwrap()).unwrap();
                let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude]).unwrap()[0];
                assert_eq!((outcome.new, outcome.failed), (1, 0));
            });
        }

        #[test]
        fn failed_derivation_is_tallied_and_skipped() {
            with_cfg(|home| {
                write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
                let bundle = claude_bundle(home);
                let mut stubs = enumerate_stubs(&bundle, ArtifactType::Claude);
                stubs.push(make_stub(ArtifactType::Claude, "does-not-exist"));

                let mut records = BTreeMap::new();
                let outcome = sync_stubs(&bundle, &stubs, &mut records).unwrap();
                assert_eq!((outcome.new, outcome.failed), (1, 1));
                assert!(records.contains_key("sess-aaa"));
                assert!(
                    !records.contains_key("does-not-exist"),
                    "failed artifacts must not be recorded as synced"
                );
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
    }

    #[test]
    fn parse_roundtrips_every_name() {
        for t in ArtifactType::ALL {
            assert_eq!(ArtifactType::parse(t.name()), Some(t));
        }
        assert_eq!(ArtifactType::parse("frobnicate"), None);
    }
}
