//! The upload lock: one upload pass at a time per config dir, so a
//! timer firing while a pass (or a `path share --all`) is still running
//! cannot push the same sessions twice.

use crate::config::UPLOAD_LOCK_FILE_NAME;
use anyhow::{Context, Result};
use std::fs::{File, TryLockError};
use std::path::Path;

/// Take the upload lock under `config_dir`, held until the returned
/// file drops. With `wait`, block until it is free; without, `None`
/// when another process holds it.
pub(crate) fn lock_uploads(config_dir: &Path, wait: bool) -> Result<Option<File>> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("create {}", config_dir.display()))?;
    let path = config_dir.join(UPLOAD_LOCK_FILE_NAME);
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    if wait {
        file.lock()
            .with_context(|| format!("lock {}", path.display()))?;
        return Ok(Some(file));
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_turns_a_second_taker_away_until_it_drops() {
        let dir = tempfile::TempDir::new().unwrap();
        let held = lock_uploads(dir.path(), false).unwrap();
        assert!(held.is_some());
        assert!(lock_uploads(dir.path(), false).unwrap().is_none());
        drop(held);
        // Waits: a test elsewhere forking a child can hold a copy of
        // the descriptor, and so the lock, until the child execs.
        assert!(lock_uploads(dir.path(), true).unwrap().is_some());
    }

    #[test]
    fn the_lock_creates_a_missing_config_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a/b");
        assert!(lock_uploads(&nested, true).unwrap().is_some());
        assert!(nested.join(UPLOAD_LOCK_FILE_NAME).exists());
    }
}
