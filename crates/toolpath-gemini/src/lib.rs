#![doc = include_str!("../README.md")]

pub mod derive;
pub mod error;
pub mod io;
pub mod paths;
pub mod project;
pub mod provider;
pub mod query;
pub mod reader;
pub mod types;

#[cfg(feature = "watcher")]
pub mod watcher;

pub use error::{ConvoError, Result};
pub use io::ConvoIO;
pub use paths::{PathResolver, SessionEntry};
pub use query::ConversationQuery;
pub use reader::ConversationReader;
pub use types::{
    ChatFile, Conversation, ConversationMetadata, FunctionResponse, FunctionResponseBody,
    GeminiContent, GeminiMessage, GeminiRole, LogEntry, TextPart, Thought, Tokens, ToolCall,
};

#[cfg(feature = "watcher")]
pub use watcher::ConversationWatcher;

/// High-level entry point for reading Gemini CLI conversations.
///
/// `GeminiConvo` is chain-unaware by design — Gemini doesn't rotate
/// files. Instead, a "conversation" is a session UUID directory: the
/// main chat file plus every sibling sub-agent chat file.
///
/// # Example
///
/// ```rust,no_run
/// use toolpath_gemini::GeminiConvo;
///
/// let manager = GeminiConvo::new();
/// let projects = manager.list_projects()?;
/// let convo = manager.read_conversation(
///     "/Users/alex/project",
///     "session-uuid",
/// )?;
/// println!("{} messages", convo.total_message_count());
/// # Ok::<(), toolpath_gemini::ConvoError>(())
/// ```
#[derive(Debug, Clone)]
pub struct GeminiConvo {
    io: ConvoIO,
}

impl Default for GeminiConvo {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiConvo {
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn exists(&self) -> bool {
        self.io.exists()
    }

    pub fn gemini_dir_path(&self) -> Result<std::path::PathBuf> {
        self.io.gemini_dir_path()
    }

    pub fn list_projects(&self) -> Result<Vec<String>> {
        self.io.list_projects()
    }

    pub fn project_exists(&self, project_path: &str) -> bool {
        self.io.project_exists(project_path)
    }

    /// List session UUIDs for a project (each corresponds to one
    /// `chats/<uuid>/` directory).
    pub fn list_conversations(&self, project_path: &str) -> Result<Vec<String>> {
        self.io.list_sessions(project_path)
    }

    /// Metadata for every session in a project, sorted newest first.
    pub fn list_conversation_metadata(
        &self,
        project_path: &str,
    ) -> Result<Vec<ConversationMetadata>> {
        self.io.list_session_metadata(project_path)
    }

    /// List chat-file stems for a given session UUID.
    pub fn list_chat_files(&self, project_path: &str, session_uuid: &str) -> Result<Vec<String>> {
        self.io.list_chat_files(project_path, session_uuid)
    }

    /// Read a full conversation — the main chat plus every sibling
    /// sub-agent chat file.
    pub fn read_conversation(
        &self,
        project_path: &str,
        session_uuid: &str,
    ) -> Result<Conversation> {
        self.io.read_session(project_path, session_uuid)
    }

    /// Read a single chat file without pulling in siblings.
    pub fn read_chat_file(
        &self,
        project_path: &str,
        session_uuid: &str,
        chat_name: &str,
    ) -> Result<ChatFile> {
        self.io.read_chat(project_path, session_uuid, chat_name)
    }

    pub fn read_conversation_metadata(
        &self,
        project_path: &str,
        session_uuid: &str,
    ) -> Result<ConversationMetadata> {
        self.io.read_session_metadata(project_path, session_uuid)
    }

    pub fn conversation_exists(&self, project_path: &str, session_uuid: &str) -> Result<bool> {
        self.io.session_exists(project_path, session_uuid)
    }

    /// Read every conversation in a project, sorted by last activity.
    pub fn read_all_conversations(&self, project_path: &str) -> Result<Vec<Conversation>> {
        let sessions = self.list_conversations(project_path)?;
        let mut out = Vec::new();
        for uuid in sessions {
            match self.read_conversation(project_path, &uuid) {
                Ok(c) => out.push(c),
                Err(e) => eprintln!("Warning: Failed to read conversation {}: {}", uuid, e),
            }
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.last_activity));
        Ok(out)
    }

    pub fn most_recent_conversation(&self, project_path: &str) -> Result<Option<Conversation>> {
        let metas = self.list_conversation_metadata(project_path)?;
        match metas.first() {
            Some(m) => Ok(Some(self.read_conversation(project_path, &m.session_uuid)?)),
            None => Ok(None),
        }
    }

    /// Case-insensitive substring search across all conversations in a
    /// project. Returns conversations that contain a match.
    pub fn find_conversations_with_text(
        &self,
        project_path: &str,
        search_text: &str,
    ) -> Result<Vec<Conversation>> {
        let conversations = self.read_all_conversations(project_path)?;
        Ok(conversations
            .into_iter()
            .filter(|c| {
                let q = ConversationQuery::new(c);
                !q.contains_text(search_text).is_empty()
            })
            .collect())
    }

    pub fn query<'a>(&self, conversation: &'a Conversation) -> ConversationQuery<'a> {
        ConversationQuery::new(conversation)
    }

    pub fn read_logs(&self, project_path: &str) -> Result<Vec<LogEntry>> {
        self.io.read_logs(project_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, GeminiConvo) {
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let project_slot = gemini.join("tmp/myrepo");
        let session_dir = project_slot.join("chats/session-uuid");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();

        fs::write(
            session_dir.join("main.json"),
            r#"{
  "sessionId":"main-s",
  "projectHash":"h",
  "startTime":"2026-04-17T15:00:00Z",
  "lastUpdated":"2026-04-17T15:10:00Z",
  "directories":["/abs/myrepo"],
  "messages":[
    {"id":"m1","timestamp":"2026-04-17T15:00:00Z","type":"user","content":[{"text":"Hello"}]},
    {"id":"m2","timestamp":"2026-04-17T15:00:01Z","type":"gemini","content":"Hi","model":"gemini-3-flash-preview"}
  ]
}"#,
        )
        .unwrap();

        let resolver = PathResolver::new().with_gemini_dir(&gemini);
        (temp, GeminiConvo::with_resolver(resolver))
    }

    #[test]
    fn test_list_projects() {
        let (_t, mgr) = setup();
        assert_eq!(
            mgr.list_projects().unwrap(),
            vec!["/abs/myrepo".to_string()]
        );
    }

    #[test]
    fn test_list_conversations() {
        let (_t, mgr) = setup();
        let sessions = mgr.list_conversations("/abs/myrepo").unwrap();
        assert_eq!(sessions, vec!["session-uuid".to_string()]);
    }

    #[test]
    fn test_read_conversation() {
        let (_t, mgr) = setup();
        let c = mgr
            .read_conversation("/abs/myrepo", "session-uuid")
            .unwrap();
        assert_eq!(c.main.messages.len(), 2);
        assert!(c.sub_agents.is_empty());
    }

    #[test]
    fn test_read_conversation_metadata() {
        let (_t, mgr) = setup();
        let meta = mgr
            .read_conversation_metadata("/abs/myrepo", "session-uuid")
            .unwrap();
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.sub_agent_count, 0);
    }

    #[test]
    fn test_most_recent_conversation() {
        let (_t, mgr) = setup();
        let c = mgr.most_recent_conversation("/abs/myrepo").unwrap();
        assert!(c.is_some());
        assert_eq!(c.unwrap().main.session_id, "main-s");
    }

    #[test]
    fn test_most_recent_conversation_empty() {
        let (_t, mgr) = setup();
        let c = mgr.most_recent_conversation("/nonexistent").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn test_read_all_conversations_sorted() {
        let (t, mgr) = setup();
        let gemini = t.path().join(".gemini");
        let second = gemini.join("tmp/myrepo/chats/session-b");
        fs::create_dir_all(&second).unwrap();
        fs::write(
            second.join("main.json"),
            r#"{"sessionId":"b","projectHash":"","startTime":"2026-04-20T00:00:00Z","lastUpdated":"2026-04-20T00:00:00Z","messages":[]}"#,
        )
        .unwrap();
        let all = mgr.read_all_conversations("/abs/myrepo").unwrap();
        assert_eq!(all.len(), 2);
        // The b session is newer; should come first
        assert_eq!(all[0].main.session_id, "b");
    }

    #[test]
    fn test_find_conversations_with_text() {
        let (_t, mgr) = setup();
        let results = mgr
            .find_conversations_with_text("/abs/myrepo", "Hello")
            .unwrap();
        assert_eq!(results.len(), 1);
        let none = mgr
            .find_conversations_with_text("/abs/myrepo", "unrelated xyzzy")
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn test_query_helper() {
        let (_t, mgr) = setup();
        let c = mgr
            .read_conversation("/abs/myrepo", "session-uuid")
            .unwrap();
        let q = mgr.query(&c);
        assert_eq!(q.by_role(GeminiRole::User).len(), 1);
    }

    #[test]
    fn test_conversation_exists() {
        let (_t, mgr) = setup();
        assert!(
            mgr.conversation_exists("/abs/myrepo", "session-uuid")
                .unwrap()
        );
        assert!(!mgr.conversation_exists("/abs/myrepo", "nope").unwrap());
    }

    #[test]
    fn test_gemini_dir_path() {
        let (t, mgr) = setup();
        assert_eq!(mgr.gemini_dir_path().unwrap(), t.path().join(".gemini"));
    }

    #[test]
    fn test_list_chat_files() {
        let (_t, mgr) = setup();
        let files = mgr.list_chat_files("/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(files, vec!["main".to_string()]);
    }

    #[test]
    fn test_default() {
        let _mgr = GeminiConvo::default();
    }

    #[test]
    fn test_project_exists() {
        let (_t, mgr) = setup();
        assert!(mgr.project_exists("/abs/myrepo"));
        assert!(!mgr.project_exists("/never"));
    }
}
