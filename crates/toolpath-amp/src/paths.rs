//! Filesystem layout for Amp state.
//!
//! Amp threads are **server-authoritative** — thread content is fetched via
//! `amp threads export`, not read from a directory (see
//! `docs/agents/formats/amp/directory-layout.md`). What lives on disk is
//! app state under the data dir (`session.json`, `secrets.json`, …), which
//! this resolver locates for existence checks ("is Amp installed / has it
//! ever run here?").
//!
//! The data dir honours `$XDG_DATA_HOME/amp`, falling back to
//! `~/.local/share/amp`. (Amp's own XDG handling is inconsistent across its
//! internal modules — see the piece-00 build log — so the isolation recipe
//! sets `HOME` *and* the XDG variables; this resolver follows the same
//! convention.)

use crate::error::{ConvoError, Result};
use std::path::{Path, PathBuf};

const AMP_DATA_SUBDIR: &str = ".local/share/amp";

/// Builder-style resolver over Amp's on-disk state.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: Option<PathBuf>,
    amp_dir: Option<PathBuf>,
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
            // `$XDG_DATA_HOME/amp` overrides the default data dir.
            amp_dir: std::env::var_os("XDG_DATA_HOME").map(|x| PathBuf::from(x).join("amp")),
        }
    }

    pub fn with_home<P: Into<PathBuf>>(mut self, home: P) -> Self {
        self.home_dir = Some(home.into());
        self
    }

    /// Override the Amp data directory directly (defaults to
    /// `~/.local/share/amp`, or `$XDG_DATA_HOME/amp` when set).
    pub fn with_amp_dir<P: Into<PathBuf>>(mut self, amp_dir: P) -> Self {
        self.amp_dir = Some(amp_dir.into());
        self
    }

    pub fn home_dir(&self) -> Result<&Path> {
        self.home_dir.as_deref().ok_or(ConvoError::NoHomeDirectory)
    }

    /// The Amp data directory (app state, not thread content).
    pub fn amp_dir(&self) -> Result<PathBuf> {
        if let Some(d) = &self.amp_dir {
            return Ok(d.clone());
        }
        Ok(self.home_dir()?.join(AMP_DATA_SUBDIR))
    }

    /// Whether Amp has state on this machine (installed and run at least
    /// once).
    pub fn exists(&self) -> bool {
        self.amp_dir().map(|p| p.exists()).unwrap_or(false)
    }

    /// The data dir, erroring with [`ConvoError::AmpDirectoryNotFound`] when
    /// it does not exist. Callers use this to distinguish "Amp isn't here"
    /// from a fetch failure.
    pub fn require_amp_dir(&self) -> Result<PathBuf> {
        let dir = self.amp_dir()?;
        if dir.exists() {
            Ok(dir)
        } else {
            Err(ConvoError::AmpDirectoryNotFound(dir))
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn bare(home: &Path) -> PathResolver {
        // Bypass ::new() so $XDG_DATA_HOME from the test environment can't
        // leak in.
        PathResolver {
            home_dir: Some(home.to_path_buf()),
            amp_dir: None,
        }
    }

    #[test]
    fn amp_dir_defaults_under_home() {
        let t = TempDir::new().unwrap();
        let r = bare(t.path());
        assert_eq!(r.amp_dir().unwrap(), t.path().join(".local/share/amp"));
    }

    #[test]
    fn with_amp_dir_overrides() {
        let t = TempDir::new().unwrap();
        let custom = t.path().join("custom-amp");
        let r = bare(t.path()).with_amp_dir(&custom);
        assert_eq!(r.amp_dir().unwrap(), custom);
    }

    #[test]
    fn exists_reflects_disk() {
        let t = TempDir::new().unwrap();
        let dir = t.path().join(".local/share/amp");
        let r = bare(t.path());
        assert!(!r.exists());
        std::fs::create_dir_all(&dir).unwrap();
        assert!(r.exists());
    }

    #[test]
    fn require_amp_dir_errors_when_missing() {
        let t = TempDir::new().unwrap();
        let r = bare(t.path());
        assert!(matches!(
            r.require_amp_dir().unwrap_err(),
            ConvoError::AmpDirectoryNotFound(_)
        ));
    }

    #[test]
    fn no_home_errors() {
        let r = PathResolver {
            home_dir: None,
            amp_dir: None,
        };
        assert!(matches!(
            r.amp_dir().unwrap_err(),
            ConvoError::NoHomeDirectory
        ));
    }
}
