//! On-disk cache for toolpath documents at `$CONFIG_DIR/documents/`.
//!
//! `path p import` and `path p export` both use this as the pivot
//! between external formats and toolpath JSON. Users refer to cached
//! documents by a short id (filename without `.json`) instead of full
//! paths. The `p cache ls | rm` subcommands make the directory legible.

use anyhow::{Context, Result, anyhow, bail};
use std::path::PathBuf;
use toolpath::v1::Graph;

use crate::config::config_dir;

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
    Ok(config_dir()?.join(crate::config::DOCUMENTS_DIR_NAME))
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
/// The document goes to a temporary file, and a rename puts it at its
/// path. A reader sees the document that was there or the new one,
/// never a part of one. When `force == false` the rename does not
/// replace a file: of two concurrent `path import` invocations of one
/// id, one errors.
pub(crate) fn write_cached(id: &str, doc: &Graph, force: bool) -> Result<PathBuf> {
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

    // The temporary file must be in the cache directory. A rename does
    // not cross filesystems.
    let mut temp = tempfile::NamedTempFile::new_in(&dir)
        .with_context(|| format!("create a temporary file in {}", dir.display()))?;
    temp.write_all(json.as_bytes())
        .with_context(|| format!("write {}", temp.path().display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", temp.path().display()))?;
    }

    let persisted = if force {
        temp.persist(&path)
    } else {
        temp.persist_noclobber(&path)
    };
    match persisted {
        Ok(_) => Ok(path),
        Err(e) if !force && e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "cache entry {id} already exists at {}; pass --force to overwrite",
                path.display()
            );
        }
        Err(e) => Err(anyhow!("write {}: {}", path.display(), e.error)),
    }
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
        let meta = entry.metadata()?;
        out.push(CacheEntry {
            id,
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        });
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.modified));
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
    fn a_refused_write_leaves_the_document_and_no_other_file() {
        with_cfg(|_| {
            let first = Graph::new("g-first");
            let path = write_cached("claude-abc", &first, false).unwrap();
            write_cached("claude-abc", &Graph::new("g-second"), false).unwrap_err();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                first.to_json_pretty().unwrap()
            );
            let names: Vec<_> = std::fs::read_dir(cache_dir().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, ["claude-abc.json"]);
        });
    }

    #[test]
    fn a_reader_sees_a_whole_document_during_a_rewrite() {
        with_cfg(|_| {
            let docs = [
                Graph::new("a".repeat(200_000)),
                Graph::new("b".repeat(300_000)),
            ];
            let path = write_cached("claude-abc", &docs[0], false).unwrap();
            let done = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|s| {
                s.spawn(|| {
                    for n in 0..200 {
                        write_cached("claude-abc", &docs[n % 2], true).unwrap();
                    }
                    done.store(true, std::sync::atomic::Ordering::Relaxed);
                });
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let json = std::fs::read_to_string(&path).unwrap();
                    Graph::from_json(&json).unwrap();
                }
            });
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
