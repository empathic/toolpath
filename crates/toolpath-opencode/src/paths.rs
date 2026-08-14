//! Filesystem layout for opencode state.
//!
//! opencode stores everything under `$XDG_DATA_HOME/opencode/`
//! (`~/.local/share/opencode/` on macOS/Linux). The primary
//! conversation store is the `opencode.db` SQLite database; per-step
//! filesystem snapshots live in sibling bare git repositories under
//! `snapshot/<project-id>/<sha1(worktree)>/`.
//!
//! `project.id` is itself the SHA of the repo's first root commit
//! (`git rev-list --max-parents=0 HEAD`), so a project survives
//! being moved on disk. The inner snapshot dirname is the SHA-1 of
//! the absolute worktree path, which means snapshots are keyed by
//! exact path — moving the worktree orphans old snapshots even
//! though the session IDs still resolve.

use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

const SNAPSHOT_SUBDIR: &str = "snapshot";
const DB_FILE: &str = "opencode.db";
const LOG_SUBDIR: &str = "log";

/// Builder-style resolver over the opencode data directory.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: PathBuf,
    xdg_data_home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
}

impl PathResolver {
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self {
            home_dir: home.into(),
            xdg_data_home: None,
            data_dir: None,
        }
    }

    /// Set the XDG data root. The data directory is `<xdg>/opencode`,
    /// and it wins against the home-derived default.
    pub fn with_xdg_data_home<P: Into<PathBuf>>(mut self, xdg_data_home: P) -> Self {
        self.xdg_data_home = Some(xdg_data_home.into());
        self
    }

    /// Override the data directory directly (defaults to
    /// `<xdg>/opencode` or `<home>/.local/share/opencode`).
    pub fn with_data_dir<P: Into<PathBuf>>(mut self, data_dir: P) -> Self {
        self.data_dir = Some(data_dir.into());
        self
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(d) = &self.data_dir {
            return d.clone();
        }
        if let Some(xdg) = &self.xdg_data_home {
            return xdg.join("opencode");
        }
        self.home_dir.join(".local/share/opencode")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join(DB_FILE)
    }

    pub fn snapshot_root(&self) -> PathBuf {
        self.data_dir().join(SNAPSHOT_SUBDIR)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir().join(LOG_SUBDIR)
    }

    /// The bare git repository that backs snapshots for a given
    /// `(project_id, worktree)` pair.
    ///
    /// opencode has used two layouts over its lifetime:
    /// - Current: `snapshot/<project-id>/<sha1(worktree)>/` — one
    ///   gitdir per `(project, worktree)` pair so forked worktrees
    ///   get isolated snapshot stores.
    /// - Older: `snapshot/<project-id>/` — a single gitdir per
    ///   project regardless of worktree.
    ///
    /// Returns the first candidate that exists. If neither exists,
    /// returns the current-layout path (so the caller's subsequent
    /// `git2::Repository::open` will produce a clean NotFound error).
    pub fn snapshot_gitdir(&self, project_id: &str, worktree: &Path) -> PathBuf {
        let root = self.snapshot_root();
        let worktree_hash = sha1_hex(worktree.to_string_lossy().as_bytes());
        let nested = root.join(project_id).join(&worktree_hash);
        if nested.exists() {
            return nested;
        }
        let flat = root.join(project_id);
        if flat.exists() && flat.join("config").exists() {
            return flat;
        }
        nested
    }

    pub fn exists(&self) -> bool {
        self.data_dir().exists()
    }

    pub fn db_exists(&self) -> bool {
        self.db_path().exists()
    }
}

pub(crate) fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(40);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let resolver = PathResolver::new(temp.path()).with_data_dir(&data);
        (temp, resolver)
    }

    #[test]
    fn data_dir_defaults_to_home() {
        let temp = TempDir::new().unwrap();
        let r = PathResolver::new(temp.path());
        assert_eq!(r.data_dir(), temp.path().join(".local/share/opencode"));
        assert_eq!(r.home_dir(), temp.path());
    }

    #[test]
    fn xdg_data_home_wins_against_home_and_loses_to_the_data_dir() {
        let r = PathResolver::new("/home/alex").with_xdg_data_home("/xdg/data");
        assert_eq!(r.data_dir(), PathBuf::from("/xdg/data/opencode"));

        let r = r.with_data_dir("/explicit/dir");
        assert_eq!(r.data_dir(), PathBuf::from("/explicit/dir"));
    }

    #[test]
    fn db_path_under_data_dir() {
        let (_t, r) = setup();
        assert!(r.db_path().ends_with("opencode/opencode.db"));
    }

    #[test]
    fn snapshot_root_and_log_dir_under_data_dir() {
        let r = PathResolver::new("/home/alex");
        assert_eq!(
            r.snapshot_root(),
            PathBuf::from("/home/alex/.local/share/opencode/snapshot")
        );
        assert_eq!(
            r.log_dir(),
            PathBuf::from("/home/alex/.local/share/opencode/log")
        );
    }

    #[test]
    fn snapshot_gitdir_uses_sha1_of_worktree() {
        let (_t, r) = setup();
        let pid = "4e82d608d080e9d92be51e24b592302df6a8cbf8";
        let wt = Path::new("/Users/ben/empathic/oss/toolpath");
        let gd = r.snapshot_gitdir(pid, wt);
        // sha1("/Users/ben/empathic/oss/toolpath") = bb93f39a…
        assert!(gd.to_string_lossy().contains(pid));
        assert!(
            gd.to_string_lossy()
                .contains("bb93f39a69862ba18e7893cc96424f83876a9687")
        );
    }

    #[test]
    fn sha1_of_known_string() {
        assert_eq!(
            sha1_hex(b"/Users/ben/empathic/oss/toolpath"),
            "bb93f39a69862ba18e7893cc96424f83876a9687"
        );
    }

    #[test]
    fn exists_reflects_data_dir() {
        let (_t, r) = setup();
        assert!(r.exists());
        let missing = PathResolver::new("/never/exists");
        assert!(!missing.exists());
    }
}
