//! On-disk cache for toolpath documents at `$CONFIG_DIR/documents/`.
//!
//! `path p import` and `path p export` both use this as the pivot
//! between external formats and toolpath JSON. Users refer to cached
//! documents by a short id (filename without `.json`) instead of full
//! paths. The `p cache ls | rm` subcommands make the directory legible.
//!
//! With the `cache-index` feature, the document index (the `index`
//! module) holds the facts of each document that a listing reads, and
//! this module records each document it writes or removes.

use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use toolpath::v1::Graph;

use crate::config::config_dir;

#[cfg(all(test, not(target_os = "emscripten")))]
pub(crate) mod fixtures;
#[cfg(all(feature = "cache-index", not(target_os = "emscripten")))]
pub(crate) mod index;
/// The build has no document index. A write to the cache has nothing
/// to record.
#[cfg(not(all(feature = "cache-index", not(target_os = "emscripten"))))]
mod index {
    use super::CacheEntry;
    use anyhow::Result;
    use std::path::Path;
    use toolpath::v1::Graph;

    pub(crate) struct IndexWriter;

    impl IndexWriter {
        pub(crate) fn new(_: &Path) -> Self {
            Self
        }

        pub(crate) fn record(&mut self, _: &CacheEntry, _: &Graph) -> Result<()> {
            Ok(())
        }
    }

    pub(crate) fn forget_removed(_: &Path, _: &str) -> Result<()> {
        Ok(())
    }
}
pub(crate) use index::IndexWriter;
#[cfg(not(target_os = "emscripten"))]
mod summary;
#[cfg(not(target_os = "emscripten"))]
pub(crate) use summary::SessionSummary;

/// A document file in the cache: an entry of `list_cached`, or the
/// file `write_cached` wrote.
#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: std::time::SystemTime,
}

impl CacheEntry {
    /// The entry of the document `id` at `path`, with the size and
    /// mtime that `meta` holds.
    fn new(id: String, path: PathBuf, meta: &std::fs::Metadata) -> Self {
        Self {
            id,
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        }
    }
}

/// What [`write_cached`] did.
#[derive(Debug)]
#[must_use = "`index_error` says whether the document index holds the document"]
pub(crate) struct CacheWrite {
    /// The document file.
    pub path: PathBuf,
    /// The failure to record the document in the document index. The
    /// document is in the cache either way, and the next reader of
    /// the index parses it.
    pub index_error: Option<anyhow::Error>,
}

/// The cache directory: `$CONFIG_DIR/documents/`.
pub(crate) fn cache_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join(crate::config::DOCUMENTS_DIR_NAME))
}

/// Path for a given cache id (does not check existence).
pub(crate) fn cache_path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.ends_with(".json") {
        bail!("invalid cache id: {id:?}");
    }
    Ok(cache_dir()?.join(format!("{id}.json")))
}

/// Write a toolpath document to the cache under `id`, and record it
/// in the document index. Errors if the file already exists unless
/// `force` is true. A failure of the index is not an error of the
/// write: it comes back in [`CacheWrite::index_error`], and the caller
/// decides what to report.
///
/// Uses `O_CREAT | O_EXCL` (`create_new`) when `force == false` so the
/// exists-check and the write are atomic — two concurrent `path import`
/// invocations racing the same id can't silently stomp each other.
pub(crate) fn write_cached(id: &str, doc: &Graph, force: bool) -> Result<CacheWrite> {
    write_cached_with_index(&mut IndexWriter::new(&config_dir()?), id, doc, force)
}

/// Does what [`write_cached`] does, and records the document through
/// `index`. A caller that writes many documents keeps one
/// [`IndexWriter`] for all of them, so the index opens once.
pub(crate) fn write_cached_with_index(
    index: &mut IndexWriter,
    id: &str,
    doc: &Graph,
    force: bool,
) -> Result<CacheWrite> {
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

    // Another writer of the same id can rewrite the file between the
    // write above and this stat. The index then holds the rows of this
    // document under the stamp of the other one, until the file changes.
    let meta = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let written = CacheEntry::new(id.to_string(), path.clone(), &meta);
    let index_error = index.record(&written, doc).err();
    Ok(CacheWrite { path, index_error })
}

/// Resolve a `<ref>` string to a filesystem path. A ref is either a
/// bare cache id (looks up `$CACHE_DIR/<ref>.json`) or a file path
/// (contains `/` or `\\`, or ends with `.json`).
pub(crate) fn cache_ref(s: &str) -> Result<PathBuf> {
    if s.contains('/') || s.contains('\\') || s.ends_with(".json") {
        let p = PathBuf::from(s);
        if !p.exists() {
            bail!(
                "file not found: {}; if you meant a cache id, drop the path/extension and run `path p cache ls`",
                p.display()
            );
        }
        return Ok(p);
    }
    let p = cache_path(s)?;
    if !p.exists() {
        bail!(
            "cache entry {s} not found at {}; run `path p cache ls` to see what's cached",
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
        out.push(CacheEntry::new(id, path, &entry.metadata()?));
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.modified));
    Ok(out)
}

/// Removes the document `id` from the cache, and its rows from the
/// document index. Returns the failure to update the index, if any:
/// the document is removed either way, and the caller decides what to
/// report.
pub(crate) fn remove_cached(id: &str) -> Result<Option<anyhow::Error>> {
    let path = cache_path(id)?;
    if !path.exists() {
        return Err(anyhow!("cache entry {id} not found"));
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(config_dir()
        .and_then(|dir| index::forget_removed(&dir, id))
        .err())
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

    fn sample_doc() -> Graph {
        Graph::new("g-sample")
    }

    #[test]
    fn write_and_read_cache_entry() {
        with_cfg(|_| {
            let doc = sample_doc();
            let p = write_cached("claude-abc", &doc, false).unwrap().path;
            assert!(p.exists());
            assert_eq!(p.file_name().unwrap(), "claude-abc.json");
        });
    }

    #[test]
    fn write_errors_if_exists_without_force() {
        with_cfg(|_| {
            let doc = sample_doc();
            let _ = write_cached("claude-abc", &doc, false).unwrap();
            let err = write_cached("claude-abc", &doc, false).unwrap_err();
            assert!(err.to_string().contains("already exists"));
        });
    }

    #[test]
    fn write_force_overwrites() {
        with_cfg(|_| {
            let doc = sample_doc();
            let _ = write_cached("claude-abc", &doc, false).unwrap();
            let _ = write_cached("claude-abc", &doc, true).unwrap();
        });
    }

    #[test]
    fn cache_ref_finds_existing_cache_entry() {
        with_cfg(|_| {
            let doc = sample_doc();
            let p = write_cached("claude-abc", &doc, false).unwrap().path;
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
            let _ = write_cached("a", &doc, false).unwrap();
            let _ = write_cached("b", &doc, false).unwrap();
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
            let p = write_cached("claude-abc", &sample_doc(), false)
                .unwrap()
                .path;
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
