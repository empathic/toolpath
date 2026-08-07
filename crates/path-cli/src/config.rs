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

/// Write `value` as pretty JSON to a user-private file: the parent
/// directory is created `0700` and the file itself `0600`.
///
/// Every credential-bearing blob under the config dir goes through
/// here (Pathbase sessions, S3 settings) so the permissions story is
/// stated once instead of re-derived per call site.
pub(crate) fn write_private_json<T: serde::Serialize>(
    path: &std::path::Path,
    value: &T,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| anyhow!("create {}: {e}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    let payload = serde_json::to_string_pretty(value)?;
    std::fs::write(path, payload).map_err(|e| anyhow!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow!("chmod 0600 {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Read a JSON blob written by [`write_private_json`]. A missing or
/// empty file is `Ok(None)` — "not configured", not an error.
pub(crate) fn read_private_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).map_err(|e| anyhow!("decode {}: {e}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("read {}: {e}", path.display())),
    }
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
