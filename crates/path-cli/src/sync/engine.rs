//! The sync engine: the manifest at `$CONFIG_DIR/manifest.json`, the
//! stat-gated ingestion loop, and the record surfaces `p import`,
//! `share`, and `p cache rm` use to keep the manifest honest.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use toolpath::v1::{Graph, PathOrRef};
use toolpath_redact::{Action, Decision, DetectorSet, RedactConfig, RedactionPolicy};

use super::sources::{self, ArtifactSource};
use crate::artifact::{ArtifactRef, ArtifactType};
use crate::cache::write_cached;
use crate::config::{MANIFEST_FILE_NAME, MANIFEST_LOCK_FILE_NAME, config_dir};
use crate::derive::DerivedDoc;
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
    /// Documents whose stored redaction policy was replayed over the
    /// re-derived content. A subset of `updated`, tallied separately
    /// because it answers a different question.
    pub(crate) re_redacted: usize,
    /// Findings a previous redaction individually skipped that the
    /// replayed policy redacts again — a hand-picked skip refers to
    /// content that has since moved, so it cannot be replayed.
    pub(crate) reappeared_skips: usize,
}

impl SyncOutcome {
    /// Artifacts seen, by disposition. The redaction counters are a
    /// different axis and are deliberately not summed in.
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
            build_detectors,
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
    detectors: DetectorFactory,
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
    let mut detectors = DetectorCache::new(detectors);
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
        let policy = existing.and_then(|r| r.redaction.as_ref());
        match derive_replaying_redaction(source, artifact, policy, &mut detectors) {
            Ok((derived, replay)) => {
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
                        redaction: policy.cloned(),
                    },
                );
                outcome.re_redacted += replay.documents;
                outcome.reappeared_skips += replay.reappeared_skips;
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

// ── redaction replay ───────────────────────────────────────────────

/// How replay rebuilds the detectors a stored policy names. A parameter
/// rather than a direct call so tests drive replay without the vendored
/// ruleset.
type DetectorFactory = fn(&[String]) -> Result<DetectorSet>;

/// What one artifact's replay did, for the sync summary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Replay {
    documents: usize,
    reappeared_skips: usize,
}

/// The detector registry replay resolves policy names against.
///
/// Returning `None` fails the artifact rather than replaying with a
/// weaker detector set than the policy was written for, which would
/// write back a document missing redactions it had.
fn detector_named(name: &str) -> Option<Box<dyn toolpath_redact::Detector>> {
    match name {
        "internal" => Some(Box::new(toolpath_redact::internal::InternalDetector::new())),
        _ => None,
    }
}

fn build_detectors(names: &[String]) -> Result<DetectorSet> {
    let mut set = DetectorSet::default();
    for name in names {
        let detector = detector_named(name).ok_or_else(|| {
            anyhow!("no detector named {name:?}; cannot replay this document's redaction")
        })?;
        set.push(detector);
    }
    Ok(set)
}

/// One detector set per distinct name list per run. The built-in
/// detector compiles a few hundred regexes on construction, and a sync
/// typically replays one policy across many documents.
struct DetectorCache {
    build: DetectorFactory,
    sets: BTreeMap<Vec<String>, DetectorSet>,
}

impl DetectorCache {
    fn new(build: DetectorFactory) -> Self {
        Self {
            build,
            sets: BTreeMap::new(),
        }
    }

    fn get(&mut self, names: &[String]) -> Result<&DetectorSet> {
        if !self.sets.contains_key(names) {
            let set = (self.build)(names)?;
            self.sets.insert(names.to_vec(), set);
        }
        Ok(&self.sets[names])
    }
}

/// Derive an artifact, then replay the redaction policy its record
/// carries.
///
/// `is_unchanged` never inspects the document, so a re-derive would
/// clobber an in-place redaction. Replay the policy before writing.
/// Either half failing fails the artifact, which leaves the previously
/// redacted document in place — never an un-redacted one over it.
fn derive_replaying_redaction(
    source: &dyn ArtifactSource,
    artifact: &ArtifactRef,
    policy: Option<&RedactionPolicy>,
    detectors: &mut DetectorCache,
) -> Result<(DerivedDoc, Replay)> {
    let mut derived = source.derive(artifact)?;
    let Some(policy) = policy else {
        return Ok((derived, Replay::default()));
    };
    let replay = replay_policy(&derived.cache_id, policy, &mut derived.doc, detectors)?;
    Ok((derived, replay))
}

/// Re-redact `doc` under `policy`, counting what a rule-based policy
/// cannot preserve.
fn replay_policy(
    cache_id: &str,
    policy: &RedactionPolicy,
    doc: &mut Graph,
    detectors: &mut DetectorCache,
) -> Result<Replay> {
    let key = crate::cache::read_redact_key(&policy.key_id)?.ok_or_else(|| {
        anyhow!(
            "redaction key {} is missing; refusing to overwrite the redacted document \
             with an un-redacted re-derive",
            policy.key_id
        )
    })?;
    let set = detectors.get(&policy.detectors)?;
    let decisions = policy_decisions(policy)?;
    let cfg = replay_config(policy, key);
    let reappeared_skips = count_reappeared_skips(cache_id, set, &cfg, &decisions);

    for path in doc.paths.iter_mut() {
        let PathOrRef::Path(path) = path else {
            continue;
        };
        let plan = plan_for(path, set, &cfg, &decisions)?;
        toolpath_redact::apply(path, &plan, &cfg)?;
    }
    Ok(Replay {
        documents: 1,
        reappeared_skips,
    })
}

fn plan_for(
    path: &toolpath::v1::Path,
    set: &DetectorSet,
    cfg: &RedactConfig,
    decisions: &[Decision],
) -> Result<toolpath_redact::Plan> {
    let mut plan = toolpath_redact::plan::generate_checked(
        path, set, cfg,
        // A sync runs unattended, so a detector that would send
        // candidate material off the machine has nobody to approve it.
        false,
    )?;
    toolpath_redact::plan::apply_decisions(&mut plan, decisions);
    Ok(plan)
}

/// Findings the policy would redact that are still present in the
/// document as it was last redacted: exactly the ones a previous run
/// skipped by hand. Rule-based decisions replay, so anything the
/// `reject` predicates cover is excluded here.
///
/// Informational only — a document that cannot be read or planned
/// reports nothing rather than failing an otherwise good replay.
fn count_reappeared_skips(
    cache_id: &str,
    set: &DetectorSet,
    cfg: &RedactConfig,
    decisions: &[Decision],
) -> usize {
    let Ok(file) = crate::cache::cache_path(cache_id) else {
        return 0;
    };
    let Ok(json) = std::fs::read_to_string(&file) else {
        return 0;
    };
    let Ok(previous) = Graph::from_json(&json) else {
        return 0;
    };
    previous
        .paths
        .iter()
        .filter_map(|p| match p {
            PathOrRef::Path(path) => plan_for(path, set, cfg, decisions).ok(),
            PathOrRef::Ref(_) => None,
        })
        .map(|plan| {
            plan.findings
                .iter()
                .filter(|f| f.action == Action::Redact)
                .count()
        })
        .sum()
}

/// The policy's predicates as decisions. `reject` lands last because
/// [`toolpath_redact::plan::apply_decisions`] lets later decisions win,
/// so an explicit skip survives a broader accept.
fn policy_decisions(policy: &RedactionPolicy) -> Result<Vec<Decision>> {
    let mut out = Vec::with_capacity(policy.accept.len() + policy.reject.len());
    for (predicates, action) in [
        (&policy.accept, Action::Redact),
        (&policy.reject, Action::Skip),
    ] {
        for source in predicates {
            out.push(Decision {
                predicate: toolpath_redact::parse_predicate(source)?,
                action,
                transform: None,
            });
        }
    }
    Ok(out)
}

fn replay_config(policy: &RedactionPolicy, key: Vec<u8>) -> RedactConfig {
    RedactConfig {
        threshold: policy.threshold,
        mode: policy.mode,
        mode_for: policy.mode_for.clone(),
        key,
        now: Utc::now(),
        // Dropping signatures and revealing values both need a human at
        // the terminal; a signed document fails its replay instead, and
        // keeps the redacted copy it already has.
        drop_signatures: false,
        reveal: false,
    }
}

/// Store the policy `path p redact` applied to a cached document, so a
/// later re-derive replays it. Reports whether any record pointed at
/// `cache_id`: a document sync does not track (a file input, a github
/// or pathbase import) has nowhere to keep one, and its redaction will
/// not survive a re-derive because nothing re-derives it.
// Called by `path p redact`; drop the allow once that dispatch lands.
#[allow(dead_code)]
pub(crate) fn record_redaction_policy(cache_id: &str, policy: &RedactionPolicy) -> Result<bool> {
    let mut recorded = false;
    update_manifest(|manifest| {
        for records in manifest.values_mut() {
            for rec in records.values_mut() {
                if rec.cache_id.as_deref() == Some(cache_id) {
                    rec.redaction = Some(policy.clone());
                    recorded = true;
                }
            }
        }
    })?;
    Ok(recorded)
}

/// Record an externally-derived cache write (`p import`, `share`) in
/// the manifest, so sync doesn't re-derive what was just written.
pub(crate) fn record_artifact(artifact: &ArtifactRef, cache_id: &str) -> Result<()> {
    update_manifest(|manifest| {
        let records = manifest
            .entry(artifact.artifact_type.name().to_string())
            .or_default();
        // Re-importing a session must not drop the redaction policy
        // guarding it; only `p cache rm` clears that.
        let redaction = records.get(&artifact.id).and_then(|r| r.redaction.clone());
        records.insert(
            artifact.id.clone(),
            SyncRecord {
                path: artifact.path.clone(),
                cache_id: Some(cache_id.to_string()),
                modified: artifact.modified,
                size: artifact.size,
                synced_at: Utc::now(),
                redaction,
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
                    // The key goes with the document, and a policy whose
                    // key is gone can only fail every future sync — so
                    // the explicit `rm` retires both together.
                    rec.redaction = None;
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

    /// A detector named `literal:<prefix>`, so replay wiring is testable
    /// without the vendored ruleset. Claims the whole alphanumeric run
    /// starting at the prefix, so one name covers a family of keys the
    /// way a real rule does.
    struct LiteralDetector(String);

    impl toolpath_redact::Detector for LiteralDetector {
        fn id(&self) -> &'static str {
            "literal"
        }

        fn detect(
            &self,
            c: &toolpath_redact::Candidate<'_>,
        ) -> toolpath_redact::Result<Vec<toolpath_redact::Finding>> {
            Ok(c.text
                .match_indices(&self.0)
                .map(|(start, m)| {
                    let tail = &c.text[start + m.len()..];
                    let run = tail
                        .find(|ch: char| !ch.is_ascii_alphanumeric())
                        .unwrap_or(tail.len());
                    toolpath_redact::Finding {
                        span: start..start + m.len() + run,
                        rule: "literal".to_string(),
                        score: 1.0,
                        detector: "literal",
                    }
                })
                .collect())
        }
    }

    fn literal_detectors(names: &[String]) -> Result<DetectorSet> {
        let mut set = DetectorSet::default();
        for name in names {
            let literal = name
                .strip_prefix("literal:")
                .ok_or_else(|| anyhow!("test detector names are literal:<text>, got {name:?}"))?;
            set.push(Box::new(LiteralDetector(literal.to_string())));
        }
        Ok(set)
    }

    fn literal_policy(secret: &str, key_id: &str) -> RedactionPolicy {
        policy_with(vec![format!("literal:{secret}")], key_id)
    }

    fn policy_with(detectors: Vec<String>, key_id: &str) -> RedactionPolicy {
        RedactionPolicy {
            detectors,
            threshold: 0.8,
            mode: toolpath_redact::Transform::Marker,
            mode_for: Vec::new(),
            accept: Vec::new(),
            reject: Vec::new(),
            key_id: key_id.to_string(),
        }
    }

    /// Sync one type the way `sync_bundle` does, with the test detector
    /// registry in place of the built-in one.
    fn sync_claude(bundle: &HarnessBundle) -> SyncOutcome {
        let source = sources::source_for(bundle, ArtifactType::Claude).unwrap();
        let artifacts = source.enumerate();
        let records = load_manifest()
            .unwrap()
            .get("claude")
            .cloned()
            .unwrap_or_default();
        sync_artifacts(
            source.as_ref(),
            ArtifactType::Claude,
            &artifacts,
            &records,
            &mut (),
            literal_detectors,
        )
        .unwrap()
    }

    /// What `path p redact` leaves behind: a key, a redacted document,
    /// and a policy on the artifact's manifest record. `hand` stands in
    /// for the interactive picker, which can decide a finding the
    /// policy cannot express.
    fn redact_cached_with(
        cache_id: &str,
        policy: &RedactionPolicy,
        detectors: DetectorFactory,
        hand: impl Fn(&mut toolpath_redact::Plan),
    ) {
        let key = crate::cache::load_or_create_redact_key(&policy.key_id).unwrap();
        let file = crate::cache::cache_path(cache_id).unwrap();
        let mut doc = Graph::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let set = detectors(&policy.detectors).unwrap();
        let decisions = policy_decisions(policy).unwrap();
        let cfg = replay_config(policy, key);
        for path in doc.paths.iter_mut() {
            let PathOrRef::Path(path) = path else {
                continue;
            };
            let mut plan = plan_for(path, &set, &cfg, &decisions).unwrap();
            hand(&mut plan);
            toolpath_redact::apply(path, &plan, &cfg).unwrap();
        }
        write_cached(cache_id, &doc, true).unwrap();
        assert!(record_redaction_policy(cache_id, policy).unwrap());
    }

    fn redact_cached(cache_id: &str, policy: &RedactionPolicy) {
        redact_cached_with(cache_id, policy, literal_detectors, |_| {});
    }

    fn append_turn(home: &Path, session: &str, text: &str) {
        let file = home
            .join(".claude/projects/-test-project")
            .join(format!("{session}.jsonl"));
        let mut body = std::fs::read_to_string(&file).unwrap();
        body.push_str(&format!(
            r#"{{"type":"user","uuid":"u-{text:.4}","timestamp":"2024-01-02T00:05:00Z","cwd":"/test/project","message":{{"role":"user","content":"{text}"}}}}"#
        ));
        body.push('\n');
        std::fs::write(&file, body).unwrap();
    }

    /// The cache entry a synced claude session landed at.
    fn cached_id(session: &str) -> String {
        load_manifest().unwrap()["claude"][session]
            .cache_id
            .clone()
            .expect("the session is materialized")
    }

    fn read_cached(cache_id: &str) -> String {
        std::fs::read_to_string(crate::cache::cache_path(cache_id).unwrap()).unwrap()
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
                    redaction: Some(literal_policy("AKIA", "claude-p1")),
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
                build_detectors,
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
                build_detectors,
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
    fn manifest_without_redaction_field_still_loads() {
        let json = r#"{"claude":{"sess-1":{"cache_id":"claude-sess-1","synced_at":"2026-01-01T00:00:00Z"}}}"#;
        assert!(serde_json::from_str::<Manifest>(json).is_ok());
    }

    #[test]
    fn unknown_detector_refuses_replay() {
        assert_eq!(
            build_detectors(&["internal".to_string()])
                .map(|set| set.ids())
                .unwrap_or_default(),
            vec!["internal"]
        );
        let err = build_detectors(&["not-a-detector".to_string()])
            .err()
            .expect("no detector is resolvable by that name");
        assert!(err.to_string().contains("cannot replay"));
    }

    #[test]
    fn import_preserves_the_redaction_policy() {
        with_cfg(|_| {
            let artifact = make_ref(ArtifactType::Claude, "sess-aaa");
            record_artifact(&artifact, "claude-sess-aaa").unwrap();
            record_redaction_policy(
                "claude-sess-aaa",
                &literal_policy("AKIA", "claude-sess-aaa"),
            )
            .unwrap();

            // A re-import of the same session rewrites the record.
            record_artifact(&artifact, "claude-sess-aaa").unwrap();
            assert!(
                load_manifest().unwrap()["claude"]["sess-aaa"]
                    .redaction
                    .is_some(),
                "an import must not drop the policy guarding the document"
            );
        });
    }

    #[test]
    fn eviction_retires_the_policy_with_the_document() {
        with_cfg(|_| {
            let artifact = make_ref(ArtifactType::Claude, "sess-aaa");
            record_artifact(&artifact, "claude-sess-aaa").unwrap();
            record_redaction_policy(
                "claude-sess-aaa",
                &literal_policy("AKIA", "claude-sess-aaa"),
            )
            .unwrap();

            evict_cache_id("claude-sess-aaa").unwrap();
            let rec = &load_manifest().unwrap()["claude"]["sess-aaa"];
            assert!(rec.cache_id.is_none());
            assert!(
                rec.redaction.is_none(),
                "a policy whose key `p cache rm` deleted would fail every future sync"
            );
        });
    }

    #[test]
    fn record_redaction_policy_reports_untracked_documents() {
        with_cfg(|_| {
            assert!(
                !record_redaction_policy("github-owner-repo-42", &literal_policy("AKIA", "x"))
                    .unwrap()
            );
        });
    }

    #[test]
    fn sync_fails_loudly_on_missing_key() {
        with_cfg(|home| {
            write_claude_session(
                home,
                "-test-project",
                "sess-1",
                "key AKIAIOSFODNN7REALKEY here",
            );
            let bundle = claude_bundle(home);
            sync_claude(&bundle);
            let cache_id = load_manifest().unwrap()["claude"]["sess-1"]
                .cache_id
                .clone()
                .unwrap();
            let policy = literal_policy("AKIAIOSFODNN7", &cache_id);
            redact_cached(&cache_id, &policy);
            let redacted = read_cached(&cache_id);

            // The key is lost behind the CLI's back — an out-of-band
            // delete, a restored config dir, a half-copied machine.
            crate::cache::remove_redact_key(&policy.key_id).unwrap();
            append_turn(home, "sess-1", "another turn");

            let outcome = sync_claude(&bundle);
            assert_eq!(
                (outcome.updated, outcome.failed, outcome.re_redacted),
                (0, 1, 0)
            );
            assert_eq!(
                read_cached(&cache_id),
                redacted,
                "an un-redacted re-derive must never land on top of a redacted document"
            );
        });
    }

    #[test]
    fn sync_skips_redacted_doc_when_source_unchanged() {
        with_cfg(|home| {
            write_claude_session(
                home,
                "-test-project",
                "sess-1",
                "key AKIAIOSFODNN7REALKEY here",
            );
            let bundle = claude_bundle(home);
            sync_claude(&bundle);
            let cache_id = load_manifest().unwrap()["claude"]["sess-1"]
                .cache_id
                .clone()
                .unwrap();
            redact_cached(&cache_id, &literal_policy("AKIAIOSFODNN7", &cache_id));
            let redacted = read_cached(&cache_id);

            let outcome = sync_claude(&bundle);
            assert_eq!(
                (outcome.unchanged, outcome.re_redacted),
                (1, 0),
                "an untouched source is not re-derived, so there is nothing to replay"
            );
            assert_eq!(read_cached(&cache_id), redacted);
        });
    }

    #[test]
    fn sync_reapplies_redaction_after_source_grows() {
        with_cfg(|home| {
            write_claude_session(
                home,
                "-test-project",
                "sess-1",
                "key AKIAIOSFODNN7REALKEY here",
            );
            let bundle = claude_bundle(home);
            sync_claude(&bundle);
            let cache_id = cached_id("sess-1");
            redact_cached(&cache_id, &literal_policy("AKIAIOSFODNN7", &cache_id));

            append_turn(home, "sess-1", "another turn with AKIAIOSFODNN7SECONDKEY");
            let outcome = sync_claude(&bundle);
            assert_eq!((outcome.updated, outcome.re_redacted), (1, 1));

            let doc = read_cached(&cache_id);
            assert!(doc.contains("another turn"), "new content must land");
            assert!(
                !doc.contains("AKIAIOSFODNN7REALKEY"),
                "redaction must survive re-derive"
            );
            assert!(
                !doc.contains("AKIAIOSFODNN7SECONDKEY"),
                "new content must be redacted too"
            );
        });
    }

    #[test]
    fn sync_reports_reappeared_skips() {
        with_cfg(|home| {
            write_claude_session(
                home,
                "-test-project",
                "sess-1",
                "key AKIAIOSFODNN7REALKEY here",
            );
            let bundle = claude_bundle(home);
            sync_claude(&bundle);
            append_turn(home, "sess-1", "and AKIAIOSFODNN7SECONDKEY too");
            sync_claude(&bundle);

            // The user redacted one of the two and skipped the other by
            // hand — a decision no rule-based policy can express.
            let cache_id = cached_id("sess-1");
            let policy = literal_policy("AKIAIOSFODNN7", &cache_id);
            redact_cached_with(&cache_id, &policy, literal_detectors, |plan| {
                plan.findings.last_mut().unwrap().action = Action::Skip;
            });
            assert!(read_cached(&cache_id).contains("AKIAIOSFODNN7SECONDKEY"));

            append_turn(home, "sess-1", "a third turn");
            let outcome = sync_claude(&bundle);
            assert_eq!((outcome.re_redacted, outcome.reappeared_skips), (1, 1));
            assert!(
                !read_cached(&cache_id).contains("AKIAIOSFODNN7SECONDKEY"),
                "a skip that cannot be replayed fails closed: the finding comes back"
            );
        });
    }

    #[test]
    fn sync_bundle_replays_the_builtin_detector() {
        with_cfg(|home| {
            write_claude_session(
                home,
                "-test-project",
                "sess-1",
                "aws access_key AKIAIOSFODNN7REALKEY",
            );
            let bundle = claude_bundle(home);
            sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap();
            let cache_id = cached_id("sess-1");
            let policy = policy_with(vec!["internal".to_string()], &cache_id);
            redact_cached_with(&cache_id, &policy, build_detectors, |_| {});
            assert!(!read_cached(&cache_id).contains("AKIAIOSFODNN7REALKEY"));

            append_turn(home, "sess-1", "and access_key AKIAZYTQRMLKNPVWXCDE");
            let (_, outcome) = sync_bundle(&bundle, &[ArtifactType::Claude], &mut ()).unwrap()[0];
            assert_eq!((outcome.updated, outcome.re_redacted), (1, 1));

            let doc = read_cached(&cache_id);
            assert!(doc.contains("and access_key"), "new content must land");
            assert!(!doc.contains("AKIAIOSFODNN7REALKEY"));
            assert!(!doc.contains("AKIAZYTQRMLKNPVWXCDE"));
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
