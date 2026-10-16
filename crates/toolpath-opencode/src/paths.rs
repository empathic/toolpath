//! Filesystem layout for opencode state.
//!
//! opencode stores everything under `$XDG_DATA_HOME/opencode/`
//! (`~/.local/share/opencode/` on macOS/Linux). The primary
//! conversation store is a SQLite database in that directory; per-step
//! filesystem snapshots live in sibling bare git repositories under
//! `snapshot/<project-id>/<sha1(worktree)>/`.
//!
//! # Which database file
//!
//! The database's file name depends on the opencode release:
//!
//! - Older releases (observed: 1.3.10, and the `opencode.db` dated
//!   2026-09-01 on the machine this was written on) read and write
//!   `opencode.db`.
//! - Current releases name the file after their release *channel*.
//!   Decoded from the bundle inside opencode 1.18.18 (channel
//!   `"stable"`), the resolution is, in order:
//!   1. `$OPENCODE_DB` if set — used as is when absolute, otherwise
//!      joined to the data dir;
//!   2. `opencode.db` when the channel is `latest`, `beta` or `prod`,
//!      or `$OPENCODE_DISABLE_CHANNEL_DB` is `1`/`true`;
//!   3. otherwise `opencode-<channel>.db`, i.e. `opencode-stable.db`
//!      for a stable build.
//!
//!   A current opencode does **not** migrate or read a pre-existing
//!   `opencode.db`; both files simply coexist. The release that
//!   introduced the channel name is not pinned here — only the two
//!   witnesses above are.
//!
//! toolpath mirrors that rule so that `path resume --harness opencode`
//! writes the file the installed `opencode` will open, and derive reads
//! the same file opencode shows:
//!
//! - `$OPENCODE_DB` (or [`PathResolver::with_db_file`]) pins the file
//!   exactly as it does for opencode.
//! - Otherwise the candidates are `opencode-stable.db` then
//!   `opencode.db` (only `opencode.db` when
//!   `$OPENCODE_DISABLE_CHANNEL_DB` is on, see
//!   [`PathResolver::with_channel_db`]); [`PathResolver::db_path`]
//!   returns the first that exists, or `opencode-stable.db` when
//!   neither does, since that is what a current opencode creates.
//!
//! Keeping both files in sync is deliberately *not* attempted:
//! opencode itself reads exactly one of them, and the stable file's
//! `session` table has columns and foreign keys the legacy file lacks.
//! The one case this rule cannot see is a machine whose only file is a
//! legacy `opencode.db` but whose installed opencode is a current,
//! never-run build; run `opencode` once (it creates
//! `opencode-stable.db`) or set `OPENCODE_DB`.
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
const LOG_SUBDIR: &str = "log";

/// The database a current, stable-channel opencode reads and writes
/// (`opencode-<channel>.db` with channel `stable`).
pub const DB_FILE_STABLE: &str = "opencode-stable.db";
/// The database older opencode releases read and write, and that current
/// releases fall back to on the `latest`/`beta`/`prod` channels or with
/// [`DISABLE_CHANNEL_DB_ENV`] set.
pub const DB_FILE_LEGACY: &str = "opencode.db";
/// opencode's own override of the database location; honoured here with
/// the same semantics (absolute as is, relative under the data dir).
pub const DB_ENV: &str = "OPENCODE_DB";
/// When `1` or `true`, opencode ignores its channel and uses
/// [`DB_FILE_LEGACY`]; honoured here the same way.
pub const DISABLE_CHANNEL_DB_ENV: &str = "OPENCODE_DISABLE_CHANNEL_DB";

/// Builder-style resolver over the opencode data directory.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    /// `$OPENCODE_DB` or [`PathResolver::with_db_file`].
    db_file: Option<PathBuf>,
    /// `false` when `$OPENCODE_DISABLE_CHANNEL_DB` is on.
    channel_db: bool,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    /// A resolver seeded from the environment: `$HOME`, and opencode's
    /// own `$OPENCODE_DB` / `$OPENCODE_DISABLE_CHANNEL_DB` overrides.
    pub fn new() -> Self {
        Self {
            home_dir: home_dir(),
            data_dir: None,
            db_file: std::env::var_os(DB_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            channel_db: !env_is_on(DISABLE_CHANNEL_DB_ENV),
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

    /// Pin the database file, exactly as opencode's `$OPENCODE_DB` does:
    /// an absolute path is used as is, a relative one is joined to the
    /// data dir. `None` clears an override picked up from the
    /// environment, restoring the channel rule.
    pub fn with_db_file<P: Into<PathBuf>>(mut self, db_file: Option<P>) -> Self {
        self.db_file = db_file.map(Into::into);
        self
    }

    /// Whether the channel-named database (`opencode-stable.db`) is
    /// considered at all. `false` mirrors opencode running with
    /// `$OPENCODE_DISABLE_CHANNEL_DB=1`: only `opencode.db` is used.
    pub fn with_channel_db(mut self, channel_db: bool) -> Self {
        self.channel_db = channel_db;
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    pub fn data_dir(&self) -> Result<PathBuf> {
        if let Some(d) = &self.data_dir {
            return Ok(d.clone());
        }
        // XDG_DATA_HOME fallback logic mirroring the `xdg-basedir` crate.
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let p = PathBuf::from(xdg).join("opencode");
            if !p.as_os_str().is_empty() {
                return Ok(p);
            }
        }
        Ok(self.home_dir()?.join(".local/share/opencode"))
    }

    /// The database files opencode could be using, most current first.
    /// See the module docs for the rule.
    pub fn db_candidates(&self) -> Result<Vec<PathBuf>> {
        if let Some(pinned) = &self.db_file {
            let p = if pinned.is_absolute() {
                pinned.clone()
            } else {
                self.data_dir()?.join(pinned)
            };
            return Ok(vec![p]);
        }
        let data = self.data_dir()?;
        if !self.channel_db {
            return Ok(vec![data.join(DB_FILE_LEGACY)]);
        }
        Ok(vec![data.join(DB_FILE_STABLE), data.join(DB_FILE_LEGACY)])
    }

    /// The database to read from and project into: the first of
    /// [`Self::db_candidates`] that exists, or the first candidate (what
    /// a current opencode would create) when none does.
    pub fn db_path(&self) -> Result<PathBuf> {
        let candidates = self.db_candidates()?;
        Ok(candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| candidates[0].clone()))
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

/// opencode's truthiness for `$OPENCODE_DISABLE_CHANNEL_DB`: exactly
/// `1` or `true`.
fn env_is_on(name: &str) -> bool {
    matches!(std::env::var(name).as_deref(), Ok("1") | Ok("true"))
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

    /// A resolver over a temp data dir with no environment overrides, so
    /// the tests are deterministic regardless of the host's `OPENCODE_*`.
    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_data_dir(&data)
            .with_db_file(None::<PathBuf>)
            .with_channel_db(true);
        (temp, resolver)
    }

    #[test]
    fn data_dir_defaults_to_home_when_no_xdg() {
        let temp = TempDir::new().unwrap();
        // SAFETY: tests that mutate env vars serialize via the lock in
        // src/reader.rs tests; the direct test below doesn't mutate.
        let r = PathResolver::new().with_home(temp.path());
        let d = r.data_dir().unwrap();
        assert!(d.ends_with(".local/share/opencode"), "got {:?}", d);
    }

    #[test]
    fn db_candidates_are_stable_then_legacy() {
        let (_t, r) = setup();
        let data = r.data_dir().unwrap();
        assert_eq!(
            r.db_candidates().unwrap(),
            vec![data.join("opencode-stable.db"), data.join("opencode.db")]
        );
    }

    #[test]
    fn db_path_is_stable_when_neither_exists() {
        let (_t, r) = setup();
        assert!(
            r.db_path()
                .unwrap()
                .ends_with("opencode/opencode-stable.db")
        );
        assert!(!r.db_exists());
    }

    #[test]
    fn db_path_is_legacy_when_only_legacy_exists() {
        let (_t, r) = setup();
        let data = r.data_dir().unwrap();
        fs::write(data.join("opencode.db"), b"").unwrap();
        assert_eq!(r.db_path().unwrap(), data.join("opencode.db"));
        assert!(r.db_exists());
    }

    #[test]
    fn db_path_prefers_stable_when_both_exist() {
        let (_t, r) = setup();
        let data = r.data_dir().unwrap();
        fs::write(data.join("opencode.db"), b"").unwrap();
        fs::write(data.join("opencode-stable.db"), b"").unwrap();
        assert_eq!(r.db_path().unwrap(), data.join("opencode-stable.db"));
    }

    #[test]
    fn channel_db_off_uses_only_legacy() {
        let (_t, r) = setup();
        let r = r.with_channel_db(false);
        let data = r.data_dir().unwrap();
        fs::write(data.join("opencode-stable.db"), b"").unwrap();
        assert_eq!(r.db_candidates().unwrap(), vec![data.join("opencode.db")]);
        assert_eq!(r.db_path().unwrap(), data.join("opencode.db"));
    }

    #[test]
    fn pinned_db_file_relative_joins_data_dir() {
        let (_t, r) = setup();
        let r = r.with_db_file(Some("custom.db"));
        let data = r.data_dir().unwrap();
        fs::write(data.join("opencode-stable.db"), b"").unwrap();
        assert_eq!(r.db_candidates().unwrap(), vec![data.join("custom.db")]);
        // Pinned wins even though it does not exist and the stable one does.
        assert_eq!(r.db_path().unwrap(), data.join("custom.db"));
    }

    #[test]
    fn pinned_db_file_absolute_is_used_as_is() {
        let (t, r) = setup();
        let elsewhere = t.path().join("elsewhere.db");
        let r = r.with_db_file(Some(elsewhere.clone()));
        assert_eq!(r.db_path().unwrap(), elsewhere);
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
