//! On-disk cache for toolpath documents at `$CONFIG_DIR/documents/`.
//!
//! `path import` and `path export` both use this as the pivot between
//! external formats and toolpath JSON. Users refer to cached documents
//! by a short id (filename without `.json`) instead of full paths.
//! The `path cache ls | rm` subcommands make the directory legible.

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::Document;

use crate::config::config_dir;

const DOCUMENTS_DIR: &str = "documents";

#[derive(Subcommand, Debug)]
pub enum CacheOp {
    /// List cached documents (newest first)
    Ls,
    /// Remove a cached document by id
    Rm {
        /// Cache id (filename without `.json`)
        id: String,
    },
}

pub fn run(op: CacheOp) -> Result<()> {
    match op {
        CacheOp::Ls => run_ls(),
        CacheOp::Rm { id } => run_rm(&id),
    }
}

fn run_ls() -> Result<()> {
    let entries = list_cached()?;
    if entries.is_empty() {
        eprintln!("No cached documents. Run `path import <source>` to create one.");
        return Ok(());
    }
    for e in entries {
        println!("{}\t{}\t{}", e.id, e.bytes, e.path.display());
    }
    Ok(())
}

fn run_rm(id: &str) -> Result<()> {
    remove_cached(id)?;
    eprintln!("Removed {id}");
    Ok(())
}

/// An entry surfaced by `list_cached`.
#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: std::time::SystemTime,
}

/// The cache directory: `$CONFIG_DIR/documents/`.
pub(crate) fn cache_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(DOCUMENTS_DIR))
}

/// Path for a given cache id (does not check existence).
pub(crate) fn cache_path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.ends_with(".json") {
        bail!("invalid cache id: {id:?}");
    }
    Ok(cache_dir()?.join(format!("{id}.json")))
}

/// Write a toolpath document to the cache under `id`. Errors if the
/// file already exists unless `force` is true.
///
/// Uses `O_CREAT | O_EXCL` (`create_new`) when `force == false` so the
/// exists-check and the write are atomic — two concurrent `path import`
/// invocations racing the same id can't silently stomp each other.
pub(crate) fn write_cached(id: &str, doc: &Document, force: bool) -> Result<PathBuf> {
    use std::io::Write;

    let dir = cache_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let path = cache_path(id)?;
    let json = doc.to_json_pretty()?;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).truncate(true);
    if force {
        opts.create(true);
    } else {
        opts.create_new(true);
    }

    let mut file = match opts.open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "cache entry {id} already exists at {}; pass --force to overwrite",
                path.display()
            );
        }
        Err(e) => {
            return Err(anyhow!("open {}: {e}", path.display()));
        }
    };
    file.write_all(json.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
    }
    Ok(path)
}

/// Resolve a `<ref>` string to a filesystem path. A ref is either a
/// bare cache id (looks up `$CACHE_DIR/<ref>.json`) or a file path
/// (contains `/` or `\\`, or ends with `.json`).
pub(crate) fn cache_ref(s: &str) -> Result<PathBuf> {
    if s.contains('/') || s.contains('\\') || s.ends_with(".json") {
        let p = PathBuf::from(s);
        if !p.exists() {
            bail!(
                "file not found: {}; if you meant a cache id, drop the path/extension and run `path cache ls`",
                p.display()
            );
        }
        return Ok(p);
    }
    let p = cache_path(s)?;
    if !p.exists() {
        bail!(
            "cache entry {s} not found at {}; run `path cache ls` to see what's cached",
            p.display()
        );
    }
    Ok(p)
}

pub(crate) fn list_cached() -> Result<Vec<CacheEntry>> {
    let dir = cache_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let meta = entry.metadata()?;
        out.push(CacheEntry {
            id,
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(out)
}

pub(crate) fn remove_cached(id: &str) -> Result<()> {
    let path = cache_path(id)?;
    if !path.exists() {
        return Err(anyhow!("cache entry {id} not found"));
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(())
}

/// Build a cache id for a given source + inner id.
///
/// Sanitizes `/` and other filesystem-unfriendly characters in the
/// inner id to `_` so (e.g.) git branch names land cleanly. Also strips
/// a trailing `.json` so the result never collides with the cache's
/// file extension (see [`cache_path`]).
pub(crate) fn make_id(source: &str, inner: &str) -> String {
    let trimmed = inner.trim_end_matches(".json");
    let safe: String = trimmed
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | ' ' | '\t' => '_',
            c => c,
        })
        .collect();
    format!("{source}-{safe}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};

    fn with_cfg<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
        let temp = tempfile::tempdir().unwrap();
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path());
        }
        let result = f(temp.path());
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        result
    }

    fn sample_doc() -> Document {
        Document::Step(toolpath::v1::Step::new(
            "s1",
            "human:alex",
            "2026-01-01T00:00:00Z",
        ))
    }

    #[test]
    fn write_and_read_cache_entry() {
        with_cfg(|_| {
            let doc = sample_doc();
            let p = write_cached("claude-abc", &doc, false).unwrap();
            assert!(p.exists());
            assert_eq!(p.file_name().unwrap(), "claude-abc.json");
        });
    }

    #[test]
    fn write_errors_if_exists_without_force() {
        with_cfg(|_| {
            let doc = sample_doc();
            write_cached("claude-abc", &doc, false).unwrap();
            let err = write_cached("claude-abc", &doc, false).unwrap_err();
            assert!(err.to_string().contains("already exists"));
        });
    }

    #[test]
    fn write_force_overwrites() {
        with_cfg(|_| {
            let doc = sample_doc();
            write_cached("claude-abc", &doc, false).unwrap();
            write_cached("claude-abc", &doc, true).unwrap();
        });
    }

    #[test]
    fn cache_ref_finds_existing_cache_entry() {
        with_cfg(|_| {
            let doc = sample_doc();
            let p = write_cached("claude-abc", &doc, false).unwrap();
            let resolved = cache_ref("claude-abc").unwrap();
            assert_eq!(resolved, p);
        });
    }

    #[test]
    fn cache_ref_returns_file_path_unchanged() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{}").unwrap();
        let resolved = cache_ref(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn cache_ref_errors_on_missing_id() {
        with_cfg(|_| {
            let err = cache_ref("does-not-exist").unwrap_err();
            assert!(err.to_string().contains("not found"));
        });
    }

    #[test]
    fn cache_path_rejects_slashes_and_json_suffix() {
        assert!(cache_path("foo/bar").is_err());
        assert!(cache_path("foo.json").is_err());
        assert!(cache_path("").is_err());
    }

    #[test]
    fn list_empty_when_dir_missing() {
        with_cfg(|_| {
            assert!(list_cached().unwrap().is_empty());
        });
    }

    #[test]
    fn list_and_remove_roundtrip() {
        with_cfg(|_| {
            let doc = sample_doc();
            write_cached("a", &doc, false).unwrap();
            write_cached("b", &doc, false).unwrap();
            let entries = list_cached().unwrap();
            assert_eq!(entries.len(), 2);

            remove_cached("a").unwrap();
            let entries = list_cached().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, "b");

            assert!(remove_cached("a").is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn writes_file_with_0600() {
        use std::os::unix::fs::PermissionsExt;
        with_cfg(|_| {
            let p = write_cached("claude-abc", &sample_doc(), false).unwrap();
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        });
    }

    #[test]
    fn make_id_sanitizes_slashes() {
        assert_eq!(make_id("git", "main"), "git-main");
        assert_eq!(make_id("git", "feature/x"), "git-feature_x");
        assert_eq!(make_id("pathbase", "trc_01H"), "pathbase-trc_01H");
    }

    #[test]
    fn make_id_strips_trailing_json() {
        assert_eq!(make_id("pathbase", "trc_01H.json"), "pathbase-trc_01H");
        assert_eq!(make_id("git", "path-main.json"), "git-path-main");
    }

    #[test]
    fn make_id_result_survives_cache_path() {
        // Regression: make_id output must be accepted by cache_path.
        let id = make_id("pathbase", "trc_01H.json");
        assert!(cache_path(&id).is_ok());
    }
}
