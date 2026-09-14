//! What left this machine, to where, when, and as whom.
//!
//! `~/.toolpath/exports.json` maps destination → cache ID → the last
//! upload's URI, SHA-256, size, time, and uploader. Two jobs: bulk
//! export skips a document whose bytes already landed at that
//! destination, and anyone auditing egress from this machine has a
//! local record without asking the bucket.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExportRecord {
    pub uri: String,
    pub sha256: String,
    pub bytes: u64,
    pub uploaded_at: chrono::DateTime<chrono::Utc>,
    pub uploader: String,
}

/// destination → cache ID → last upload. `BTreeMap`s so the file on
/// disk is stably ordered.
pub(crate) type Ledger = BTreeMap<String, BTreeMap<String, ExportRecord>>;

pub(crate) fn ledger_path() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join(crate::config::EXPORTS_FILE_NAME))
}

pub(crate) fn load(path: &Path) -> Result<Ledger> {
    Ok(crate::config::read_private_json(path)?.unwrap_or_default())
}

/// Insert one record and write the ledger back. Temp-and-rename so a
/// crash mid-write leaves the previous ledger intact; 0600 because
/// URIs and uploader names are nobody else's business.
pub(crate) fn record(
    path: &Path,
    destination: &str,
    cache_id: &str,
    rec: ExportRecord,
) -> Result<()> {
    let mut ledger = load(path)?;
    ledger
        .entry(destination.to_string())
        .or_default()
        .insert(cache_id.to_string(), rec);

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ledger path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        crate::config::EXPORTS_FILE_NAME,
        std::process::id()
    ));
    std::fs::write(&tmp, serde_json::to_string_pretty(&ledger)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Not yet called from `cmd_export`: skip-on-unchanged wiring lands
/// with the shared `export_body` path.
#[allow(dead_code)]
pub(crate) fn unchanged(ledger: &Ledger, destination: &str, cache_id: &str, sha256: &str) -> bool {
    ledger
        .get(destination)
        .and_then(|m| m.get(cache_id))
        .is_some_and(|r| r.sha256 == sha256)
}

pub(crate) fn sha256_hex(body: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(body))
}

/// `<user>@<host>` from the environment, falling back to `hostname(1)`
/// and then to `unknown`. Attribution, not authentication — the bucket's
/// own access log is the authoritative record of the principal.
pub(crate) fn uploader() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    format!("{user}@{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sha: &str) -> ExportRecord {
        ExportRecord {
            uri: "s3://b/2026-01-01-hello--g1.json".to_string(),
            sha256: sha.to_string(),
            bytes: 3,
            uploaded_at: chrono::Utc::now(),
            uploader: "alex@laptop".to_string(),
        }
    }

    #[test]
    fn a_missing_ledger_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("exports.json")).unwrap().is_empty());
    }

    #[test]
    fn records_accumulate_per_destination_and_cache_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports.json");
        record(&path, "s3://b/traces", "claude-a", rec("aaa")).unwrap();
        record(&path, "s3://b/traces", "claude-b", rec("bbb")).unwrap();
        record(&path, "/srv/traces", "claude-a", rec("ccc")).unwrap();

        let ledger = load(&path).unwrap();
        assert_eq!(ledger["s3://b/traces"]["claude-a"].sha256, "aaa");
        assert_eq!(ledger["s3://b/traces"]["claude-b"].sha256, "bbb");
        assert_eq!(ledger["/srv/traces"]["claude-a"].sha256, "ccc");

        // A re-export replaces the entry.
        record(&path, "s3://b/traces", "claude-a", rec("ddd")).unwrap();
        assert_eq!(
            load(&path).unwrap()["s3://b/traces"]["claude-a"].sha256,
            "ddd"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn unchanged_matches_on_destination_cache_id_and_sha() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports.json");
        record(&path, "s3://b/traces", "claude-a", rec("aaa")).unwrap();
        let ledger = load(&path).unwrap();
        assert!(unchanged(&ledger, "s3://b/traces", "claude-a", "aaa"));
        assert!(!unchanged(&ledger, "s3://b/traces", "claude-a", "zzz"));
        assert!(!unchanged(&ledger, "s3://b/traces", "claude-z", "aaa"));
        assert!(!unchanged(&ledger, "s3://other", "claude-a", "aaa"));
    }

    #[test]
    fn sha256_hex_is_lowercase_hex_of_the_body() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn uploader_has_a_user_and_a_host() {
        let who = uploader();
        assert!(who.contains('@'), "{who}");
        assert!(!who.starts_with('@') && !who.ends_with('@'), "{who}");
    }
}
