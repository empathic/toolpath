//! On-disk cache for toolpath documents at `$CONFIG_DIR/documents/`.
//!
//! `path p import` and `path p export` both use this as the pivot
//! between external formats and toolpath JSON. Users refer to cached
//! documents by a short id (filename without `.json`) instead of full
//! paths. The `p cache ls | rm` subcommands make the directory legible.

use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use toolpath::v1::Graph;

use std::path::Path;

/// An entry surfaced by `list_cached`.
#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: std::time::SystemTime,
}

/// The cache directory: `$CONFIG_DIR/documents/`.
pub(crate) fn cache_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(crate::config::DOCUMENTS_DIR_NAME)
}

/// Path for a given cache id (does not check existence).
pub(crate) fn cache_path(config_dir: &Path, id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.ends_with(".json") {
        bail!("invalid cache id: {id:?}");
    }
    Ok(cache_dir(config_dir).join(format!("{id}.json")))
}

/// Write a toolpath document to the cache under `id`. Errors if the
/// file already exists unless `force` is true.
///
/// Uses `O_CREAT | O_EXCL` (`create_new`) when `force == false` so the
/// exists-check and the write are atomic — two concurrent `path import`
/// invocations racing the same id can't silently stomp each other.
pub(crate) fn write_cached(
    config_dir: &Path,
    id: &str,
    doc: &Graph,
    force: bool,
) -> Result<PathBuf> {
    use std::io::Write;

    let dir = cache_dir(config_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let path = cache_path(config_dir, id)?;
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
pub(crate) fn cache_ref(config_dir: &Path, s: &str) -> Result<PathBuf> {
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
    let p = cache_path(config_dir, s)?;
    if !p.exists() {
        bail!(
            "cache entry {s} not found at {}; run `path cache ls` to see what's cached",
            p.display()
        );
    }
    Ok(p)
}

pub(crate) fn list_cached(config_dir: &Path) -> Result<Vec<CacheEntry>> {
    let dir = cache_dir(config_dir);
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

pub(crate) fn remove_cached(config_dir: &Path, id: &str) -> Result<()> {
    let path = cache_path(config_dir, id)?;
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

/// The cache id a Pathbase download lands at:
/// `pathbase-<owner>-<repo>-<uuid>`.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn pathbase_cache_id(owner: &str, repo: &str, id: &str) -> String {
    make_id("pathbase", &format!("{owner}-{repo}-{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config directory in a fresh tempdir. Dropping the `TempDir`
    /// removes it.
    fn config_dir_in_tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_doc() -> Graph {
        Graph::new("g-sample")
    }

    #[test]
    fn write_and_read_cache_entry() {
        let temp = config_dir_in_tempdir();
        let doc = sample_doc();
        let p = write_cached(temp.path(), "claude-abc", &doc, false).unwrap();
        assert!(p.exists());
        assert_eq!(p.file_name().unwrap(), "claude-abc.json");
    }

    #[test]
    fn write_errors_if_exists_without_force() {
        let temp = config_dir_in_tempdir();
        let doc = sample_doc();
        write_cached(temp.path(), "claude-abc", &doc, false).unwrap();
        let err = write_cached(temp.path(), "claude-abc", &doc, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn write_force_overwrites() {
        let temp = config_dir_in_tempdir();
        let doc = sample_doc();
        write_cached(temp.path(), "claude-abc", &doc, false).unwrap();
        write_cached(temp.path(), "claude-abc", &doc, true).unwrap();
    }

    #[test]
    fn cache_ref_finds_existing_cache_entry() {
        let temp = config_dir_in_tempdir();
        let doc = sample_doc();
        let p = write_cached(temp.path(), "claude-abc", &doc, false).unwrap();
        let resolved = cache_ref(temp.path(), "claude-abc").unwrap();
        assert_eq!(resolved, p);
    }

    #[test]
    fn cache_ref_returns_file_path_unchanged() {
        let temp = config_dir_in_tempdir();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{}").unwrap();
        let resolved = cache_ref(temp.path(), tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn cache_ref_errors_on_missing_id() {
        let temp = config_dir_in_tempdir();
        let err = cache_ref(temp.path(), "does-not-exist").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn cache_path_rejects_slashes_and_json_suffix() {
        let temp = config_dir_in_tempdir();
        assert!(cache_path(temp.path(), "foo/bar").is_err());
        assert!(cache_path(temp.path(), "foo.json").is_err());
        assert!(cache_path(temp.path(), "").is_err());
    }

    #[test]
    fn list_empty_when_dir_missing() {
        let temp = config_dir_in_tempdir();
        assert!(list_cached(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn list_and_remove_roundtrip() {
        let temp = config_dir_in_tempdir();
        let doc = sample_doc();
        write_cached(temp.path(), "a", &doc, false).unwrap();
        write_cached(temp.path(), "b", &doc, false).unwrap();
        let entries = list_cached(temp.path()).unwrap();
        assert_eq!(entries.len(), 2);

        remove_cached(temp.path(), "a").unwrap();
        let entries = list_cached(temp.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "b");

        assert!(remove_cached(temp.path(), "a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn writes_file_with_0600() {
        use std::os::unix::fs::PermissionsExt;
        let temp = config_dir_in_tempdir();
        let p = write_cached(temp.path(), "claude-abc", &sample_doc(), false).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
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
        let temp = config_dir_in_tempdir();
        let id = make_id("pathbase", "trc_01H.json");
        assert!(cache_path(temp.path(), &id).is_ok());
    }
}
