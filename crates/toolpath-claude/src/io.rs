use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::ConversationReader;
use crate::types::{Conversation, ConversationMetadata, HistoryEntry};
use std::path::PathBuf;

#[derive(Debug, Clone)]
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

    pub fn read_conversation(&self, project_path: &str, session_id: &str) -> Result<Conversation> {
        let path = self.resolver.conversation_file(project_path, session_id)?;
        ConversationReader::read_conversation(&path)
    }

    pub fn read_conversation_metadata(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<ConversationMetadata> {
        let path = self.resolver.conversation_file(project_path, session_id)?;
        ConversationReader::read_conversation_metadata(&path)
    }

    pub fn list_conversations(&self, project_path: &str) -> Result<Vec<String>> {
        self.resolver.list_conversations(project_path)
    }

    pub fn list_conversation_metadata(
        &self,
        project_path: &str,
    ) -> Result<Vec<ConversationMetadata>> {
        let sessions = self.list_conversations(project_path)?;
        let mut metadata = Vec::new();

        for session_id in sessions {
            match self.read_conversation_metadata(project_path, &session_id) {
                Ok(meta) => metadata.push(meta),
                Err(e) => {
                    eprintln!("Warning: Failed to read metadata for {}: {}", session_id, e);
                }
            }
        }

        metadata.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        Ok(metadata)
    }

    pub fn list_projects(&self) -> Result<Vec<String>> {
        self.resolver.list_project_dirs()
    }

    pub fn read_history(&self) -> Result<Vec<HistoryEntry>> {
        let path = self.resolver.history_file()?;
        ConversationReader::read_history(&path)
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    pub fn claude_dir_path(&self) -> Result<PathBuf> {
        self.resolver.claude_dir()
    }

    pub fn conversation_exists(&self, project_path: &str, session_id: &str) -> Result<bool> {
        let path = self.resolver.conversation_file(project_path, session_id)?;
        Ok(path.exists())
    }

    pub fn project_exists(&self, project_path: &str) -> bool {
        self.resolver
            .project_dir(project_path)
            .map(|p| p.exists())
            .unwrap_or(false)
    }
}

impl Default for ConvoIO {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_io() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"u2","timestamp":"2024-01-01T00:01:00Z","message":{"role":"assistant","content":"Hi"}}"#;
        fs::write(
            project_dir.join("session-1.jsonl"),
            format!("{}\n{}\n", entry1, entry2),
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let io = ConvoIO::with_resolver(resolver);
        (temp, io)
    }

    #[test]
    fn test_default() {
        let _io = ConvoIO::default();
    }

    #[test]
    fn test_read_conversation() {
        let (_temp, io) = setup_io();
        let convo = io.read_conversation("/test/project", "session-1").unwrap();
        assert_eq!(convo.entries.len(), 2);
    }

    #[test]
    fn test_read_conversation_metadata() {
        let (_temp, io) = setup_io();
        let meta = io
            .read_conversation_metadata("/test/project", "session-1")
            .unwrap();
        assert_eq!(meta.message_count, 2);
    }

    #[test]
    fn test_list_conversations() {
        let (_temp, io) = setup_io();
        let sessions = io.list_conversations("/test/project").unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_list_conversation_metadata() {
        let (_temp, io) = setup_io();
        let meta = io.list_conversation_metadata("/test/project").unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 2);
    }

    #[test]
    fn test_list_projects() {
        let (_temp, io) = setup_io();
        let projects = io.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
    }

    #[test]
    fn test_exists() {
        let (_temp, io) = setup_io();
        assert!(io.exists());
    }

    #[test]
    fn test_claude_dir_path() {
        let (_temp, io) = setup_io();
        let path = io.claude_dir_path().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_conversation_exists() {
        let (_temp, io) = setup_io();
        assert!(
            io.conversation_exists("/test/project", "session-1")
                .unwrap()
        );
        assert!(
            !io.conversation_exists("/test/project", "nonexistent")
                .unwrap()
        );
    }

    #[test]
    fn test_project_exists() {
        let (_temp, io) = setup_io();
        assert!(io.project_exists("/test/project"));
        assert!(!io.project_exists("/nonexistent"));
    }

    #[test]
    fn test_read_history_no_file() {
        let (_temp, io) = setup_io();
        let history = io.read_history().unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_resolver_accessor() {
        let (_temp, io) = setup_io();
        assert!(io.resolver().exists());
    }
}
