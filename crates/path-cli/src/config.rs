//! Shared config-directory resolution.
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
/// The picker listing cache under the config dir (see `listing_cache`).
pub(crate) const LISTING_CACHE_FILE_NAME: &str = "listing-cache.json";
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
}
