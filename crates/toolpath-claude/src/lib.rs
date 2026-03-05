#![doc = include_str!("../README.md")]

#[cfg(feature = "watcher")]
pub mod async_watcher;
pub(crate) mod chain;
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
    HistoryEntry, Message, MessageContent, MessageRole, ToolResultContent, ToolResultRef,
    ToolUseRef, Usage,
};
#[cfg(feature = "watcher")]
pub use watcher::ConversationWatcher;

/// High-level interface for reading Claude conversations.
///
/// This is the primary entry point for most use cases. It provides
/// convenient methods for reading conversations, listing projects,
/// and accessing conversation history.
///
/// **Chain-default:** `read_conversation` and `list_conversations` operate
/// on logical conversations (merged session chains). Use `read_segment`
/// and `list_segments` for single-file access.
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
/// // Read a conversation (follows session chains automatically)
/// let convo = manager.read_conversation(
///     "/Users/alex/project",
///     "session-uuid"
/// )?;
///
/// println!("Conversation has {} messages", convo.message_count());
/// # Ok::<(), toolpath_claude::ConvoError>(())
/// ```
#[derive(Debug)]
pub struct ClaudeConvo {
    io: ConvoIO,
    chain_cache: std::cell::RefCell<std::collections::HashMap<String, chain::ChainIndex>>,
}

impl Clone for ClaudeConvo {
    fn clone(&self) -> Self {
        Self {
            io: self.io.clone(),
            chain_cache: std::cell::RefCell::new(self.chain_cache.borrow().clone()),
        }
    }
}

impl Default for ClaudeConvo {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeConvo {
    /// Creates a new ClaudeConvo manager with default path resolution.
    pub fn new() -> Self {
        Self {
            io: ConvoIO::new(),
            chain_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
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
            chain_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
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
    /// **Chain-aware:** if this session is part of a chain (file rotation),
    /// all segments are merged into a single `Conversation` with bridge
    /// entries filtered out and `session_ids` populated.
    ///
    /// Use [`Self::read_segment`] for single-file access.
    pub fn read_conversation(&self, project_path: &str, session_id: &str) -> Result<Conversation> {
        let chain = self.chain_for(project_path, session_id)?;

        if chain.len() <= 1 {
            return self.io.read_conversation(project_path, session_id);
        }

        // Multi-segment: merge all segments
        let head = &chain[0];
        let mut merged = Conversation::new(head.clone());

        for segment_id in &chain {
            let convo = self.io.read_conversation(project_path, segment_id)?;

            if merged.started_at.is_none() {
                merged.started_at = convo.started_at;
            }
            merged.last_activity = convo.last_activity.or(merged.last_activity);
            if merged.project_path.is_none() {
                merged.project_path = convo.project_path.clone();
            }

            for entry in &convo.entries {
                if chain::is_bridge_entry(entry, segment_id) {
                    continue;
                }
                merged.add_entry(entry.clone());
            }
        }

        merged.session_ids = chain;
        Ok(merged)
    }

    /// Reads conversation metadata without loading the full content.
    ///
    /// **Chain-aware:** aggregates `message_count` (sum), `started_at`
    /// (earliest), and `last_activity` (latest) across all segments.
    pub fn read_conversation_metadata(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<ConversationMetadata> {
        let chain = self.chain_for(project_path, session_id)?;

        if chain.len() <= 1 {
            return self.io.read_conversation_metadata(project_path, session_id);
        }

        let head = &chain[0];
        let mut total_messages = 0usize;
        let mut started_at = None;
        let mut last_activity = None;
        let mut project_path_val = String::new();
        let mut file_path = std::path::PathBuf::new();

        for (i, segment_id) in chain.iter().enumerate() {
            let meta = self
                .io
                .read_conversation_metadata(project_path, segment_id)?;
            total_messages += meta.message_count;

            if started_at.is_none() || meta.started_at < started_at {
                started_at = meta.started_at;
            }
            if last_activity.is_none() || meta.last_activity > last_activity {
                last_activity = meta.last_activity;
            }
            if project_path_val.is_empty() {
                project_path_val = meta.project_path;
            }
            if i == 0 {
                file_path = meta.file_path;
            }
        }

        Ok(ConversationMetadata {
            session_id: head.clone(),
            project_path: project_path_val,
            file_path,
            message_count: total_messages,
            started_at,
            last_activity,
        })
    }

    /// Lists logical conversation IDs for a project (chain heads only).
    ///
    /// Chained sessions collapse to a single entry (the head).
    /// Use [`Self::list_segments`] for all file stems.
    pub fn list_conversations(&self, project_path: &str) -> Result<Vec<String>> {
        self.chain_heads(project_path)
    }

    /// Lists metadata for all logical conversations in a project.
    ///
    /// Chain heads only. Results are sorted by last activity (most recent first).
    pub fn list_conversation_metadata(
        &self,
        project_path: &str,
    ) -> Result<Vec<ConversationMetadata>> {
        let heads = self.chain_heads(project_path)?;
        let mut metadata = Vec::new();

        for session_id in heads {
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

    // ── Single-file access (opt-in) ──────────────────────────────────

    /// Reads a single JSONL file without following chains.
    pub fn read_segment(&self, project_path: &str, session_id: &str) -> Result<Conversation> {
        self.io.read_conversation(project_path, session_id)
    }

    /// Lists all file stems (including successor segments).
    pub fn list_segments(&self, project_path: &str) -> Result<Vec<String>> {
        self.io.list_conversations(project_path)
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

    /// Resolves the full session chain containing `session_id`, returned
    /// in chronological order (oldest segment first).
    ///
    /// For single-segment sessions, returns `[session_id]`.
    #[allow(dead_code)]
    pub(crate) fn session_chain(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<Vec<String>> {
        self.chain_for(project_path, session_id)
    }

    /// Returns the chain head (earliest segment) for `session_id`.
    ///
    /// For single-segment sessions, returns `session_id` unchanged.
    #[allow(dead_code)]
    pub(crate) fn chain_head(&self, project_path: &str, session_id: &str) -> Result<String> {
        let chain = self.session_chain(project_path, session_id)?;
        Ok(chain
            .into_iter()
            .next()
            .unwrap_or_else(|| session_id.to_string()))
    }

    // ── Private helpers ──────────────────────────────────────────────

    /// Refresh the chain index for `project_path` and resolve the chain
    /// for `session_id`. RefCell borrow is scoped internally.
    fn chain_for(&self, project_path: &str, session_id: &str) -> Result<Vec<String>> {
        let mut cache = self.chain_cache.borrow_mut();
        let index = cache
            .entry(project_path.to_string())
            .or_insert_with(chain::ChainIndex::new);
        index.refresh(self.resolver(), project_path)?;
        Ok(index.resolve_chain(session_id))
    }

    /// Refresh the chain index and return chain heads.
    fn chain_heads(&self, project_path: &str) -> Result<Vec<String>> {
        let mut cache = self.chain_cache.borrow_mut();
        let index = cache
            .entry(project_path.to_string())
            .or_insert_with(chain::ChainIndex::new);
        index.refresh(self.resolver(), project_path)?;
        Ok(index.chain_heads())
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

    // ── Session chain convenience methods ────────────────────────────

    fn setup_chained_conversations() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // session-a: standalone start
        fs::write(
            project_dir.join("session-a.jsonl"),
            r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Start"}}"#,
        ).unwrap();

        // session-b: successor of a (bridge entry points to a)
        let b = vec![
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"b1","type":"user","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"Middle"}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), b.join("\n")).unwrap();

        // session-c: successor of b
        let c = vec![
            r#"{"uuid":"c0","type":"user","timestamp":"2024-01-01T02:00:00Z","sessionId":"session-b","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"c1","type":"user","timestamp":"2024-01-01T02:00:01Z","sessionId":"session-c","message":{"role":"user","content":"End"}}"#,
        ];
        fs::write(project_dir.join("session-c.jsonl"), c.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn test_session_chain_full() {
        let (_temp, manager) = setup_chained_conversations();
        let chain = manager.session_chain("/test/project", "session-a").unwrap();
        assert_eq!(chain, vec!["session-a", "session-b", "session-c"]);
    }

    #[test]
    fn test_session_chain_from_middle() {
        let (_temp, manager) = setup_chained_conversations();
        let chain = manager.session_chain("/test/project", "session-b").unwrap();
        assert_eq!(chain, vec!["session-a", "session-b", "session-c"]);
    }

    #[test]
    fn test_session_chain_single() {
        let (_temp, manager) = setup_test_with_conversation();
        let chain = manager
            .session_chain("/test/project", "session-abc")
            .unwrap();
        assert_eq!(chain, vec!["session-abc"]);
    }

    #[test]
    fn test_chain_head_from_tail() {
        let (_temp, manager) = setup_chained_conversations();
        let head = manager.chain_head("/test/project", "session-c").unwrap();
        assert_eq!(head, "session-a");
    }

    #[test]
    fn test_chain_head_already_head() {
        let (_temp, manager) = setup_chained_conversations();
        let head = manager.chain_head("/test/project", "session-a").unwrap();
        assert_eq!(head, "session-a");
    }

    #[test]
    fn test_chain_head_single_session() {
        let (_temp, manager) = setup_test_with_conversation();
        let head = manager.chain_head("/test/project", "session-abc").unwrap();
        assert_eq!(head, "session-abc");
    }

    // ── Chain-default API tests ──────────────────────────────────────

    #[test]
    fn test_read_conversation_follows_chain() {
        let (_temp, manager) = setup_chained_conversations();

        // Reading from any segment returns the full merged conversation
        let convo = manager
            .read_conversation("/test/project", "session-a")
            .unwrap();
        assert_eq!(convo.session_id, "session-a");
        assert_eq!(
            convo.session_ids,
            vec!["session-a", "session-b", "session-c"]
        );
        // a1, b1, c1 (bridge entries b0 and c0 filtered out)
        assert_eq!(convo.entries.len(), 3);
        assert_eq!(convo.entries[0].uuid, "a1");
        assert_eq!(convo.entries[1].uuid, "b1");
        assert_eq!(convo.entries[2].uuid, "c1");

        // From the middle
        let convo_b = manager
            .read_conversation("/test/project", "session-b")
            .unwrap();
        assert_eq!(
            convo_b.session_ids,
            vec!["session-a", "session-b", "session-c"]
        );
        assert_eq!(convo_b.entries.len(), 3);

        // From the tail
        let convo_c = manager
            .read_conversation("/test/project", "session-c")
            .unwrap();
        assert_eq!(convo_c.entries.len(), 3);
    }

    #[test]
    fn test_list_conversations_returns_chain_heads() {
        let (_temp, manager) = setup_chained_conversations();

        let sessions = manager.list_conversations("/test/project").unwrap();
        // Three files but only one chain head
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&"session-a".to_string()));
    }

    #[test]
    fn test_read_segment_single_file() {
        let (_temp, manager) = setup_chained_conversations();

        // read_segment returns only the single file, not merged
        let segment = manager.read_segment("/test/project", "session-b").unwrap();
        assert_eq!(segment.session_id, "session-b");
        assert_eq!(segment.entries.len(), 2); // b0 (bridge) + b1
        assert!(segment.session_ids.is_empty());
    }

    #[test]
    fn test_list_segments_returns_all() {
        let (_temp, manager) = setup_chained_conversations();

        let mut segments = manager.list_segments("/test/project").unwrap();
        segments.sort();
        assert_eq!(segments, vec!["session-a", "session-b", "session-c"]);
    }

    #[test]
    fn test_read_conversation_metadata_aggregates_chain() {
        let (_temp, manager) = setup_chained_conversations();

        let meta = manager
            .read_conversation_metadata("/test/project", "session-a")
            .unwrap();
        assert_eq!(meta.session_id, "session-a");
        // a: 1 msg, b: 2 msgs, c: 2 msgs = 5 total
        assert_eq!(meta.message_count, 5);
        // started_at from first segment, last_activity from last
        assert!(meta.started_at.is_some());
        assert!(meta.last_activity.is_some());
        assert!(meta.last_activity > meta.started_at);
    }
}
