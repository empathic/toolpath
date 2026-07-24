//! Higher-level filesystem operations over [`PathResolver`].

use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::RolloutReader;
use crate::types::{RolloutItem, Session, SessionMetadata};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ConvoIO {
    resolver: PathResolver,
}

impl ConvoIO {
    pub fn new() -> Self {
        Self {
            resolver: PathResolver::new(),
        }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    pub fn codex_dir_path(&self) -> Result<PathBuf> {
        self.resolver.codex_dir()
    }

    /// List every rollout file under `~/.codex/sessions/`, newest first.
    pub fn list_rollout_files(&self) -> Result<Vec<PathBuf>> {
        self.resolver.list_rollout_files()
    }

    /// List every session id (the rollout filename stem, which
    /// [`Self::read_session`] resolves without a tree walk), newest
    /// first. One directory walk, no file reads — unlike
    /// [`Self::list_sessions`], which parses every file for metadata.
    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .list_rollout_files()?
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect())
    }

    /// Return lightweight metadata for every rollout, newest first.
    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let files = self.list_rollout_files()?;
        let mut metas = Vec::with_capacity(files.len());
        for path in files {
            match self.read_metadata(&path) {
                Ok(m) => metas.push(m),
                Err(e) => {
                    eprintln!("Warning: failed to read {}: {}", path.display(), e);
                }
            }
        }
        metas.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        Ok(metas)
    }

    /// Read one session by id or filename stem.
    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        let path = self.resolver.find_rollout_file(session_id)?;
        RolloutReader::read_session(&path)
    }

    /// Read one session by absolute path.
    pub fn read_session_path<P: AsRef<std::path::Path>>(&self, path: P) -> Result<Session> {
        RolloutReader::read_session(path)
    }

    /// Cheap per-file metadata: parses the session_meta line + walks
    /// the file for first/last timestamps.
    pub fn read_metadata<P: AsRef<std::path::Path>>(&self, path: P) -> Result<SessionMetadata> {
        let path = path.as_ref();
        // Full parse is simplest; rollout files are small (typical
        // session 200-300 KB). If that becomes a bottleneck we'd peek
        // the first line plus `stat` for mtime.
        let session = RolloutReader::read_session(path)?;

        let meta_line = session.items().find_map(|item| match item {
            RolloutItem::SessionMeta(m) => Some(m),
            _ => None,
        });

        let (cwd, cli_version, git_branch, git_commit) = match &meta_line {
            Some(m) => (
                Some(m.cwd.clone()),
                Some(m.cli_version.clone()),
                m.git.as_ref().and_then(|g| g.branch.clone()),
                m.git.as_ref().and_then(|g| g.commit_hash.clone()),
            ),
            None => (None, None, None, None),
        };

        Ok(SessionMetadata {
            id: session.id.clone(),
            file_path: session.file_path.clone(),
            started_at: session.started_at(),
            last_activity: session.last_activity(),
            cwd,
            cli_version,
            first_user_message: session.first_user_text(),
            git_branch,
            git_commit,
            line_count: session.lines.len(),
        })
    }

    pub fn session_exists(&self, session_id: &str) -> bool {
        self.resolver.find_rollout_file(session_id).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        let day = codex.join("sessions/2026/04/20");
        fs::create_dir_all(&day).unwrap();
        let body = [
            r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{"id":"019dabc6-aaa","timestamp":"2026-04-20T16:43:30.171Z","cwd":"/tmp/proj","originator":"codex-tui","cli_version":"0.118.0","source":"cli","git":{"commit_hash":"abc","branch":"main"}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        ]
        .join("\n");
        fs::write(
            day.join("rollout-2026-04-20T10-00-00-019dabc6-aaa.jsonl"),
            body,
        )
        .unwrap();

        let resolver = PathResolver::new().with_codex_dir(&codex);
        (temp, ConvoIO::with_resolver(resolver))
    }

    #[test]
    fn lists_rollouts() {
        let (_t, io) = setup();
        let files = io.list_rollout_files().unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn list_sessions_returns_metadata() {
        let (_t, io) = setup();
        let sessions = io.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "019dabc6-aaa");
        assert_eq!(sessions[0].first_user_message.as_deref(), Some("hi"));
        assert_eq!(sessions[0].git_branch.as_deref(), Some("main"));
        assert_eq!(sessions[0].git_commit.as_deref(), Some("abc"));
        assert_eq!(sessions[0].cli_version.as_deref(), Some("0.118.0"));
    }

    /// Ids come from filenames alone, so even a file whose body would
    /// fail to parse is listed; the failure surfaces on read instead.
    #[test]
    fn list_session_ids_returns_stems_without_reading_bodies() {
        let (_t, io) = setup();
        let day = io.resolver().sessions_root().unwrap().join("2026/04/21");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-04-21T09-00-00-bbb.jsonl"),
            "not json",
        )
        .unwrap();

        let ids = io.list_session_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"rollout-2026-04-20T10-00-00-019dabc6-aaa".to_string()));
        assert!(ids.contains(&"rollout-2026-04-21T09-00-00-bbb".to_string()));
        for id in &ids {
            assert!(io.read_session(id).is_ok() || id.contains("bbb"));
        }
    }

    #[test]
    fn read_session_by_id() {
        let (_t, io) = setup();
        let s = io.read_session("019dabc6-aaa").unwrap();
        assert_eq!(s.lines.len(), 2);
    }

    #[test]
    fn read_session_by_partial_uuid() {
        let (_t, io) = setup();
        let s = io.read_session("019dabc6").unwrap();
        assert_eq!(s.id, "019dabc6-aaa");
    }

    #[test]
    fn session_exists() {
        let (_t, io) = setup();
        assert!(io.session_exists("019dabc6-aaa"));
        assert!(!io.session_exists("nope"));
    }

    #[test]
    fn metadata_line_count_accurate() {
        let (_t, io) = setup();
        let metas = io.list_sessions().unwrap();
        assert_eq!(metas[0].line_count, 2);
    }

    #[test]
    fn list_sessions_empty_when_no_root() {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let io = ConvoIO::with_resolver(PathResolver::new().with_codex_dir(&codex));
        assert!(io.list_sessions().unwrap().is_empty());
    }
}
