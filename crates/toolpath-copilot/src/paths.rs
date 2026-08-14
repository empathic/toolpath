//! Filesystem layout for GitHub Copilot CLI state.
//!
//! Sessions live at `~/.copilot/session-state/<session-id>/events.jsonl`
//! (see `docs/agents/formats/copilot-cli/directory-layout.md`). The caller
//! supplies the home directory, and [`PathResolver::with_copilot_dir`]
//! replaces the whole root. Older sessions may sit under the legacy
//! `history-session-state/` directory; we glance at it as a secondary
//! location.

use crate::error::{ConvoError, Result};
use std::fs;
use std::path::{Path, PathBuf};

const COPILOT_SUBDIR: &str = ".copilot";
const SESSION_STATE_SUBDIR: &str = "session-state";
const LEGACY_SESSION_STATE_SUBDIR: &str = "history-session-state";
const EVENTS_FILE: &str = "events.jsonl";
const WORKSPACE_FILE: &str = "workspace.yaml";
const SESSION_STORE_DB: &str = "session-store.db";

/// Builder-style resolver over the `~/.copilot/` filesystem.
#[derive(Debug, Clone)]
pub struct PathResolver {
    home_dir: PathBuf,
    copilot_dir: Option<PathBuf>,
}

impl PathResolver {
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self {
            home_dir: home.into(),
            copilot_dir: None,
        }
    }

    /// Override the copilot directory directly (defaults to
    /// `~/.copilot`).
    pub fn with_copilot_dir<P: Into<PathBuf>>(mut self, copilot_dir: P) -> Self {
        self.copilot_dir = Some(copilot_dir.into());
        self
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn copilot_dir(&self) -> PathBuf {
        match &self.copilot_dir {
            Some(d) => d.clone(),
            None => self.home_dir.join(COPILOT_SUBDIR),
        }
    }

    pub fn session_state_dir(&self) -> PathBuf {
        self.copilot_dir().join(SESSION_STATE_SUBDIR)
    }

    pub fn legacy_session_state_dir(&self) -> PathBuf {
        self.copilot_dir().join(LEGACY_SESSION_STATE_SUBDIR)
    }

    pub fn session_store_db(&self) -> PathBuf {
        self.copilot_dir().join(SESSION_STORE_DB)
    }

    pub fn exists(&self) -> bool {
        self.copilot_dir().exists()
    }

    /// `events.jsonl` path for a resolved session id.
    pub fn events_file(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.find_session_dir(session_id)?.join(EVENTS_FILE))
    }

    /// `workspace.yaml` path for a resolved session id.
    pub fn workspace_file(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.find_session_dir(session_id)?.join(WORKSPACE_FILE))
    }

    /// Enumerate every session directory (current + legacy) that contains an
    /// `events.jsonl`, newest first by `events.jsonl` mtime.
    pub fn list_session_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        for root in [self.session_state_dir(), self.legacy_session_state_dir()] {
            collect_session_dirs(&root, &mut dirs);
        }
        dirs.sort_by_key(|p| {
            let events = p.join(EVENTS_FILE);
            fs::metadata(&events)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| std::cmp::Reverse(d.as_secs()))
                .unwrap_or(std::cmp::Reverse(0))
        });
        Ok(dirs)
    }

    /// Resolve a session identifier to its `session-state/<id>/` directory.
    ///
    /// Accepts an exact session-id directory name or a unique prefix of one.
    /// (Name-based resolution would require reading `session-store.db`; see
    /// `docs/agents/formats/copilot-cli/resume-and-sessions.md`.)
    pub fn find_session_dir(&self, session_id: &str) -> Result<PathBuf> {
        let all = self.list_session_dirs()?;
        // Exact directory-name match first.
        for p in &all {
            if dir_name(p) == Some(session_id) {
                return Ok(p.clone());
            }
        }
        // Unique prefix match.
        let matches: Vec<&PathBuf> = all
            .iter()
            .filter(|p| {
                dir_name(p)
                    .map(|n| n.starts_with(session_id))
                    .unwrap_or(false)
            })
            .collect();
        match matches.len() {
            0 => Err(ConvoError::SessionNotFound(session_id.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(ConvoError::SessionNotFound(format!(
                "{} (ambiguous — {} matches)",
                session_id,
                matches.len()
            ))),
        }
    }
}

fn dir_name(p: &Path) -> Option<&str> {
    p.file_name().and_then(|n| n.to_str())
}

/// Collect immediate subdirectories of `root` that contain an `events.jsonl`.
fn collect_session_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && path.join(EVENTS_FILE).is_file()
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create `<copilot>/session-state/<id>/events.jsonl` with `body`.
    fn write_session(copilot: &Path, subdir: &str, id: &str, body: &str) {
        let dir = copilot.join(subdir).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(EVENTS_FILE), body).unwrap();
    }

    fn setup() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let copilot = temp.path().join(".copilot");
        fs::create_dir_all(&copilot).unwrap();
        let resolver = PathResolver::new(temp.path()).with_copilot_dir(&copilot);
        (temp, resolver)
    }

    #[test]
    fn copilot_dir_defaults_to_home() {
        let temp = TempDir::new().unwrap();
        let r = PathResolver::new(temp.path());
        assert_eq!(r.copilot_dir(), temp.path().join(".copilot"));
    }

    #[test]
    fn copilot_dir_override_wins_over_home() {
        let r = PathResolver::new("/home/alex").with_copilot_dir("/copilot/root");
        assert_eq!(r.copilot_dir(), PathBuf::from("/copilot/root"));
    }

    #[test]
    fn session_state_dir_under_copilot_dir() {
        let (_t, r) = setup();
        assert!(r.session_state_dir().ends_with(".copilot/session-state"));
    }

    #[test]
    fn lists_session_dirs() {
        let (t, r) = setup();
        let copilot = t.path().join(".copilot");
        write_session(&copilot, "session-state", "abc-123", "{}");
        write_session(&copilot, "session-state", "def-456", "{}");
        // A directory without events.jsonl is ignored.
        fs::create_dir_all(copilot.join("session-state/empty")).unwrap();
        let dirs = r.list_session_dirs().unwrap();
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn lists_legacy_sessions_too() {
        let (t, r) = setup();
        let copilot = t.path().join(".copilot");
        write_session(&copilot, "session-state", "new-1", "{}");
        write_session(&copilot, "history-session-state", "old-1", "{}");
        let dirs = r.list_session_dirs().unwrap();
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn find_by_exact_id() {
        let (t, r) = setup();
        let copilot = t.path().join(".copilot");
        write_session(&copilot, "session-state", "abc-123", "{}");
        let dir = r.find_session_dir("abc-123").unwrap();
        assert_eq!(dir_name(&dir), Some("abc-123"));
        assert_eq!(r.events_file("abc-123").unwrap(), dir.join("events.jsonl"));
    }

    #[test]
    fn find_by_unique_prefix() {
        let (t, r) = setup();
        let copilot = t.path().join(".copilot");
        write_session(&copilot, "session-state", "abc-123", "{}");
        let dir = r.find_session_dir("abc").unwrap();
        assert_eq!(dir_name(&dir), Some("abc-123"));
    }

    #[test]
    fn find_missing_errors() {
        let (_t, r) = setup();
        assert!(matches!(
            r.find_session_dir("nope").unwrap_err(),
            ConvoError::SessionNotFound(_)
        ));
    }

    #[test]
    fn find_ambiguous_prefix_errors() {
        let (t, r) = setup();
        let copilot = t.path().join(".copilot");
        write_session(&copilot, "session-state", "abc-1", "{}");
        write_session(&copilot, "session-state", "abc-2", "{}");
        assert!(matches!(
            r.find_session_dir("abc").unwrap_err(),
            ConvoError::SessionNotFound(_)
        ));
    }

    #[test]
    fn list_empty_when_no_root() {
        let (_t, r) = setup();
        assert!(r.list_session_dirs().unwrap().is_empty());
    }
}
