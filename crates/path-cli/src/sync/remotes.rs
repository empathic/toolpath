//! Where each session's path lives remotely: `$CONFIG_DIR/remotes.json`.
//!
//! The manifest is a cache index any sync can rebuild, and path-cli
//! builds before 0.30.0 rewrite it wholesale without fields they do not
//! know. These records are the one thing under the config dir nothing
//! can rebuild, so they live in their own file, written under the
//! manifest's lock.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::engine::{lock_manifest, write_config_file};
use super::sources;
use crate::artifact::ArtifactType;
use crate::config::{MANIFEST_FILE_NAME, REMOTES_FILE_NAME};
use crate::derive::PathRef;

/// The remotes file format this build reads and writes.
const REMOTES_VERSION: u32 = 1;

/// What one remote holds of a session's path. The remote's copy is a
/// prefix of the local derive: `steps` steps, the last of them
/// `last_step`. The key the record sits under is the path's resource
/// URL on that remote — on Pathbase its API URL,
/// `<server>/api/v1/u/<owner>/repos/<repo>/graphs/<graph>/paths/<path>`
/// (`cmd_pathbase::remote_path_url`) — so the containing graph is the
/// key without its last two segments and is not stored. `path share`
/// reads them to append to the remote path instead of creating another
/// (`share_append`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteRecord {
    /// How many steps of the derive the remote holds.
    pub(crate) steps: usize,
    /// Id of the derive's step `steps - 1`.
    pub(crate) last_step: String,
    /// The head the remote was last told.
    pub(crate) head: String,
    /// Whether the remote path carries a `path.parent`: it was created
    /// as a continuation of a frozen path, so some of the derive's steps
    /// are inherited from the chain above it rather than its own. The
    /// server stores a path's edges only among its own steps, so every
    /// batch appended to such a path keeps only parents among them.
    /// Which ids are owned and which inherited is asked of the server,
    /// never inferred from positions in the local derive: once a session
    /// has been truncated and regrown, a continuation's own steps need
    /// not be a contiguous suffix of it. Absent in records written
    /// before it existed, which are never continuations' records: false.
    #[serde(default)]
    pub(crate) has_parent: bool,
    /// The local source's stamp when those steps were sent; absent
    /// when the source provides neither part, so freshness cannot be
    /// told by stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_stamp: Option<SourceStamp>,
}

/// A source's stat-level stamp as the manifest record stores it:
/// mtime or updated-at, and size, each absent when the source does
/// not provide it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceStamp {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
}

impl SourceStamp {
    /// `stamp` as a record field: `None` when both parts are unknown.
    pub(crate) fn known((modified, size): sources::Stamp) -> Option<Self> {
        (modified.is_some() || size.is_some()).then_some(Self { modified, size })
    }
}

/// One session's records: path URL → record.
pub(crate) type SessionRemotes = BTreeMap<String, RemoteRecord>;

/// The remotes file: `{ "version": 1, "sessions": { <type>: { <id>:
/// { <path URL>: record } } } }`, sessions addressed as the manifest
/// addresses them, by artifact type name and then id.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Remotes {
    pub(crate) version: u32,
    pub(crate) sessions: BTreeMap<String, BTreeMap<String, SessionRemotes>>,
}

impl Default for Remotes {
    fn default() -> Self {
        Self {
            version: REMOTES_VERSION,
            sessions: BTreeMap::new(),
        }
    }
}

impl Remotes {
    /// The session's records, on every destination.
    pub(crate) fn of(
        &self,
        artifact_type: ArtifactType,
        id: &str,
    ) -> impl Iterator<Item = &RemoteRecord> {
        self.sessions
            .get(artifact_type.name())
            .and_then(|sessions| sessions.get(id))
            .into_iter()
            .flat_map(|records| records.values())
    }

    /// The session's record on the destination (server, owner, repo) of
    /// `probe`, any path API URL there, with the key it sits under.
    pub(crate) fn at(
        &self,
        artifact_type: ArtifactType,
        id: &str,
        probe: &str,
    ) -> Option<(&str, &RemoteRecord)> {
        let dest = destination(probe)?;
        self.sessions
            .get(artifact_type.name())?
            .get(id)?
            .iter()
            .find(|(key, _)| destination(key).as_ref() == Some(&dest))
            .map(|(key, record)| (key.as_str(), record))
    }
}

/// Write `record` under `url` for the session (`artifact_type`, `id`).
/// A session keeps one record per destination: any earlier record at
/// that URL, or at another path on the same server, owner, and repo, is
/// replaced.
pub(crate) fn record_remote(
    config_dir: &Path,
    artifact_type: ArtifactType,
    id: &str,
    url: &str,
    record: RemoteRecord,
) -> Result<()> {
    let _lock = lock_manifest(config_dir)?;
    let mut remotes = load_remotes(config_dir)?;
    let session = remotes
        .sessions
        .entry(artifact_type.name().to_string())
        .or_default()
        .entry(id.to_string())
        .or_default();
    let dest = destination(url);
    if dest.is_some() {
        session.retain(|key, _| destination(key) != dest);
    }
    session.insert(url.to_string(), record);
    save_remotes(config_dir, &remotes)
}

/// Where a record key points: the server (scheme and host), owner, and
/// repo of a Pathbase path API URL. `None` for any other key, which
/// shares a destination with nothing.
fn destination(key: &str) -> Option<(String, String, String)> {
    match crate::derive::parse_pathbase_ref(key, None).ok()? {
        (
            Some(server),
            PathRef {
                owner,
                repo,
                path: Some(_),
                ..
            },
        ) => Some((server, owner, repo)),
        _ => None,
    }
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
/// remotes file, rekeyed to the path's API URL and with the stamp
/// typed. A record already in the remotes file for that destination
/// wins; among several pre-release records for one destination, which
/// nothing orders, the first by URL is kept. The caller holds the manifest lock
/// and saves the manifest next, which drops the field. An unreadable
/// manifest is left for `load_manifest` to report.
pub(super) fn lift_from_manifest(config_dir: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct LegacyRecord {
        #[serde(default)]
        remotes: BTreeMap<String, LegacyRemote>,
    }
    #[derive(Deserialize)]
    struct LegacyRemote {
        steps: usize,
        last_step: String,
        head: String,
        /// `<modified>/<size>`, each part empty when unknown.
        source_stamp: String,
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
        for (id, rec) in records {
            for (url, old) in rec.remotes {
                let remote = RemoteRecord {
                    steps: old.steps,
                    last_step: old.last_step,
                    head: old.head,
                    has_parent: false,
                    source_stamp: parse_stamp_string(&old.source_stamp),
                };
                lifted.push((name.clone(), id.clone(), api_url(&url), remote));
            }
        }
    }
    if lifted.is_empty() {
        return Ok(());
    }
    let mut remotes = load_remotes(config_dir)?;
    for (name, id, url, remote) in lifted {
        let session = remotes
            .sessions
            .entry(name)
            .or_default()
            .entry(id)
            .or_default();
        let dest = destination(&url);
        let taken = session
            .keys()
            .any(|k| *k == url || (dest.is_some() && destination(k) == dest));
        if !taken {
            session.insert(url, remote);
        }
    }
    save_remotes(config_dir, &remotes)
}

/// A pre-release `<modified>/<size>` stamp string, typed. A part that
/// is empty or does not parse is unknown.
fn parse_stamp_string(stamp: &str) -> Option<SourceStamp> {
    let (modified, size) = stamp.rsplit_once('/')?;
    SourceStamp::known((modified.parse().ok(), size.parse().ok()))
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

    const G: &str = "00000000-0000-0000-0000-00000000000a";
    const P: &str = "00000000-0000-0000-0000-00000000000b";
    const PATH_URL: &str = "https://a/api/v1/u/me/repos/x/graphs/00000000-0000-0000-0000-00000000000a/paths/00000000-0000-0000-0000-00000000000b";
    /// [`PATH_URL`] in the pre-release spelling.
    const LEGACY_KEY: &str = "https://a/u/me/x/graphs/00000000-0000-0000-0000-00000000000a/paths/00000000-0000-0000-0000-00000000000b";

    /// A path API URL on `server`, repo `me/<repo>`, ids ending in `n`.
    fn path_url(server: &str, repo: &str, n: u8) -> String {
        format!(
            "https://{server}/api/v1/u/me/repos/{repo}/graphs/00000000-0000-0000-0000-0000000000a{n}/paths/00000000-0000-0000-0000-0000000000b{n}"
        )
    }

    fn remote_record(steps: usize) -> RemoteRecord {
        RemoteRecord {
            steps,
            last_step: format!("s{}", steps - 1),
            head: format!("s{}", steps - 1),
            has_parent: false,
            source_stamp: Some(SourceStamp {
                modified: Some("2026-01-01T00:00:00Z".parse().unwrap()),
                size: Some(10),
            }),
        }
    }

    fn session(config_dir: &Path, id: &str) -> SessionRemotes {
        load_remotes(config_dir)
            .unwrap()
            .sessions
            .get_mut("claude")
            .and_then(|sessions| sessions.remove(id))
            .unwrap_or_default()
    }

    /// What a pre-0.30.0 path-cli writes back after its own manifest
    /// update: the records it knows, nothing else.
    const OLD_MANIFEST: &str = r#"{"claude":{"s1":{"path":"/p","cache_id":"claude-s1","size":4096,"synced_at":"2026-07-09T00:00:00Z"}}}"#;

    #[test]
    fn record_remote_writes_the_versioned_file_and_replaces_per_url() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let other = &path_url("a", "y", 2);
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(1)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", other, remote_record(2)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", PATH_URL, remote_record(3)).unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cfg.join(REMOTES_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["sessions"]["claude"]["s1"][PATH_URL]["steps"], 3);
        assert_eq!(json["sessions"]["claude"]["s1"][other]["steps"], 2);
        assert!(
            !cfg.join(MANIFEST_FILE_NAME).exists(),
            "a record does not touch the manifest"
        );
    }

    /// Two shares of one session to one destination leave the second;
    /// another repo or server is a separate destination.
    #[test]
    fn record_remote_keeps_one_record_per_destination() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let first = &path_url("a", "x", 1);
        let second = &path_url("a", "x", 2);
        let other_repo = &path_url("a", "y", 3);
        let other_server = &path_url("b", "x", 4);
        record_remote(cfg, ArtifactType::Claude, "s1", first, remote_record(1)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", second, remote_record(2)).unwrap();
        assert_eq!(
            session(cfg, "s1"),
            [(second.to_string(), remote_record(2))].into()
        );

        record_remote(
            cfg,
            ArtifactType::Claude,
            "s1",
            other_repo,
            remote_record(3),
        )
        .unwrap();
        record_remote(
            cfg,
            ArtifactType::Claude,
            "s1",
            other_server,
            remote_record(4),
        )
        .unwrap();
        let keys: Vec<String> = session(cfg, "s1").into_keys().collect();
        assert_eq!(keys, [second, other_repo, other_server].map(String::clone));
        assert!(
            session(cfg, "s2").is_empty(),
            "other sessions are untouched"
        );
    }

    #[test]
    fn at_finds_the_record_on_the_probed_destination_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let x = &path_url("a", "x", 1);
        let y = &path_url("a", "y", 2);
        record_remote(cfg, ArtifactType::Claude, "s1", x, remote_record(1)).unwrap();
        record_remote(cfg, ArtifactType::Claude, "s1", y, remote_record(2)).unwrap();
        let remotes = load_remotes(cfg).unwrap();

        let probe_x = path_url("a", "x", 9);
        assert_eq!(
            remotes.at(ArtifactType::Claude, "s1", &probe_x),
            Some((x.as_str(), &remote_record(1)))
        );
        assert_eq!(
            remotes.at(ArtifactType::Claude, "s1", &path_url("a", "y", 9)),
            Some((y.as_str(), &remote_record(2)))
        );
        assert_eq!(
            remotes.at(ArtifactType::Claude, "s1", &path_url("b", "x", 9)),
            None
        );
        assert_eq!(remotes.at(ArtifactType::Claude, "s2", &probe_x), None);
        assert_eq!(remotes.at(ArtifactType::Codex, "s1", &probe_x), None);
        assert_eq!(
            remotes.at(ArtifactType::Claude, "s1", "s3://bucket/key"),
            None
        );
    }

    #[test]
    fn destination_is_server_owner_and_repo_of_a_path_api_url() {
        let dest = |s: &str, o: &str, r: &str| Some((s.into(), o.into(), r.into()));
        assert_eq!(destination(PATH_URL), dest("https://a", "me", "x"));
        assert_eq!(
            destination(&format!("https://a/u/me/repos/x/graphs/{G}")),
            None
        );
        assert_eq!(destination(LEGACY_KEY), None);
        assert_eq!(destination("s3://bucket/key"), None);
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
                    LEGACY_KEY: {
                        "steps": 4,
                        "last_step": "s3",
                        "head": "s3",
                        "source_stamp": "2026-01-01T00:00:00Z/10",
                    },
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

    /// A lifted record does not join or replace a newer record for the
    /// same destination.
    #[test]
    fn a_lifted_record_does_not_replace_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path();
        let newer = path_url("a", "x", 2);
        record_remote(cfg, ArtifactType::Claude, "s1", &newer, remote_record(5)).unwrap();
        let legacy = serde_json::json!({
            "claude": {"s1": {
                "synced_at": "2026-07-09T00:00:00Z",
                "remotes": {LEGACY_KEY: {
                    "steps": 1,
                    "last_step": "s0",
                    "head": "s0",
                    "source_stamp": "/",
                }},
            }},
        });
        std::fs::write(cfg.join(MANIFEST_FILE_NAME), legacy.to_string()).unwrap();
        evict_cache_id(cfg, "none").unwrap();
        assert_eq!(session(cfg, "s1"), [(newer, remote_record(5))].into());
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
        assert_eq!(api_url(LEGACY_KEY), PATH_URL);
        assert_eq!(
            api_url(&format!("http://h:8080/base/u/me/x/graphs/{G}/paths/{P}")),
            format!("http://h:8080/base/api/v1/u/me/repos/x/graphs/{G}/paths/{P}")
        );
        assert_eq!(api_url(PATH_URL), PATH_URL);
        assert_eq!(api_url("s3://bucket/key"), "s3://bucket/key");
    }

    #[test]
    fn remote_record_serializes_as_the_spec_shape() {
        let record = RemoteRecord {
            has_parent: true,
            ..remote_record(412)
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "steps": 412,
                "last_step": "s411",
                "head": "s411",
                "has_parent": true,
                "source_stamp": {"modified": "2026-01-01T00:00:00Z", "size": 10},
            })
        );
        assert_eq!(
            serde_json::from_value::<RemoteRecord>(json).unwrap(),
            record
        );
    }

    /// A record written before `has_parent` existed is a fresh path's.
    #[test]
    fn a_record_without_has_parent_has_no_parent() {
        let old = serde_json::json!({"steps": 3, "last_step": "s2", "head": "s2"});
        let record: RemoteRecord = serde_json::from_value(old).unwrap();
        assert!(!record.has_parent);
    }

    /// A stamp with neither part is left out of the record and reads
    /// back as unknown; a partial one keeps only the part it has.
    #[test]
    fn an_unknown_stamp_is_absent() {
        let unknown = RemoteRecord {
            source_stamp: SourceStamp::known((None, None)),
            ..remote_record(1)
        };
        let json = serde_json::to_value(&unknown).unwrap();
        assert_eq!(json.get("source_stamp"), None, "{json}");
        let back: RemoteRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.source_stamp, None);

        let partial = RemoteRecord {
            source_stamp: SourceStamp::known((None, Some(7))),
            ..remote_record(1)
        };
        assert_eq!(
            serde_json::to_value(&partial).unwrap()["source_stamp"],
            serde_json::json!({"size": 7})
        );
    }

    #[test]
    fn parse_stamp_string_reads_each_part_or_none() {
        let at: DateTime<Utc> = "2024-01-02T00:00:01.123456789Z".parse().unwrap();
        assert_eq!(
            parse_stamp_string("2024-01-02T00:00:01.123456789Z/7"),
            SourceStamp::known((Some(at), Some(7)))
        );
        assert_eq!(
            parse_stamp_string("/7"),
            SourceStamp::known((None, Some(7)))
        );
        assert_eq!(parse_stamp_string("/"), None);
        assert_eq!(parse_stamp_string("garbage"), None);
    }
}
