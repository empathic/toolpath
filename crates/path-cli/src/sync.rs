//! `path p cache sync` — incremental ingestion of artifacts into the
//! document cache — and [`ArtifactType`], the single enum naming the
//! artifact sources the CLI operates over.
//!
//! Sync enumerates artifacts across the requested types (today all six
//! are agent-session providers, using the same aggregation `path share`
//! uses), compares each against the sync manifest at
//! `$CONFIG_DIR/sync.json`, and derives + caches only what is new or
//! changed. The manifest maps artifact type → artifact id → the
//! `last_activity` fingerprint recorded at last sync, so an unchanged
//! artifact costs a metadata read instead of a re-derivation, and
//! running sync twice in a row is a no-op. Artifacts deleted upstream
//! keep both their cache document and their manifest record — the
//! cache is an archive, not a mirror.

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
    use std::path::PathBuf;

    use super::ArtifactType;
    use crate::cmd_cache::write_cached;
    use crate::cmd_import::DerivedDoc;
    use crate::cmd_share::{ArtifactRow, HarnessBundle, gather_artifacts};
    use crate::config::config_dir;

    const MANIFEST_FILE: &str = "sync.json";

    /// What the manifest remembers about one synced artifact.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct SyncRecord {
        /// Filesystem path the artifact is keyed under, for path-keyed
        /// providers (claude/gemini/pi: the project directory).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) path: Option<String>,
        /// Cache entry the derived document was written to.
        pub(crate) cache_id: String,
        /// Fingerprint: the artifact's last activity as reported at
        /// sync time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) last_activity: Option<DateTime<Utc>>,
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
        // Only ranking depends on cwd and sync ignores row order, so any
        // directory works here.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut out = Vec::with_capacity(types.len());
        for &artifact_type in types {
            let rows = gather_artifacts(bundle, &cwd, Some(artifact_type), None);
            let mut records = manifest
                .get(artifact_type.name())
                .cloned()
                .unwrap_or_default();
            let outcome = sync_rows(bundle, &rows, &mut records)?;
            if !records.is_empty() {
                manifest.insert(artifact_type.name().to_string(), records);
                save_manifest(&manifest)?;
            }
            out.push((artifact_type, outcome));
        }
        Ok(out)
    }

    /// Sync one type's rows against its manifest records. Derivation
    /// failures are warned and tallied, not fatal; cache-write failures
    /// (disk, permissions) abort.
    fn sync_rows(
        bundle: &HarnessBundle,
        rows: &[ArtifactRow],
        records: &mut BTreeMap<String, SyncRecord>,
    ) -> Result<SyncOutcome> {
        let mut outcome = SyncOutcome::default();
        for row in rows {
            let existing = records.get(&row.session_id);
            let is_new = existing.is_none();
            if let Some(rec) = existing
                && rec.last_activity == row.last_activity
            {
                outcome.unchanged += 1;
                continue;
            }
            match derive_row(bundle, row) {
                Ok(derived) => {
                    // force: sync owns refresh semantics — a re-sync or a
                    // prior manual `p import` of the same session must not
                    // error on the existing cache entry.
                    write_cached(&derived.cache_id, &derived.doc, true)?;
                    records.insert(
                        row.session_id.clone(),
                        SyncRecord {
                            path: row.path.clone(),
                            cache_id: derived.cache_id,
                            last_activity: row.last_activity,
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
                        row.artifact_type.name(),
                        row.session_id
                    );
                    outcome.failed += 1;
                }
            }
        }
        Ok(outcome)
    }

    /// Derive one artifact through the same manager the row was
    /// enumerated from, so listing and derivation always agree on
    /// provider roots.
    fn derive_row(bundle: &HarnessBundle, row: &ArtifactRow) -> Result<DerivedDoc> {
        use crate::cmd_import as imp;
        let path = || {
            row.path
                .as_deref()
                .ok_or_else(|| anyhow!("artifact {} has no path", row.session_id))
        };
        match row.artifact_type {
            ArtifactType::Claude => {
                imp::derive_claude_session_with(mgr(&bundle.claude)?, path()?, &row.session_id)
            }
            ArtifactType::Gemini => imp::derive_gemini_session_with(
                mgr(&bundle.gemini)?,
                path()?,
                &row.session_id,
                false,
            ),
            ArtifactType::Pi => {
                imp::derive_pi_session_with(mgr(&bundle.pi)?, path()?, &row.session_id)
            }
            ArtifactType::Codex => {
                imp::derive_codex_session_with(mgr(&bundle.codex)?, &row.session_id)
            }
            ArtifactType::Opencode => {
                imp::derive_opencode_session_with(mgr(&bundle.opencode)?, &row.session_id, false)
            }
            ArtifactType::Cursor => {
                imp::derive_cursor_session_with(mgr(&bundle.cursor)?, &row.session_id)
            }
        }
    }

    fn mgr<T>(slot: &Option<T>) -> Result<&T> {
        slot.as_ref()
            .ok_or_else(|| anyhow!("provider not available"))
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

        fn make_row(artifact_type: ArtifactType, session_id: &str) -> ArtifactRow {
            ArtifactRow {
                artifact_type,
                path: Some("/test/project".to_string()),
                cwd: None,
                session_id: session_id.to_string(),
                title: String::new(),
                last_activity: None,
                matches_cwd: false,
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
                        last_activity: Some("2024-01-02T00:00:01Z".parse().unwrap()),
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
                assert!(rec.last_activity.is_some());
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

                // Session continues: a later user turn lands in the file.
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
                let cwd = std::env::current_dir().unwrap();
                let mut rows = gather_artifacts(&bundle, &cwd, Some(ArtifactType::Claude), None);
                rows.push(make_row(ArtifactType::Claude, "does-not-exist"));

                let mut records = BTreeMap::new();
                let outcome = sync_rows(&bundle, &rows, &mut records).unwrap();
                assert_eq!((outcome.new, outcome.failed), (1, 1));
                assert!(records.contains_key("sess-aaa"));
                assert!(
                    !records.contains_key("does-not-exist"),
                    "failed artifacts must not be recorded as synced"
                );
            });
        }

        #[test]
        fn derive_row_errors_when_provider_missing() {
            let bundle = HarnessBundle::default();
            let row = make_row(ArtifactType::Claude, "sess");
            let Err(err) = derive_row(&bundle, &row) else {
                panic!("derive_row must fail without a claude manager");
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
