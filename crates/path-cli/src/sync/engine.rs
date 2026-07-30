//! The sync engine: the manifest at `$CONFIG_DIR/manifest.json`, the
//! stat-gated ingestion loop, and the record surfaces `p import`,
//! `share`, and `p cache rm` use to keep the manifest honest.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::sources::{self, ArtifactSource};
use crate::artifact::{ArtifactRef, ArtifactType};
use crate::cache::write_cached;
use crate::config::{MANIFEST_FILE_NAME, MANIFEST_LOCK_FILE_NAME, config_dir};
use crate::harness::HarnessBundle;

/// How many manifest writes accumulate before a mid-run checkpoint.
/// Small enough that an interrupted first sync loses at most a few
/// records (the cache docs themselves survive either way); large
/// enough that manifest serialization stays noise against the
/// derives it punctuates.
const MANIFEST_CHECKPOINT_EVERY_WRITES: usize = 10;

/// What the manifest remembers about one known artifact. A record
/// with a `cache_id` is materialized in the cache; one without is
/// merely known — evicted by `p cache rm`.
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
    /// Policy to replay after a re-derive. Rule-based only: individual
    /// finding ids cannot be replayed against content that has moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) redaction: Option<RedactionPolicy>,
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
    pub(crate) fn total(&self) -> usize {
        self.new + self.updated + self.unchanged + self.failed
    }
}

/// How the engine reports progress; rendering is the caller's
/// concern. `p cache sync` draws a live stderr line, callers that
/// sync logically with no UI pass `&mut ()`. Per artifact type:
/// one `begin` once the stat pass has sized the pending work, one
/// `failed` per derive error (before its `tick`), one `tick` per
/// finished derive, one `end` when the type's loop is done.
pub(crate) trait SyncObserver {
    fn begin(&mut self, _artifact_type: ArtifactType, _pending: usize) {}
    fn tick(&mut self) {}
    fn failed(&mut self, _artifact: &ArtifactRef, _error: &anyhow::Error) {}
    fn end(&mut self) {}
}

/// The no-UI observer: a sync that draws nothing.
impl SyncObserver for () {}

/// Sync the given artifact types from `bundle` into the cache,
/// newest artifacts first. The manifest is checkpointed every few
/// writes (see [`MANIFEST_CHECKPOINT_EVERY_WRITES`]), so an interrupted run keeps
/// nearly everything it derived. Reads come from a point-in-time
/// snapshot; each checkpoint merges only the records this run
/// wrote, under the manifest lock, so concurrent invocations
/// (query auto-syncs, imports) union their records instead of
/// clobbering each other.
pub(crate) fn sync_bundle(
    bundle: &HarnessBundle,
    types: &[ArtifactType],
    observer: &mut dyn SyncObserver,
) -> Result<Vec<(ArtifactType, SyncOutcome)>> {
    let manifest = load_manifest()?;
    let mut out = Vec::with_capacity(types.len());
    for &artifact_type in types {
        // Types with no source in this bundle — an uninstalled
        // provider, or git, which is recorded but never discovered —
        // sync as a no-op.
        let Some(source) = sources::source_for(bundle, artifact_type) else {
            out.push((artifact_type, SyncOutcome::default()));
            continue;
        };
        let artifacts = source.enumerate();
        let records = manifest
            .get(artifact_type.name())
            .cloned()
            .unwrap_or_default();
        let outcome = sync_artifacts(
            source.as_ref(),
            artifact_type,
            &artifacts,
            &records,
            observer,
        )?;
        out.push((artifact_type, outcome));
    }
    Ok(out)
}

/// The stat gate: a materialized record whose real stamps match the
/// artifact needs nothing — no read, no scope check. All-`None` stamps
/// mean freshness is unknowable; only a real stamp can vouch
/// (mirrors `record_is_current`).
fn is_unchanged(rec: Option<&SyncRecord>, artifact: &ArtifactRef) -> bool {
    rec.is_some_and(|rec| {
        (rec.modified.is_some() || rec.size.is_some())
            && rec.modified == artifact.modified
            && rec.size == artifact.size
            && rec
                .cache_id
                .as_deref()
                .is_some_and(|id| crate::cache::cache_path(id).is_ok_and(|p| p.exists()))
    })
}

/// Artifacts newest-first (unstamped last), so an interrupted run has
/// spent its time on the sessions the user most likely wants.
fn newest_first(artifacts: &[ArtifactRef]) -> Vec<&ArtifactRef> {
    let mut order: Vec<&ArtifactRef> = artifacts.iter().collect();
    order.sort_by(|a, b| b.modified.cmp(&a.modified));
    order
}

/// Merge staged records into the manifest under the lock and clear
/// the stage.
fn flush_writes(pending: &mut BTreeMap<&'static str, BTreeMap<String, SyncRecord>>) -> Result<()> {
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

/// Sync one source's artifacts against a snapshot of its manifest
/// records, newest first. Records are checkpointed to the manifest every
/// [`MANIFEST_CHECKPOINT_EVERY_WRITES`] writes (and once more at the end), so an
/// interrupted run keeps nearly everything it derived. Derivation
/// failures are warned and tallied, not fatal; cache-write failures
/// (disk, permissions) abort.
fn sync_artifacts(
    source: &dyn ArtifactSource,
    artifact_type: ArtifactType,
    artifacts: &[ArtifactRef],
    records: &BTreeMap<String, SyncRecord>,
    observer: &mut dyn SyncObserver,
) -> Result<SyncOutcome> {
    let mut outcome = SyncOutcome::default();
    // Evaluate the stat gate once per artifact: the pass feeds both the
    // progress denominator and the loop's skip decision.
    let order: Vec<(&ArtifactRef, bool)> = newest_first(artifacts)
        .into_iter()
        .map(|artifact| (artifact, is_unchanged(records.get(&artifact.id), artifact)))
        .collect();
    let pending_total = order.iter().filter(|(_, unchanged)| !unchanged).count();
    observer.begin(artifact_type, pending_total);
    let mut writes: BTreeMap<&'static str, BTreeMap<String, SyncRecord>> = BTreeMap::new();
    let mut unflushed = 0usize;
    for (artifact, unchanged) in order {
        if unchanged {
            outcome.unchanged += 1;
            continue;
        }
        let existing = records.get(&artifact.id);
        let is_new = existing.is_none();
        let stage = |writes: &mut BTreeMap<&'static str, BTreeMap<String, SyncRecord>>,
                     record: SyncRecord| {
            writes
                .entry(artifact.artifact_type.name())
                .or_default()
                .insert(artifact.id.clone(), record);
        };
        // An artifact without a path must not erase one an earlier
        // run recorded.
        let memoized_path = artifact
            .path
            .clone()
            .or_else(|| existing.and_then(|r| r.path.clone()));
        match source.derive(artifact) {
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
                        modified: artifact.modified,
                        size: artifact.size,
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
                observer.failed(artifact, &e);
                outcome.failed += 1;
            }
        }
        observer.tick();
        if unflushed >= MANIFEST_CHECKPOINT_EVERY_WRITES {
            flush_writes(&mut writes)?;
            unflushed = 0;
        }
    }
    flush_writes(&mut writes)?;
    observer.end();
    Ok(outcome)
}

/// Record an externally-derived cache write (`p import`, `share`) in
/// the manifest, so sync doesn't re-derive what was just written.
pub(crate) fn record_artifact(artifact: &ArtifactRef, cache_id: &str) -> Result<()> {
    update_manifest(|manifest| {
        manifest
            .entry(artifact.artifact_type.name().to_string())
            .or_default()
            .insert(
                artifact.id.clone(),
                SyncRecord {
                    path: artifact.path.clone(),
                    cache_id: Some(cache_id.to_string()),
                    modified: artifact.modified,
                    size: artifact.size,
                    synced_at: Utc::now(),
                },
            );
    })
}

/// Whether the manifest already records exactly this artifact state
/// under exactly this cache entry, with the doc present — i.e. a
/// write would reproduce what's already there.
pub(crate) fn record_is_current(artifact: &ArtifactRef, cache_id: &str) -> bool {
    let Ok(manifest) = load_manifest() else {
        return false;
    };
    manifest
        .get(artifact.artifact_type.name())
        .and_then(|records| records.get(&artifact.id))
        .is_some_and(|rec| {
            rec.cache_id.as_deref() == Some(cache_id)
                // None stamps mean freshness is unknowable (git); only
                // a real, matching stamp can vouch for the cache entry.
                && (rec.modified.is_some() || rec.size.is_some())
                && rec.modified == artifact.modified
                && rec.size == artifact.size
                && crate::cache::cache_path(cache_id).is_ok_and(|p| p.exists())
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
    let (modified, size) = sources::source_for(bundle, artifact_type)?.stamp(project, id)?;
    // None stamps mean freshness is unknowable; only a real,
    // matching stamp can vouch for the cache entry.
    ((rec.modified.is_some() || rec.size.is_some())
        && rec.modified == modified
        && rec.size == size
        && crate::cache::cache_path(&cache_id).is_ok_and(|p| p.exists()))
    .then_some(cache_id)
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
    Ok(config_dir()?.join(MANIFEST_FILE_NAME))
}

/// Take the exclusive advisory lock serializing manifest writers
/// across processes (query auto-syncs and imports can run
/// concurrently). A sibling lock file — never renamed, unlike the
/// manifest itself — held until the returned handle drops.
fn lock_manifest() -> Result<std::fs::File> {
    let path = manifest_path()?;
    let dir = path.parent().expect("manifest path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let lock_path = dir.join(MANIFEST_LOCK_FILE_NAME);
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
    let tmp = dir.join(format!("{MANIFEST_FILE_NAME}.{}.tmp", std::process::id()));
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
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(home.join(".claude"));
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn cached_step_count(cache_id: &str) -> usize {
        let path = crate::cache::cache_path(cache_id).unwrap();
        let json = std::fs::read_to_string(path).unwrap();
        let doc = toolpath::v1::Graph::from_json(&json).unwrap();
        doc.single_path().map(|p| p.steps.len()).unwrap_or(0)
    }

    fn make_ref(artifact_type: ArtifactType, id: &str) -> ArtifactRef {
        ArtifactRef {
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
    fn enumerated_claude_sessions_are_stamped() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);
            let source = sources::source_for(&bundle, ArtifactType::Claude).unwrap();
            let artifacts = source.enumerate();
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].id, "sess-aaa");
            assert_eq!(artifacts[0].path.as_deref(), Some("/test/project"));
            assert!(
                artifacts[0].modified.is_some(),
                "file mtime must be stamped"
            );
            assert!(artifacts[0].size.unwrap() > 0, "file size must be stamped");
        });
    }

    #[test]
    fn first_sync_ingests_then_second_is_unchanged() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            write_claude_session(home, "-test-project", "sess-bbb", "Fix a bug");
            let bundle = claude_bundle(home);

            let outcomes = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
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
                crate::cache::cache_path(cache_id).unwrap().exists(),
                "cache doc must exist for {cache_id}"
            );

            let (_, second) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
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
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();

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

            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
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

            let outcomes = sync_bundle(&bundle, &[ArtifactType::Codex], &mut ()).unwrap();
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
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();

            // Losing the manifest (or a prior manual `p import`) leaves a
            // cache entry sync doesn't know about; re-syncing must
            // overwrite it, not die on the exists-check.
            std::fs::remove_file(manifest_path().unwrap()).unwrap();
            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
            assert_eq!((outcome.new, outcome.failed), (1, 0));
        });
    }

    #[test]
    fn failed_derivation_is_tallied_and_skipped() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);
            let source = sources::source_for(&bundle, ArtifactType::Claude).unwrap();
            let mut artifacts = source.enumerate();
            artifacts.push(make_ref(ArtifactType::Claude, "does-not-exist"));

            let outcome = sync_artifacts(
                source.as_ref(),
                ArtifactType::Claude,
                &artifacts,
                &BTreeMap::new(),
                &mut (),
            )
            .unwrap();
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
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
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

            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
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
            let (_, again) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
            assert_eq!((again.updated, again.unchanged), (0, 1));
        });
    }

    #[test]
    fn all_none_stamps_never_read_as_unchanged() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();

            // A record whose stamps are all None (stat failed when it
            // was written) must not match a stub whose stat also
            // failed — unknowable freshness re-derives.
            let mut records = load_manifest().unwrap()["claude"].clone();
            let rec = records.get_mut("sess-aaa").unwrap();
            rec.modified = None;
            rec.size = None;
            let artifact = make_ref(ArtifactType::Claude, "sess-aaa");
            let source = sources::source_for(&bundle, ArtifactType::Claude).unwrap();
            let outcome = sync_artifacts(
                source.as_ref(),
                ArtifactType::Claude,
                &[artifact],
                &records,
                &mut (),
            )
            .unwrap();
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
            let derived = crate::derive::derive_claude_session_with(
                bundle.claude.as_ref().unwrap(),
                "/test/project",
                "sess-aaa",
            )
            .unwrap();
            let artifact = derived.provenance.as_ref().unwrap();
            assert_eq!(artifact.id, "sess-aaa");
            assert!(artifact.modified.is_some() && artifact.size.is_some());
            crate::cache::write_cached(&derived.cache_id, &derived.doc, true).unwrap();
            record_artifact(artifact, &derived.cache_id).unwrap();

            // The import's stamp must match sync's own enumeration.
            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
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
    fn evicted_cache_entry_rematerializes_on_next_sync() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
            let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                .cache_id
                .clone()
                .unwrap();

            // `p cache rm`: doc removed, record downgraded to known.
            crate::cache::remove_cached(&cache_id).unwrap();
            evict_cache_id(&cache_id).unwrap();
            assert!(
                load_manifest().unwrap()["claude"]["sess-aaa"]
                    .cache_id
                    .is_none()
            );

            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
            assert_eq!((outcome.new, outcome.updated), (0, 1));
            assert!(
                crate::cache::cache_path(&cache_id).unwrap().exists(),
                "evicted artifact re-materializes"
            );
        });
    }

    #[test]
    fn manually_deleted_doc_is_restored_even_with_stale_record() {
        with_cfg(|home| {
            write_claude_session(home, "-test-project", "sess-aaa", "Add a feature");
            let bundle = claude_bundle(home);
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
            let cache_id = load_manifest().unwrap()["claude"]["sess-aaa"]
                .cache_id
                .clone()
                .unwrap();

            // Doc deleted behind the CLI's back: the record still claims
            // materialization, but sync verifies the doc exists.
            let doc = crate::cache::cache_path(&cache_id).unwrap();
            std::fs::remove_file(&doc).unwrap();
            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
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

            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
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
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
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
            crate::cache::remove_cached(&cache_id).unwrap();
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
    fn newest_first_orders_by_mtime_with_unstamped_last() {
        let mut old = make_ref(ArtifactType::Claude, "old");
        old.modified = Some("2026-01-01T00:00:00Z".parse().unwrap());
        let mut new = make_ref(ArtifactType::Claude, "new");
        new.modified = Some("2026-07-01T00:00:00Z".parse().unwrap());
        let unstamped = make_ref(ArtifactType::Claude, "unstamped");

        let stubs = vec![old, unstamped, new];
        let ids: Vec<&str> = newest_first(&stubs).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "old", "unstamped"]);
    }
}
