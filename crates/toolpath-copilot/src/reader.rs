//! Read a Copilot CLI session directory into a [`Session`].
//!
//! The reader is tolerant: a malformed `events.jsonl` line is logged to
//! stderr and skipped (a crash can leave the final line truncated). Strict
//! mode turns the first malformed line into an error.

use crate::error::{ConvoError, Result};
use crate::types::{EventLine, Session, Workspace, parse_workspace};
use std::io::{BufRead, BufReader};
use std::path::Path;

const EVENTS_FILE: &str = "events.jsonl";
const WORKSPACE_FILE: &str = "workspace.yaml";

pub struct EventReader;

impl EventReader {
    /// Read a `session-state/<id>/` directory, skipping malformed lines.
    /// The session id is the directory name.
    pub fn read_session_dir<P: AsRef<Path>>(dir: P) -> Result<Session> {
        Self::read_session_dir_with(dir, false)
    }

    /// [`Self::read_session_dir`] with the strict flag supplied by the
    /// caller. Strict mode returns the first malformed line as an error.
    pub fn read_session_dir_with<P: AsRef<Path>>(dir: P, strict: bool) -> Result<Session> {
        let dir = dir.as_ref();
        let id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ConvoError::InvalidFormat(dir.to_path_buf()))?
            .to_string();
        let events_path = dir.join(EVENTS_FILE);
        let lines = Self::read_lines_with(&events_path, strict)?;
        let workspace = Self::read_workspace(dir);
        Ok(Session {
            id,
            dir_path: dir.to_path_buf(),
            lines,
            workspace,
        })
    }

    /// Read the sibling `workspace.yaml`, if present and non-empty. Any read
    /// or parse trouble is treated as "no workspace context" (best-effort).
    pub fn read_workspace<P: AsRef<Path>>(dir: P) -> Option<Workspace> {
        let path = dir.as_ref().join(WORKSPACE_FILE);
        let content = std::fs::read_to_string(&path).ok()?;
        let ws = parse_workspace(&content);
        if ws.is_empty() { None } else { Some(ws) }
    }

    /// Parse the JSONL lines of an `events.jsonl` file, skipping
    /// malformed lines.
    pub fn read_lines<P: AsRef<Path>>(path: P) -> Result<Vec<EventLine>> {
        Self::read_lines_with(path, false)
    }

    /// [`Self::read_lines`] with the strict flag supplied by the caller.
    /// Strict mode returns the first malformed line as an error.
    pub fn read_lines_with<P: AsRef<Path>>(path: P, strict: bool) -> Result<Vec<EventLine>> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<EventLine>(trimmed) {
                Ok(ev) => lines.push(ev),
                Err(e) => {
                    if strict {
                        return Err(ConvoError::Json(e));
                    }
                    eprintln!(
                        "Warning: skipping malformed line {} in {}: {}",
                        idx + 1,
                        path.display(),
                        e
                    );
                }
            }
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn session_dir(id: &str, body: &str) -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("session-state").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), body).unwrap();
        (temp, dir)
    }

    #[test]
    fn reads_id_from_dir_name() {
        let body = r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"cwd":"/tmp/p","model":"m"}}"#;
        let (_t, dir) = session_dir("sess-abc", body);
        let s = EventReader::read_session_dir(&dir).unwrap();
        assert_eq!(s.id, "sess-abc");
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.cwd().as_deref(), Some("/tmp/p"));
    }

    #[test]
    fn skips_blank_and_malformed_lines() {
        let body = "\n{not json}\n{\"type\":\"user.message\",\"data\":{\"text\":\"hi\"}}\n";
        let (_t, dir) = session_dir("sess-1", body);
        let s = EventReader::read_session_dir(&dir).unwrap();
        // blank skipped, malformed skipped, one good line kept
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.first_user_text().as_deref(), Some("hi"));
    }

    #[test]
    fn reads_sibling_workspace_yaml() {
        let body = r#"{"type":"session.start","data":{"cwd":"/tmp/p"}}"#;
        let (_t, dir) = session_dir("sess-ws", body);
        std::fs::write(
            dir.join("workspace.yaml"),
            "git_root: /tmp/p\nrepository: git@github.com:o/r.git\nbranch: main\n",
        )
        .unwrap();
        let s = EventReader::read_session_dir(&dir).unwrap();
        let ws = s.workspace.expect("workspace parsed");
        assert_eq!(ws.branch.as_deref(), Some("main"));
        assert_eq!(ws.repository.as_deref(), Some("git@github.com:o/r.git"));
    }

    #[test]
    fn no_workspace_yaml_is_none() {
        let body = r#"{"type":"user.message","data":{"text":"hi"}}"#;
        let (_t, dir) = session_dir("sess-no-ws", body);
        let s = EventReader::read_session_dir(&dir).unwrap();
        assert!(s.workspace.is_none());
    }

    #[test]
    fn strict_mode_errors_on_malformed() {
        let body = "{bad}\n";
        let (_t, dir) = session_dir("sess-2", body);
        let events = dir.join("events.jsonl");
        assert!(EventReader::read_lines_with(&events, true).is_err());
        assert!(EventReader::read_lines(&events).unwrap().is_empty());
        assert!(EventReader::read_session_dir_with(&dir, true).is_err());
    }
}
