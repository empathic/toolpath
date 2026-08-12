//! Shared config-directory and home-directory resolution.
//!
//! Kept in its own module so it can be used by `cmd_cache` (needed on every
//! target, including wasm/emscripten) and `cmd_pathbase` (native-only).
//! `cmd_pathbase` is cfg-gated; without this split, anything `cmd_cache`
//! imports from it would break wasm builds.

use anyhow::{Result, anyhow};
use std::path::PathBuf;

pub(crate) const CONFIG_DIR_NAME: &str = ".toolpath";
pub(crate) const CONFIG_DIR_ENV: &str = "TOOLPATH_CONFIG_DIR";

/// The artifact manifest under the config dir (see `sync::engine`).
pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
/// Sibling advisory lock serializing manifest writers. A separate
/// file because the manifest itself is replaced by rename on every
/// write, which would drop any lock held on it.
pub(crate) const MANIFEST_LOCK_FILE_NAME: &str = "manifest.json.lock";

/// The configured toolpath config directory (default `~/.toolpath`,
/// overridable via `$TOOLPATH_CONFIG_DIR`).
pub(crate) fn config_dir() -> Result<PathBuf> {
    if let Some(override_) = std::env::var_os(CONFIG_DIR_ENV) {
        return Ok(PathBuf::from(override_));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow!("$HOME is not set — cannot locate config directory"))?;
    Ok(PathBuf::from(home).join(CONFIG_DIR_NAME))
}

/// Cross-platform `$HOME` lookup matching the providers' internal helpers.
/// Returns `None` only when neither `$HOME` nor `$USERPROFILE` is set.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Display `path` as `~/relative/part` when it's under `home`, otherwise
/// return its absolute lossy form. Pure helper — does no filesystem I/O.
pub(crate) fn home_relative(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        // strip_prefix returns the empty path when path == home; treat that
        // as plain "~".
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Shared lock for tests that manipulate `$TOOLPATH_CONFIG_DIR`. Every
/// test module that calls `set_var` / `remove_var` on this env var should
/// grab this lock first, otherwise parallel tests race and clobber each
/// other's directories.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_honors_override() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, "/tmp/test-toolpath");
        }
        let dir = config_dir().unwrap();
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        assert_eq!(dir, PathBuf::from("/tmp/test-toolpath"));
    }

    #[test]
    fn home_relative_strips_home_prefix() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(
            home_relative(
                std::path::Path::new("/Users/alex/.claude/projects"),
                Some(home)
            ),
            "~/.claude/projects"
        );
    }

    #[test]
    fn home_relative_returns_tilde_for_home_itself() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(home_relative(home, Some(home)), "~");
    }

    #[test]
    fn home_relative_passes_through_paths_outside_home() {
        let home = std::path::Path::new("/Users/alex");
        assert_eq!(
            home_relative(std::path::Path::new("/tmp/elsewhere"), Some(home)),
            "/tmp/elsewhere"
        );
    }

    #[test]
    fn home_relative_passes_through_when_no_home() {
        assert_eq!(
            home_relative(std::path::Path::new("/foo/bar"), None),
            "/foo/bar"
        );
    }
}
