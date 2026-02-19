#![doc = include_str!("../README.md")]

#[cfg(feature = "watcher")]
pub mod async_watcher;
pub mod derive;
pub mod error;
pub mod io;
pub mod paths;
pub mod provider;
pub mod query;
pub mod reader;
pub mod types;
#[cfg(feature = "watcher")]
pub mod watcher;

#[cfg(feature = "watcher")]
pub use async_watcher::{AsyncConversationWatcher, WatcherConfig, WatcherHandle};
pub use error::{ConvoError, Result};
pub use io::ConvoIO;
pub use paths::PathResolver;
pub use query::{ConversationQuery, HistoryQuery};
pub use reader::ConversationReader;
pub use types::{
    CacheCreation, ContentPart, Conversation, ConversationEntry, ConversationMetadata,
    HistoryEntry, Message, MessageContent, MessageRole, ToolResultContent, ToolUseRef, Usage,
};
#[cfg(feature = "watcher")]
pub use watcher::ConversationWatcher;

/// High-level interface for reading Claude conversations.
///
/// This is the primary entry point for most use cases. It provides
/// convenient methods for reading conversations, listing projects,
/// and accessing conversation history.
///
/// # Example
///
/// ```rust,no_run
/// use toolpath_claude::ClaudeConvo;
///
/// let manager = ClaudeConvo::new();
///
/// // List all projects
/// let projects = manager.list_projects()?;
///
/// // Read a conversation
/// let convo = manager.read_conversation(
///     "/Users/alex/project",
///     "session-uuid"
/// )?;
///
/// println!("Conversation has {} messages", convo.message_count());
/// # Ok::<(), toolpath_claude::ConvoError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ClaudeConvo {
    io: ConvoIO,
}

impl Default for ClaudeConvo {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeConvo {
    /// Creates a new ClaudeConvo manager with default path resolution.
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    /// Creates a ClaudeConvo manager with a custom path resolver.
    ///
    /// This is useful for testing or when working with non-standard paths.
    ///
    /// # Example
    ///
    /// ```rust
    /// use toolpath_claude::{ClaudeConvo, PathResolver};
    ///
    /// let resolver = PathResolver::new()
    ///     .with_home("/custom/home")
    ///     .with_claude_dir("/custom/.claude");
    ///
    /// let manager = ClaudeConvo::with_resolver(resolver);
    /// ```
    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    /// Returns a reference to the underlying ConvoIO.
    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    /// Returns a reference to the path resolver.
    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    /// Reads a conversation by project path and session ID.
    ///
    /// # Arguments
    ///
    /// * `project_path` - The project path (e.g., "/Users/alex/project")
    /// * `session_id` - The session UUID
    ///
    /// # Returns
    ///
    /// Returns the parsed conversation or an error if the file doesn't exist or can't be parsed.
    pub fn read_conversation(&self, project_path: &str, session_id: &str) -> Result<Conversation> {
        self.io.read_conversation(project_path, session_id)
    }

    /// Reads conversation metadata without loading the full content.
    ///
    /// This is more efficient when you only need basic information about a conversation.
    pub fn read_conversation_metadata(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<ConversationMetadata> {
        self.io.read_conversation_metadata(project_path, session_id)
    }

    /// Lists all conversation session IDs for a project.
    pub fn list_conversations(&self, project_path: &str) -> Result<Vec<String>> {
        self.io.list_conversations(project_path)
    }

    /// Lists metadata for all conversations in a project.
    ///
    /// Results are sorted by last activity (most recent first).
    pub fn list_conversation_metadata(
        &self,
        project_path: &str,
    ) -> Result<Vec<ConversationMetadata>> {
        self.io.list_conversation_metadata(project_path)
    }

    /// Lists all projects that have conversations.
    ///
    /// Returns the original project paths (e.g., "/Users/alex/project").
    pub fn list_projects(&self) -> Result<Vec<String>> {
        self.io.list_projects()
    }

    /// Reads the global history file.
    ///
    /// The history file contains a record of all queries across all projects.
    pub fn read_history(&self) -> Result<Vec<HistoryEntry>> {
        self.io.read_history()
    }

    /// Checks if the Claude directory exists.
    pub fn exists(&self) -> bool {
        self.io.exists()
    }

    /// Returns the path to the Claude directory.
    pub fn claude_dir_path(&self) -> Result<std::path::PathBuf> {
        self.io.claude_dir_path()
    }

    /// Checks if a specific conversation exists.
    pub fn conversation_exists(&self, project_path: &str, session_id: &str) -> Result<bool> {
        self.io.conversation_exists(project_path, session_id)
    }

    /// Checks if a project directory exists.
    pub fn project_exists(&self, project_path: &str) -> bool {
        self.io.project_exists(project_path)
    }

    /// Creates a query builder for a conversation.
    pub fn query<'a>(&self, conversation: &'a Conversation) -> ConversationQuery<'a> {
        ConversationQuery::new(conversation)
    }

    /// Creates a query builder for history entries.
    pub fn query_history<'a>(&self, history: &'a [HistoryEntry]) -> HistoryQuery<'a> {
        HistoryQuery::new(history)
    }

    /// Reads all conversations for a project.
    ///
    /// Returns a vector of conversations sorted by last activity.
    pub fn read_all_conversations(&self, project_path: &str) -> Result<Vec<Conversation>> {
        let session_ids = self.list_conversations(project_path)?;
        let mut conversations = Vec::new();

        for session_id in session_ids {
            match self.read_conversation(project_path, &session_id) {
                Ok(convo) => conversations.push(convo),
                Err(e) => {
                    eprintln!("Warning: Failed to read conversation {}: {}", session_id, e);
                }
            }
        }

        conversations.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        Ok(conversations)
    }

    /// Gets the most recent conversation for a project.
    pub fn most_recent_conversation(&self, project_path: &str) -> Result<Option<Conversation>> {
        let metadata = self.list_conversation_metadata(project_path)?;

        if let Some(latest) = metadata.first() {
            Ok(Some(
                self.read_conversation(project_path, &latest.session_id)?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Finds conversations that contain specific text.
    pub fn find_conversations_with_text(
        &self,
        project_path: &str,
        search_text: &str,
    ) -> Result<Vec<Conversation>> {
        let conversations = self.read_all_conversations(project_path)?;

        Ok(conversations
            .into_iter()
            .filter(|convo| {
                let query = ConversationQuery::new(convo);
                !query.contains_text(search_text).is_empty()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_manager() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        fs::create_dir_all(claude_dir.join("projects/-test-project")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        (temp, manager)
    }

    #[test]
    fn test_basic_setup() {
        let (_temp, manager) = setup_test_manager();
        assert!(manager.exists());
    }

    #[test]
    fn test_list_projects() {
        let (_temp, manager) = setup_test_manager();
        let projects = manager.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0], "/test/project");
    }

    #[test]
    fn test_project_exists() {
        let (_temp, manager) = setup_test_manager();
        assert!(manager.project_exists("/test/project"));
        assert!(!manager.project_exists("/nonexistent"));
    }

    fn setup_test_with_conversation() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"uuid-1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"uuid-2","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there"}}"#;
        fs::write(
            project_dir.join("session-abc.jsonl"),
            format!("{}\n{}\n", entry1, entry2),
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);
        (temp, manager)
    }

    #[test]
    fn test_read_conversation() {
        let (_temp, manager) = setup_test_with_conversation();
        let convo = manager
            .read_conversation("/test/project", "session-abc")
            .unwrap();
        assert_eq!(convo.entries.len(), 2);
        assert_eq!(convo.message_count(), 2);
    }

    #[test]
    fn test_read_conversation_metadata() {
        let (_temp, manager) = setup_test_with_conversation();
        let meta = manager
            .read_conversation_metadata("/test/project", "session-abc")
            .unwrap();
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.session_id, "session-abc");
    }

    #[test]
    fn test_list_conversations() {
        let (_temp, manager) = setup_test_with_conversation();
        let sessions = manager.list_conversations("/test/project").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "session-abc");
    }

    #[test]
    fn test_list_conversation_metadata() {
        let (_temp, manager) = setup_test_with_conversation();
        let metadata = manager.list_conversation_metadata("/test/project").unwrap();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].session_id, "session-abc");
    }

    #[test]
    fn test_conversation_exists() {
        let (_temp, manager) = setup_test_with_conversation();
        assert!(
            manager
                .conversation_exists("/test/project", "session-abc")
                .unwrap()
        );
        assert!(
            !manager
                .conversation_exists("/test/project", "nonexistent")
                .unwrap()
        );
    }

    #[test]
    fn test_io_accessor() {
        let (_temp, manager) = setup_test_with_conversation();
        assert!(manager.io().exists());
    }

    #[test]
    fn test_resolver_accessor() {
        let (_temp, manager) = setup_test_with_conversation();
        assert!(manager.resolver().exists());
    }

    #[test]
    fn test_claude_dir_path() {
        let (_temp, manager) = setup_test_with_conversation();
        let path = manager.claude_dir_path().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_read_all_conversations() {
        let (_temp, manager) = setup_test_with_conversation();
        let convos = manager.read_all_conversations("/test/project").unwrap();
        assert_eq!(convos.len(), 1);
    }

    #[test]
    fn test_most_recent_conversation() {
        let (_temp, manager) = setup_test_with_conversation();
        let convo = manager.most_recent_conversation("/test/project").unwrap();
        assert!(convo.is_some());
    }

    #[test]
    fn test_most_recent_conversation_empty() {
        let (_temp, manager) = setup_test_manager();
        // No conversations in this project
        let convo = manager.most_recent_conversation("/test/project").unwrap();
        assert!(convo.is_none());
    }

    #[test]
    fn test_find_conversations_with_text() {
        let (_temp, manager) = setup_test_with_conversation();
        let results = manager
            .find_conversations_with_text("/test/project", "Hello")
            .unwrap();
        assert_eq!(results.len(), 1);

        let no_results = manager
            .find_conversations_with_text("/test/project", "nonexistent text xyz")
            .unwrap();
        assert!(no_results.is_empty());
    }

    #[test]
    fn test_query_helper() {
        let (_temp, manager) = setup_test_with_conversation();
        let convo = manager
            .read_conversation("/test/project", "session-abc")
            .unwrap();
        let q = manager.query(&convo);
        let users = q.by_role(MessageRole::User);
        assert_eq!(users.len(), 1);
    }

    #[test]
    fn test_query_history_helper() {
        let (_temp, manager) = setup_test_manager();
        let history: Vec<HistoryEntry> = vec![];
        let q = manager.query_history(&history);
        let results = q.recent(5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_read_history_no_file() {
        let (_temp, manager) = setup_test_manager();
        let history = manager.read_history().unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_default_impl() {
        // Test that Default trait works
        let _manager = ClaudeConvo::default();
    }
}
