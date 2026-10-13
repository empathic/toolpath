//! Where each session's path lives remotely: `$CONFIG_DIR/remotes.json`.
//!
//! The manifest is a cache index any sync can rebuild, and path-cli
//! builds before 0.30.0 rewrite it wholesale without fields they do not
//! know. These records are the one thing under the config dir nothing
//! can rebuild, so they live in their own file, written under the
//! manifest's lock.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::engine::{lock_manifest, write_config_file};
use super::sources;
use crate::artifact::ArtifactType;
use crate::config::{MANIFEST_FILE_NAME, REMOTES_FILE_NAME};

/// The remotes file format this build reads and writes.
const REMOTES_VERSION: u32 = 1;

/// What one remote holds of a session's path. The remote's copy is a
/// prefix of the local derive: `steps` steps, the last of them
/// `last_step`. The key the record sits under is the path's resource
/// URL on that remote — on Pathbase its API URL,
/// `<server>/api/v1/u/<owner>/repos/<repo>/graphs/<graph>/paths/<path>`
/// (`cmd_pathbase::remote_path_url`) — so the containing graph is the
/// key without its last two segments and is not stored. Nothing reads
/// these yet; incremental append and `share --all` will.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteRecord {
    /// How many steps of the derive the remote holds.
    pub(crate) steps: usize,
    /// Id of the derive's step `steps - 1`.
    pub(crate) last_step: String,
    /// The head the remote was last told.
    pub(crate) head: String,
    /// The local source's stamp ([`stamp_string`]) when those steps
    /// were sent.
    pub(crate) source_stamp: String,
}

impl RemoteRecord {
    /// The record for a remote holding every step of `path`, taken
    /// while the local source was at `stamp`. `None` for a path with
    /// no steps: there is nothing to hold.
    pub(crate) fn of(path: &toolpath::v1::Path, stamp: sources::Stamp) -> Option<Self> {
        let last = path.steps.last()?;
        Some(Self {
            steps: path.steps.len(),
            last_step: last.step.id.clone(),
            head: path.path.head.clone(),
            source_stamp: stamp_string(stamp),
        })
    }
}

/// A source stamp as one string, `<modified>/<size>`, each part empty
/// when the source does not provide it. The same stamp the stat gate
/// compares, so a pass can skip a session whose source has not moved
/// since the record was written.
pub(crate) fn stamp_string((modified, size): sources::Stamp) -> String {
    format!(
        "{}/{}",
        modified
            .map(|m| m.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            .unwrap_or_default(),
        size.map(|s| s.to_string()).unwrap_or_default()
    )
}

/// One session's records: path URL → record.
pub(crate) type SessionRemotes = BTreeMap<String, RemoteRecord>;

/// The remotes file: `{ "version": 1, "sessions": { <session key>:
/// { <path URL>: record } } }`, the session key being [`session_key`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Remotes {
    pub(crate) version: u32,
    pub(crate) sessions: BTreeMap<String, SessionRemotes>,
}

impl Default for Remotes {
    fn default() -> Self {
        Self {
            version: REMOTES_VERSION,
            sessions: BTreeMap::new(),
        }
    }
}

/// The key a session's records sit under: the manifest's artifact type
/// and id, `<type>:<id>`.
pub(crate) fn session_key(artifact_type: ArtifactType, id: &str) -> String {
    format!("{}:{id}", artifact_type.name())
}

/// Write `record` under `url` for the session (`artifact_type`, `id`),
/// replacing any earlier record at that URL.
pub(crate) fn record_remote(
    config_dir: &Path,
    artifact_type: ArtifactType,
    id: &str,
    url: &str,
    record: RemoteRecord,
) -> Result<()> {
    let _lock = lock_manifest(config_dir)?;
    let mut remotes = load_remotes(config_dir)?;
    remotes
        .sessions
        .entry(session_key(artifact_type, id))
        .or_default()
        .insert(url.to_string(), record);
    save_remotes(config_dir, &remotes)
}

/// The remotes file; empty when there is none.
pub(crate) fn load_remotes(config_dir: &Path) -> Result<Remotes> {
    let path = config_dir.join(REMOTES_FILE_NAME);
    let json = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Remotes::default()),
        Err(e) => return Err(anyhow!("read {}: {e}", path.display())),
    };
    let remotes: Remotes =
        serde_json::from_str(&json).with_context(|| format!("parse {}", path.display()))?;
    if remotes.version != REMOTES_VERSION {
        bail!(
            "{} is version {}; this path-cli reads version {REMOTES_VERSION}, upgrade it",
            path.display(),
            remotes.version
        );
    }
    Ok(remotes)
}

fn save_remotes(config_dir: &Path, remotes: &Remotes) -> Result<()> {
    write_config_file(
        config_dir,
        REMOTES_FILE_NAME,
        &serde_json::to_string_pretty(remotes)?,
    )
}

/// Move remote records a 0.30.0 pre-release build wrote into the
/// manifest (`<type> → <id> → { remotes: { <url>: record } }`) into the
/// remotes file, rekeyed to the path's API URL. A record already in the
/// remotes file at that URL wins. The caller holds the manifest lock
/// and saves the manifest next, which drops the field. An unreadable
/// manifest is left for `load_manifest` to report.
pub(super) fn lift_from_manifest(config_dir: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct LegacyRecord {
        #[serde(default)]
        remotes: SessionRemotes,
    }

    let json = match std::fs::read_to_string(config_dir.join(MANIFEST_FILE_NAME)) {
        Ok(s) if s.contains("\"remotes\"") => s,
        _ => return Ok(()),
    };
    let Ok(manifest) =
        serde_json::from_str::<BTreeMap<String, BTreeMap<String, LegacyRecord>>>(&json)
    else {
        return Ok(());
    };
    let mut lifted = Vec::new();
    for (name, records) in manifest {
        let Some(artifact_type) = ArtifactType::parse(&name) else {
            continue;
        };
        for (id, rec) in records {
            for (url, remote) in rec.remotes {
                lifted.push((session_key(artifact_type, &id), api_url(&url), remote));
            }
        }
    }
    if lifted.is_empty() {
        return Ok(());
    }
    let mut remotes = load_remotes(config_dir)?;
    for (key, url, remote) in lifted {
        remotes
            .sessions
            .entry(key)
            .or_default()
            .entry(url)
            .or_insert(remote);
    }
    save_remotes(config_dir, &remotes)
}

/// The API URL for a key in the pre-release spelling
/// `<server>/u/<owner>/<repo>/graphs/<graph>/paths/<path>`; any other
/// key unchanged.
fn api_url(key: &str) -> String {
    let segs: Vec<&str> = key.rsplitn(8, '/').collect();
    match segs.as_slice() {
        [path, "paths", graph, "graphs", repo, owner, "u", server] => {
            format!("{server}/api/v1/u/{owner}/repos/{repo}/graphs/{graph}/paths/{path}")
        }
        _ => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{evict_cache_id, load_manifest};

    const PATH_URL: &str = "https://a/api/v1/u/me/repos/x/graphs/g1/paths/p1";

    fn remote_record(steps: usize) -> RemoteRecord {
        RemoteRecord {
            steps,
            last_step: format!("s{}", steps - 1),
            head: format!("s{}", steps - 1),
            source_stamp: "2026-01-01T00:00:00Z/10".to_string(),
        }
    }

    fn session(config_dir: &Path, id: &str) -> SessionRemotes {
        load_remotes(config_dir)
            .unwrap()
            .sessions
            .remove(&session_key(ArtifactType::Claude, id))
            .unwrap_or_default()
    }

    /// What a pre-0.30.0 path-cli writes back after its own manifest
    /// update: the records it knows, nothing else.
    const OLD_MANIFEST: &str = r#"{"claude":{"s1":{"path":"/p","cache_id":"claude-s1","size":4096,"synced_at":"2026-07-09T00:00:00Z"}}}"#;

    #[test]
    fn record_remote_writes_the_versioned_file_and_replaces_per_url() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let other = "https://a/api/v1/u/me/repos/x/graphs/g2/paths/p2";
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(1)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", other, remote_record(2)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(3)).unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cfg.join(REMOTES_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["sessions"]["claude:s1"][PATH_URL]["steps"], 3);
        assert_eq!(json["sessions"]["claude:s1"][other]["steps"], 2);
        assert!(
            !cfg.join(MANIFEST_FILE_NAME).exists(),
            "a record does not touch the manifest"
        );
    }

    #[test]
    fn remotes_of_another_version_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(REMOTES_FILE_NAME),
            r#"{"version":2,"sessions":{}}"#,
        )
        .unwrap();
        let err = load_remotes(dir.path()).unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
    }

    /// An older path-cli rewriting the manifest cannot lose a record,
    /// and a manifest write that follows keeps it too.
    #[test]
    fn an_old_manifest_round_trip_leaves_remotes_intact() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(2)).unwrap();
        std::fs::write(cfg.join(MANIFEST_FILE_NAME), OLD_MANIFEST).unwrap();
        evict_cache_id(cfg, "claude-s1").unwrap();

        assert_eq!(load_manifest(cfg).unwrap()["claude"]["s1"].cache_id, None);
        assert_eq!(session(cfg, "s1")[PATH_URL], remote_record(2));
    }

    /// Records a pre-release build left in the manifest move to the
    /// remotes file under the API URL on the next manifest write, and
    /// the manifest is saved without them.
    #[test]
    fn manifest_remotes_are_lifted_once_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let legacy = serde_json::json!({
            "claude": {"s1": {
                "path": "/p",
                "synced_at": "2026-07-09T00:00:00Z",
                "remotes": {
                    "https://a/u/me/x/graphs/g1/paths/p1": remote_record(4),
                },
            }},
        });
        std::fs::write(cfg.join(MANIFEST_FILE_NAME), legacy.to_string()).unwrap();
        evict_cache_id(cfg, "none").unwrap();

        assert_eq!(
            session(cfg, "s1"),
            [(PATH_URL.to_string(), remote_record(4))].into()
        );
        let manifest = std::fs::read_to_string(cfg.join(MANIFEST_FILE_NAME)).unwrap();
        assert!(!manifest.contains("remotes"), "{manifest}");
        assert_eq!(
            load_manifest(cfg).unwrap()["claude"]["s1"].path.as_deref(),
            Some("/p")
        );
    }

    /// A newer record at the same URL is not overwritten by a lifted one.
    #[test]
    fn a_lifted_record_does_not_replace_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(5)).unwrap();
        let legacy = serde_json::json!({
            "claude": {"s1": {
                "synced_at": "2026-07-09T00:00:00Z",
                "remotes": {"https://a/u/me/x/graphs/g1/paths/p1": remote_record(1)},
            }},
        });
        std::fs::write(cfg.join(MANIFEST_FILE_NAME), legacy.to_string()).unwrap();
        evict_cache_id(cfg, "none").unwrap();
        assert_eq!(session(cfg, "s1")[PATH_URL], remote_record(5));
    }

    /// The manifest's parse error says to delete it; doing so loses no
    /// remote record.
    #[test]
    fn deleting_a_corrupt_manifest_keeps_remotes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(2)).unwrap();
        std::fs::write(cfg.join(MANIFEST_FILE_NAME), "not json").unwrap();
        let err = load_manifest(cfg).unwrap_err().to_string();
        assert!(err.contains("delete it"), "{err}");

        std::fs::remove_file(cfg.join(MANIFEST_FILE_NAME)).unwrap();
        evict_cache_id(cfg, "none").unwrap();
        assert_eq!(session(cfg, "s1")[PATH_URL], remote_record(2));
    }

    #[test]
    fn api_url_rewrites_only_the_pre_release_spelling() {
        assert_eq!(api_url("https://a/u/me/x/graphs/g1/paths/p1"), PATH_URL);
        assert_eq!(
            api_url("http://h:8080/base/u/me/x/graphs/g1/paths/p1"),
            "http://h:8080/base/api/v1/u/me/repos/x/graphs/g1/paths/p1"
        );
        assert_eq!(api_url(PATH_URL), PATH_URL);
        assert_eq!(api_url("s3://bucket/key"), "s3://bucket/key");
    }

    #[test]
    fn remote_record_serializes_as_the_spec_shape() {
        let json = serde_json::to_value(remote_record(412)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "steps": 412,
                "last_step": "s411",
                "head": "s411",
                "source_stamp": "2026-01-01T00:00:00Z/10",
            })
        );
    }

    #[test]
    fn stamp_string_leaves_absent_parts_empty() {
        let at: chrono::DateTime<chrono::Utc> = "2024-01-02T00:00:01.123456789Z".parse().unwrap();
        assert_eq!(
            stamp_string((Some(at), Some(7))),
            "2024-01-02T00:00:01.123456789Z/7"
        );
        assert_eq!(
            stamp_string((Some(at), None)),
            "2024-01-02T00:00:01.123456789Z/"
        );
        assert_eq!(stamp_string((None, Some(7))), "/7");
        assert_eq!(stamp_string((None, None)), "/");
    }

    #[test]
    fn remote_record_of_takes_the_path_tail_and_head() {
        let mut path = toolpath::v1::Path {
            path: toolpath::v1::PathIdentity {
                id: "p".into(),
                base: None,
                head: "s2".into(),
                graph_ref: None,
            },
            steps: Vec::new(),
            meta: None,
        };
        assert_eq!(RemoteRecord::of(&path, (None, None)), None);
        for id in ["s0", "s1", "s2"] {
            path.steps.push(toolpath::v1::Step {
                step: toolpath::v1::StepIdentity {
                    id: id.into(),
                    parents: Vec::new(),
                    actor: "a".into(),
                    timestamp: "2024-01-01T00:00:00Z".into(),
                },
                change: Default::default(),
                meta: None,
            });
        }
        let rec = RemoteRecord::of(&path, (None, Some(9))).unwrap();
        assert_eq!(rec.steps, 3);
        assert_eq!(rec.last_step, "s2");
        assert_eq!(rec.head, "s2");
        assert_eq!(rec.source_stamp, "/9");
    }
}
