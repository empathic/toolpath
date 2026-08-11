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

use crate::error::{ConvoError, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

const SNAPSHOT_SUBDIR: &str = "snapshot";
const DB_FILE: &str = "opencode.db";
const LOG_SUBDIR: &str = "log";

/// Builder-style resolver over the opencode data directory.
///
/// All environment reads happen in [`PathResolver::new`]; a
/// constructed resolver's answers never change, even if the
/// environment does.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    /// `$XDG_DATA_HOME` as captured at construction.
    xdg_data_home: Option<PathBuf>,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        Self {
            home_dir: home_dir(),
            data_dir: None,
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    /// Override the data directory directly (defaults to
    /// `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub fn with_data_dir<P: Into<PathBuf>>(mut self, data_dir: P) -> Self {
        self.data_dir = Some(data_dir.into());
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    pub fn data_dir(&self) -> Result<PathBuf> {
        if let Some(d) = &self.data_dir {
            return Ok(d.clone());
        }
        if let Some(xdg) = &self.xdg_data_home {
            return Ok(xdg.join("opencode"));
        }
        Ok(self.home_dir()?.join(".local/share/opencode"))
    }

    pub fn db_path(&self) -> Result<PathBuf> {
        Ok(self.data_dir()?.join(DB_FILE))
    }

    pub fn snapshot_root(&self) -> Result<PathBuf> {
        Ok(self.data_dir()?.join(SNAPSHOT_SUBDIR))
    }

    pub fn log_dir(&self) -> Result<PathBuf> {
        Ok(self.data_dir()?.join(LOG_SUBDIR))
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
    pub fn snapshot_gitdir(&self, project_id: &str, worktree: &Path) -> Result<PathBuf> {
        let root = self.snapshot_root()?;
        let worktree_hash = sha1_hex(worktree.to_string_lossy().as_bytes());
        let nested = root.join(project_id).join(&worktree_hash);
        if nested.exists() {
            return Ok(nested);
        }
        let flat = root.join(project_id);
        if flat.exists() && flat.join("config").exists() {
            return Ok(flat);
        }
        Ok(nested)
    }

    pub fn exists(&self) -> bool {
        self.data_dir().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn db_exists(&self) -> bool {
        self.db_path().map(|p| p.exists()).unwrap_or(false)
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
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
    use std::ffi::OsString;
    use std::fs;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    /// The process environment is global state shared by every test
    /// thread, so all tests that read or write `XDG_DATA_HOME` must
    /// serialize on this lock (via [`XdgVarGuard`]).
    static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Holds `TEST_ENV_LOCK` for its lifetime and restores the
    /// original `XDG_DATA_HOME` on drop (including on panic).
    struct XdgVarGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Option<OsString>,
    }

    impl XdgVarGuard {
        fn lock() -> Self {
            let lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            Self {
                _lock: lock,
                saved: std::env::var_os("XDG_DATA_HOME"),
            }
        }

        fn set(&self, value: &str) {
            // SAFETY: exclusive env access — every env-touching test
            // in this binary holds TEST_ENV_LOCK, which we hold.
            unsafe { std::env::set_var("XDG_DATA_HOME", value) }
        }

        fn unset(&self) {
            // SAFETY: as in `set`.
            unsafe { std::env::remove_var("XDG_DATA_HOME") }
        }
    }

    impl Drop for XdgVarGuard {
        fn drop(&mut self) {
            // SAFETY: as in `set` — the lock is released after this.
            unsafe {
                match self.saved.take() {
                    Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                    None => std::env::remove_var("XDG_DATA_HOME"),
                }
            }
        }
    }

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_data_dir(&data);
        (temp, resolver)
    }

    #[test]
    fn data_dir_defaults_to_home_when_no_xdg() {
        let env = XdgVarGuard::lock();
        env.unset();
        let temp = TempDir::new().unwrap();
        let r = PathResolver::new().with_home(temp.path());
        let d = r.data_dir().unwrap();
        assert!(d.ends_with(".local/share/opencode"), "got {:?}", d);
    }

    #[test]
    fn data_dir_captures_xdg_at_construction() {
        let env = XdgVarGuard::lock();
        env.set("/xdg/at/construction");
        let r = PathResolver::new();
        assert_eq!(
            r.data_dir().unwrap(),
            PathBuf::from("/xdg/at/construction/opencode")
        );
        // Changing the environment after construction must not
        // change the resolver's answers.
        env.set("/changed/later");
        assert_eq!(
            r.data_dir().unwrap(),
            PathBuf::from("/xdg/at/construction/opencode")
        );
    }

    #[test]
    fn with_home_is_not_overridden_by_later_xdg() {
        let env = XdgVarGuard::lock();
        env.unset();
        let r = PathResolver::new().with_home("/pinned/home");
        // A caller who pinned home must not silently resolve into a
        // data dir taken from env set after construction.
        env.set("/sneaky/xdg");
        assert_eq!(
            r.data_dir().unwrap(),
            PathBuf::from("/pinned/home/.local/share/opencode")
        );
    }

    #[test]
    fn db_path_under_data_dir() {
        let (_t, r) = setup();
        assert!(r.db_path().unwrap().ends_with("opencode/opencode.db"));
    }

    #[test]
    fn snapshot_gitdir_uses_sha1_of_worktree() {
        let (_t, r) = setup();
        let pid = "4e82d608d080e9d92be51e24b592302df6a8cbf8";
        let wt = Path::new("/Users/ben/empathic/oss/toolpath");
        let gd = r.snapshot_gitdir(pid, wt).unwrap();
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
        let missing = PathResolver::new().with_data_dir("/never/exists");
        assert!(!missing.exists());
    }
}
