//! Higher-level filesystem operations over [`PathResolver`].

use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::EventReader;
use crate::types::{Session, SessionMetadata};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ConvoIO {
    resolver: PathResolver,
    strict: bool,
}

impl ConvoIO {
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self::with_resolver(PathResolver::new(home))
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            resolver,
            strict: false,
        }
    }

    /// Strict mode makes an unparseable events line an error instead of
    /// a warning.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    pub fn strict(&self) -> bool {
        self.strict
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    pub fn copilot_dir_path(&self) -> PathBuf {
        self.resolver.copilot_dir()
    }

    /// List every session directory (current + legacy), newest first.
    pub fn list_session_dirs(&self) -> Result<Vec<PathBuf>> {
        self.resolver.list_session_dirs()
    }

    /// Return lightweight metadata for every session, newest first.
    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let dirs = self.list_session_dirs()?;
        let mut metas = Vec::with_capacity(dirs.len());
        for dir in dirs {
            match self.read_metadata(&dir) {
                Ok(m) => metas.push(m),
                Err(e) => eprintln!("Warning: failed to read {}: {}", dir.display(), e),
            }
        }
        metas.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        Ok(metas)
    }

    /// Read one session by id (exact or unique prefix).
    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        let dir = self.resolver.find_session_dir(session_id)?;
        EventReader::read_session_dir_with(&dir, self.strict)
    }

    /// Read one session by its directory path.
    pub fn read_session_dir<P: AsRef<std::path::Path>>(&self, dir: P) -> Result<Session> {
        EventReader::read_session_dir_with(dir, self.strict)
    }

    /// Cheap per-session metadata. Copilot session files have no compact
    /// header, so this walks the file (sessions are small).
    pub fn read_metadata<P: AsRef<std::path::Path>>(&self, dir: P) -> Result<SessionMetadata> {
        let session = EventReader::read_session_dir_with(dir, self.strict)?;
        Ok(SessionMetadata {
            id: session.id.clone(),
            dir_path: session.dir_path.clone(),
            started_at: session.started_at(),
            last_activity: session.last_activity(),
            cwd: session.cwd(),
            version: session.version(),
            first_user_message: session.first_user_text(),
            line_count: session.lines.len(),
        })
    }

    pub fn session_exists(&self, session_id: &str) -> bool {
        self.resolver.find_session_dir(session_id).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let copilot = temp.path().join(".copilot");
        let dir = copilot.join("session-state").join("sess-abc");
        fs::create_dir_all(&dir).unwrap();
        let body = [
            r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"copilotVersion":"1.0.66","context":{"cwd":"/tmp/proj"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-06-30T10:00:01.000Z","data":{"content":"hi there"}}"#,
        ]
        .join("\n");
        fs::write(dir.join("events.jsonl"), body).unwrap();
        let resolver = PathResolver::new(temp.path()).with_copilot_dir(&copilot);
        (temp, ConvoIO::with_resolver(resolver))
    }

    #[test]
    fn lists_sessions_with_metadata() {
        let (_t, io) = setup();
        let sessions = io.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-abc");
        assert_eq!(sessions[0].cwd.as_deref(), Some("/tmp/proj"));
        assert_eq!(sessions[0].version.as_deref(), Some("1.0.66"));
        assert_eq!(sessions[0].first_user_message.as_deref(), Some("hi there"));
        assert_eq!(sessions[0].line_count, 2);
    }

    #[test]
    fn reads_session_by_id_and_prefix() {
        let (_t, io) = setup();
        assert_eq!(io.read_session("sess-abc").unwrap().lines.len(), 2);
        assert_eq!(io.read_session("sess").unwrap().id, "sess-abc");
    }

    #[test]
    fn session_exists_reflects_disk() {
        let (_t, io) = setup();
        assert!(io.session_exists("sess-abc"));
        assert!(!io.session_exists("nope"));
    }

    #[test]
    fn strict_reads_reject_a_malformed_line() {
        let (t, io) = setup();
        let dir = t.path().join(".copilot/session-state/sess-bad");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), "{bad}\n").unwrap();

        assert!(io.read_session("sess-bad").is_ok());
        assert!(
            io.clone()
                .with_strict(true)
                .read_session("sess-bad")
                .is_err()
        );
    }
}
