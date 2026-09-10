//! Durable staging of a sync mutation before it is sent.
//!
//! A pending operation is written before the request goes out and
//! removed after the result is acknowledged in the manifest, so a lost
//! response or a crash is replayed with the same bytes and the same
//! idempotency key instead of minting a second graph.

use crate::artifact::ArtifactType;
use crate::config::PENDING_DIR_NAME;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OperationKind {
    Create,
    Update {
        graph_id: String,
    },
    Freeze {
        graph_id: String,
    },
    Continuation {
        source_graph_id: String,
        source_path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingOperation {
    /// The `Idempotency-Key`; also the file stem.
    pub(crate) key: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) base_url: String,
    /// `owner/name`.
    pub(crate) repo: String,
    #[serde(flatten)]
    pub(crate) kind: OperationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expected_generation: Option<i64>,
    #[serde(default)]
    pub(crate) freeze_after: bool,
    pub(crate) harness: ArtifactType,
    pub(crate) session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    /// Source stamp of the derive the body came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
    /// Owned step ids in the body; empty for a freeze.
    #[serde(default)]
    pub(crate) owned_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_from: Option<String>,
    pub(crate) body_sha256: String,
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn pending_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(PENDING_DIR_NAME)
}

fn record_path(config_dir: &Path, key: &str) -> PathBuf {
    pending_dir(config_dir).join(format!("{key}.json"))
}

fn body_path(config_dir: &Path, key: &str) -> PathBuf {
    pending_dir(config_dir).join(format!("{key}.body"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))
}

/// Write the body first, then the record that names it; a record on
/// disk always has its body. Fails if `key` is already staged.
pub(crate) fn stage(config_dir: &Path, op: &PendingOperation, body: &[u8]) -> Result<()> {
    if op.body_sha256 != sha256_hex(body) {
        bail!("staged body does not match its recorded digest");
    }
    let dir = pending_dir(config_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let record = record_path(config_dir, &op.key);
    if record.exists() {
        bail!("operation {} is already staged", op.key);
    }
    write_private(&body_path(config_dir, &op.key), body)?;
    write_private(&record, serde_json::to_string_pretty(op)?.as_bytes())
}

/// Every staged operation, oldest first.
pub(crate) fn list(config_dir: &Path) -> Result<Vec<PendingOperation>> {
    let dir = pending_dir(config_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut ops = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let op: PendingOperation =
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            ops.push(op);
        }
    }
    ops.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.key.cmp(&b.key)));
    Ok(ops)
}

/// The exact bytes staged for `op`, verified against its digest.
pub(crate) fn body(config_dir: &Path, op: &PendingOperation) -> Result<Vec<u8>> {
    let path = body_path(config_dir, &op.key);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if sha256_hex(&bytes) != op.body_sha256 {
        bail!(
            "staged body {} does not match the recorded digest; refusing to replay it",
            path.display()
        );
    }
    Ok(bytes)
}

/// Forget a staged operation. Record first, so a crash between the two
/// removals leaves an orphan body, never a record without a body.
pub(crate) fn retire(config_dir: &Path, key: &str) -> Result<()> {
    for path in [record_path(config_dir, key), body_path(config_dir, key)] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(key: &str, body: &[u8], secs: i64) -> PendingOperation {
        PendingOperation {
            key: key.into(),
            created_at: DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap(),
            base_url: "https://host".into(),
            repo: "me/pathstash".into(),
            kind: OperationKind::Update {
                graph_id: "g1".into(),
            },
            expected_generation: Some(3),
            freeze_after: false,
            harness: ArtifactType::Claude,
            session: "s1".into(),
            project: Some("/work".into()),
            modified: None,
            size: Some(1),
            owned_ids: vec!["a".into()],
            head: Some("a".into()),
            base_from: None,
            body_sha256: sha256_hex(body),
        }
    }

    #[test]
    fn stage_list_body_retire_roundtrip_in_creation_order() {
        let dir = tempfile::TempDir::new().unwrap();
        stage(dir.path(), &op("k2", b"two", 20), b"two").unwrap();
        stage(dir.path(), &op("k1", b"one", 10), b"one").unwrap();
        let ops = list(dir.path()).unwrap();
        assert_eq!(
            ops.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            ["k1", "k2"]
        );
        assert_eq!(body(dir.path(), &ops[0]).unwrap(), b"one");
        assert_eq!(ops[0], op("k1", b"one", 10));
        retire(dir.path(), "k1").unwrap();
        retire(dir.path(), "k1").unwrap();
        assert_eq!(list(dir.path()).unwrap().len(), 1);
        assert!(!dir.path().join("pending/k1.body").exists());
    }

    #[test]
    fn a_tampered_body_is_never_replayed() {
        let dir = tempfile::TempDir::new().unwrap();
        let o = op("k", b"payload", 0);
        stage(dir.path(), &o, b"payload").unwrap();
        std::fs::write(dir.path().join("pending/k.body"), b"other").unwrap();
        assert!(body(dir.path(), &o).is_err());
        assert!(stage(dir.path(), &o, b"mismatch").is_err());
        assert!(stage(dir.path(), &o, b"payload").is_err(), "duplicate key");
    }

    #[cfg(unix)]
    #[test]
    fn staged_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        stage(dir.path(), &op("k", b"x", 0), b"x").unwrap();
        for name in ["pending/k.json", "pending/k.body"] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
        let mode = std::fs::metadata(dir.path().join("pending"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn kind_is_tagged_on_the_wire() {
        let o = op("k", b"x", 0);
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(json["kind"], "update");
        assert_eq!(json["graph_id"], "g1");
        assert_eq!(json["harness"], "claude");
    }
}
