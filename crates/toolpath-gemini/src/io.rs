//! Higher-level filesystem operations over `PathResolver`.

use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::ConversationReader;
use crate::types::{ChatFile, Conversation, ConversationMetadata, GeminiRole, LogEntry};
use std::path::PathBuf;

/// First non-empty `"user"` prompt in a chat file, used as a session "title".
fn first_user_text(chat: &ChatFile) -> Option<String> {
    chat.messages
        .iter()
        .filter(|m| m.role == GeminiRole::User)
        .find_map(|m| {
            let text = m.content.text();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
}

#[derive(Debug, Clone)]
pub struct ConvoIO {
    resolver: PathResolver,
}

impl ConvoIO {
    pub fn new<P: Into<std::path::PathBuf>>(home: P) -> Self {
        Self {
            resolver: PathResolver::new(home),
        }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn gemini_dir_path(&self) -> PathBuf {
        self.resolver.gemini_dir()
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    pub fn list_projects(&self) -> Result<Vec<String>> {
        self.resolver.list_project_dirs()
    }

    pub fn list_sessions(&self, project_path: &str) -> Result<Vec<String>> {
        self.resolver.list_sessions(project_path)
    }

    pub fn list_chat_files(&self, project_path: &str, session_uuid: &str) -> Result<Vec<String>> {
        self.resolver.list_chat_files(project_path, session_uuid)
    }

    pub fn project_exists(&self, project_path: &str) -> bool {
        self.resolver
            .project_dir(project_path)
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    pub fn session_exists(&self, project_path: &str, session_id: &str) -> Result<bool> {
        // A session is "present" if ANY of: a main session file with that
        // stem, a main session file whose inner sessionId matches, or a
        // UUID directory of that name exists.
        if self
            .resolver
            .resolve_main_file(project_path, session_id)?
            .is_some()
        {
            return Ok(true);
        }
        let dir = self.resolver.session_dir(project_path, session_id)?;
        Ok(dir.exists())
    }

    /// Read a single chat file by name.
    pub fn read_chat(
        &self,
        project_path: &str,
        session_uuid: &str,
        chat_name: &str,
    ) -> Result<ChatFile> {
        let path = self
            .resolver
            .chat_file(project_path, session_uuid, chat_name)?;
        ConversationReader::read_chat_file(&path)
    }

    /// Read every chat file inside a session UUID directory.
    pub fn read_all_chats(
        &self,
        project_path: &str,
        session_uuid: &str,
    ) -> Result<Vec<(String, ChatFile)>> {
        let stems = self.list_chat_files(project_path, session_uuid)?;
        let mut out = Vec::with_capacity(stems.len());
        for stem in stems {
            let chat = self.read_chat(project_path, session_uuid, &stem)?;
            out.push((stem, chat));
        }
        Ok(out)
    }

    /// Load a full session.
    ///
    /// `session_id` may be:
    /// - A main-session file stem (e.g. `session-2026-04-17T18-09-b26d7f99`)
    ///   — the file is read, and a sibling `<inner-sessionId>/` dir (if
    ///   present) contributes sub-agent chats.
    /// - A full session UUID (the `sessionId` field inside a main chat
    ///   file, e.g. `f7cc36c0-980c-4914-ae79-439567272478`) — `chats/*.json`
    ///   is scanned for a file whose inner `sessionId` matches. This is
    ///   how Gemini CLI itself resolves `--resume <uuid>`.
    /// - A UUID directory name with no backing main file — every
    ///   `*.json` file inside is loaded; the one without `kind: "subagent"`
    ///   becomes the main.
    pub fn read_session(&self, project_path: &str, session_id: &str) -> Result<Conversation> {
        // Strategy A: resolve the main file either by file stem or by
        // inner sessionId.
        if let Some(main_path) = self.resolver.resolve_main_file(project_path, session_id)? {
            let main = ConversationReader::read_chat_file(&main_path)?;
            let uuid = main.session_id.clone();
            let sub_agents = if !uuid.is_empty() {
                let uuid_dir = self.resolver.session_dir(project_path, &uuid)?;
                if uuid_dir.exists() {
                    let stems = self.resolver.list_chat_files(project_path, &uuid)?;
                    let mut subs = Vec::with_capacity(stems.len());
                    for stem in stems {
                        match self.read_chat(project_path, &uuid, &stem) {
                            Ok(c) => subs.push(c),
                            Err(e) => eprintln!(
                                "Warning: failed to read sub-agent {}/{}: {}",
                                uuid, stem, e
                            ),
                        }
                    }
                    subs
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            let project_root: Option<String> = main
                .directories()
                .first()
                .map(|p| p.to_string_lossy().to_string());

            let mut convo = Conversation::new(session_id.to_string(), main);
            convo.project_path = project_root;
            convo.sub_agents = sub_agents;
            return Ok(convo);
        }

        // Strategy B: treat session_id as a UUID directory.
        let chats = self.read_all_chats(project_path, session_id)?;
        if chats.is_empty() {
            return Err(crate::error::ConvoError::ConversationNotFound(format!(
                "{}/{}",
                project_path, session_id
            )));
        }

        let (main_idx, _) = chats
            .iter()
            .enumerate()
            .find(|(_, (_, c))| c.kind.as_deref() != Some("subagent"))
            .unwrap_or((0, &chats[0]));

        let mut chats = chats;
        let (_, main) = chats.remove(main_idx);
        let sub_agents: Vec<ChatFile> = chats.into_iter().map(|(_, c)| c).collect();

        let project_root: Option<String> = main
            .directories()
            .first()
            .map(|p| p.to_string_lossy().to_string());

        let mut convo = Conversation::new(session_id.to_string(), main);
        convo.project_path = project_root;
        convo.sub_agents = sub_agents;
        Ok(convo)
    }

    /// Lightweight metadata for a single session.
    ///
    /// Accepts any identifier [`ConvoIO::read_session`] accepts:
    /// filename stem, inner session UUID, or a bare UUID directory name.
    pub fn read_session_metadata(
        &self,
        project_path: &str,
        session_id: &str,
    ) -> Result<ConversationMetadata> {
        // Case A: main session file resolvable by stem or inner sessionId.
        if let Some(main_path) = self.resolver.resolve_main_file(project_path, session_id)? {
            let main = ConversationReader::read_chat_file(&main_path)?;
            let uuid = main.session_id.clone();
            let mut sub_chats: Vec<ChatFile> = Vec::new();
            if !uuid.is_empty() {
                let uuid_dir = self.resolver.session_dir(project_path, &uuid)?;
                if uuid_dir.exists() {
                    for stem in self.resolver.list_chat_files(project_path, &uuid)? {
                        if let Ok(c) = self.read_chat(project_path, &uuid, &stem) {
                            sub_chats.push(c);
                        }
                    }
                }
            }
            let mut message_count = main.messages.len();
            for s in &sub_chats {
                message_count += s.messages.len();
            }
            let mut started_at = main.start_time;
            let mut last_activity = main.last_updated;
            for s in &sub_chats {
                if let Some(t) = s.start_time
                    && started_at.map(|x| t < x).unwrap_or(true)
                {
                    started_at = Some(t);
                }
                if let Some(t) = s.last_updated
                    && last_activity.map(|x| t > x).unwrap_or(true)
                {
                    last_activity = Some(t);
                }
            }
            let sub_agent_count = sub_chats
                .iter()
                .filter(|c| c.kind.as_deref() == Some("subagent"))
                .count();
            let project_root: String = main
                .directories()
                .first()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| project_path.to_string());
            let first_user_message = first_user_text(&main);
            return Ok(ConversationMetadata {
                session_uuid: session_id.to_string(),
                project_path: project_root,
                file_path: main_path,
                message_count,
                started_at,
                last_activity,
                sub_agent_count,
                first_user_message,
            });
        }

        // Case B: orphan UUID directory.
        let chats = self.read_all_chats(project_path, session_id)?;
        let session_dir = self.resolver.session_dir(project_path, session_id)?;

        let main = chats
            .iter()
            .find(|(_, c)| c.kind.as_deref() != Some("subagent"))
            .or_else(|| chats.first())
            .ok_or_else(|| {
                crate::error::ConvoError::ConversationNotFound(format!(
                    "{}/{}",
                    project_path, session_id
                ))
            })?;

        let message_count: usize = chats.iter().map(|(_, c)| c.messages.len()).sum();
        let started_at = chats.iter().filter_map(|(_, c)| c.start_time).min();
        let last_activity = chats.iter().filter_map(|(_, c)| c.last_updated).max();
        let sub_agent_count = chats
            .iter()
            .filter(|(_, c)| c.kind.as_deref() == Some("subagent"))
            .count();

        let project_root: String = main
            .1
            .directories()
            .first()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| project_path.to_string());

        let first_user_message = first_user_text(&main.1);

        Ok(ConversationMetadata {
            session_uuid: session_id.to_string(),
            project_path: project_root,
            file_path: session_dir,
            message_count,
            started_at,
            last_activity,
            sub_agent_count,
            first_user_message,
        })
    }

    pub fn list_session_metadata(&self, project_path: &str) -> Result<Vec<ConversationMetadata>> {
        let sessions = self.list_sessions(project_path)?;
        let mut out = Vec::new();
        for uuid in sessions {
            match self.read_session_metadata(project_path, &uuid) {
                Ok(meta) => out.push(meta),
                Err(e) => eprintln!("Warning: Failed to read metadata for {}: {}", uuid, e),
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        Ok(out)
    }

    pub fn read_logs(&self, project_path: &str) -> Result<Vec<LogEntry>> {
        let path = self.resolver.logs_file(project_path)?;
        ConversationReader::read_logs(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ConvoIO) {
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
        fs::write(project_slot.join(".project_root"), "/abs/myrepo").unwrap();

        let main = r#"{
  "sessionId":"main-s",
  "projectHash":"h",
  "startTime":"2026-04-17T15:00:00Z",
  "lastUpdated":"2026-04-17T15:10:00Z",
  "directories":["/abs/myrepo"],
  "messages":[
    {"id":"m1","timestamp":"2026-04-17T15:00:00Z","type":"user","content":[{"text":"Fix the bug"}]},
    {"id":"m2","timestamp":"2026-04-17T15:01:00Z","type":"gemini","content":"Sure.","model":"gemini-3-flash-preview"}
  ]
}"#;
        fs::write(session_dir.join("main.json"), main).unwrap();

        let sub = r#"{
  "sessionId":"sub-s",
  "projectHash":"h",
  "startTime":"2026-04-17T15:05:00Z",
  "lastUpdated":"2026-04-17T15:08:00Z",
  "kind":"subagent",
  "summary":"found it",
  "messages":[
    {"id":"s1","timestamp":"2026-04-17T15:05:00Z","type":"user","content":[{"text":"Search"}]}
  ]
}"#;
        fs::write(session_dir.join("sub-s.json"), sub).unwrap();

        let resolver = PathResolver::new(temp.path()).with_gemini_dir(&gemini);
        (temp, ConvoIO::with_resolver(resolver))
    }

    #[test]
    fn test_list_projects() {
        let (_t, io) = setup();
        let p = io.list_projects().unwrap();
        assert_eq!(p, vec!["/abs/myrepo".to_string()]);
    }

    #[test]
    fn test_list_sessions() {
        let (_t, io) = setup();
        let s = io.list_sessions("/abs/myrepo").unwrap();
        assert_eq!(s, vec!["session-uuid".to_string()]);
    }

    #[test]
    fn test_list_chat_files() {
        let (_t, io) = setup();
        let files = io.list_chat_files("/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(files, vec!["main".to_string(), "sub-s".to_string()]);
    }

    #[test]
    fn test_read_session_picks_main() {
        let (_t, io) = setup();
        let convo = io.read_session("/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(convo.main.session_id, "main-s");
        assert!(convo.main.kind.is_none());
        assert_eq!(convo.sub_agents.len(), 1);
        assert_eq!(convo.sub_agents[0].session_id, "sub-s");
        assert_eq!(convo.sub_agents[0].summary.as_deref(), Some("found it"));
        assert_eq!(convo.project_path.as_deref(), Some("/abs/myrepo"));
    }

    #[test]
    fn test_read_session_metadata() {
        let (_t, io) = setup();
        let meta = io
            .read_session_metadata("/abs/myrepo", "session-uuid")
            .unwrap();
        assert_eq!(meta.session_uuid, "session-uuid");
        assert_eq!(meta.message_count, 3); // 2 main + 1 sub-agent
        assert_eq!(meta.sub_agent_count, 1);
        assert!(meta.started_at.is_some());
        assert!(meta.last_activity.is_some());
    }

    #[test]
    fn test_list_session_metadata() {
        let (_t, io) = setup();
        let metas = io.list_session_metadata("/abs/myrepo").unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].session_uuid, "session-uuid");
    }

    #[test]
    fn test_read_chat_by_name() {
        let (_t, io) = setup();
        let chat = io
            .read_chat("/abs/myrepo", "session-uuid", "sub-s")
            .unwrap();
        assert_eq!(chat.kind.as_deref(), Some("subagent"));
    }

    #[test]
    fn test_session_exists() {
        let (_t, io) = setup();
        assert!(io.session_exists("/abs/myrepo", "session-uuid").unwrap());
        assert!(!io.session_exists("/abs/myrepo", "missing").unwrap());
    }

    #[test]
    fn test_project_exists() {
        let (_t, io) = setup();
        assert!(io.project_exists("/abs/myrepo"));
        assert!(!io.project_exists("/never"));
    }

    #[test]
    fn test_read_session_missing() {
        let (_t, io) = setup();
        let err = io.read_session("/abs/myrepo", "missing").unwrap_err();
        matches!(err, crate::error::ConvoError::ConversationNotFound(_));
    }

    #[test]
    fn test_read_logs_absent() {
        let (_t, io) = setup();
        let logs = io.read_logs("/abs/myrepo").unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_read_logs_present() {
        let (t, io) = setup();
        fs::write(
            t.path().join(".gemini/tmp/myrepo/logs.json"),
            r#"[{"sessionId":"s","messageId":0,"type":"user","message":"hi","timestamp":"t"}]"#,
        )
        .unwrap();
        let logs = io.read_logs("/abs/myrepo").unwrap();
        assert_eq!(logs.len(), 1);
    }

    #[test]
    fn test_read_session_only_subagents_uses_first() {
        // Edge case: session dir where every file is a sub-agent.
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let session = gemini.join("tmp/p/chats/sess");
        fs::create_dir_all(&session).unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        fs::write(
            session.join("a.json"),
            r#"{"sessionId":"a","kind":"subagent","messages":[]}"#,
        )
        .unwrap();
        fs::write(
            session.join("b.json"),
            r#"{"sessionId":"b","kind":"subagent","messages":[]}"#,
        )
        .unwrap();

        let io = ConvoIO::with_resolver(PathResolver::new(temp.path()).with_gemini_dir(&gemini));
        let convo = io.read_session("/p", "sess").unwrap();
        // Fell back to the first file as "main"
        assert_eq!(convo.sub_agents.len(), 1);
    }

    // ── Real-world layout: flat main file + sibling <uuid>/ sub-agent dir ──

    /// Build the canonical real-world layout:
    ///   chats/session-<ts>-<short>.json       (kind: "main")
    ///   chats/<full-uuid>/<name>.json         (kind: "subagent")
    fn setup_main_with_sibling_subagent() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();

        // Main session file at top of chats/
        fs::write(
            chats.join("session-2026-04-17-b26d.json"),
            r#"{
  "sessionId":"b26d-full-uuid-abc",
  "projectHash":"h",
  "kind":"main",
  "startTime":"2026-04-17T10:00:00Z",
  "lastUpdated":"2026-04-17T10:20:00Z",
  "directories":["/abs/p"],
  "messages":[
    {"id":"u1","timestamp":"2026-04-17T10:00:00Z","type":"user","content":[{"text":"go"}]},
    {"id":"a1","timestamp":"2026-04-17T10:00:01Z","type":"gemini","content":"delegating","model":"gemini-3-flash-preview","toolCalls":[
      {"id":"t","name":"task","args":{"prompt":"search"},"status":"success","timestamp":"2026-04-17T10:00:01Z","result":[{"functionResponse":{"id":"t","name":"task","response":{"output":"done"}}}]}
    ]}
  ]
}"#,
        )
        .unwrap();

        // Sibling sub-agent dir named with the full inner sessionId
        let sub_dir = chats.join("b26d-full-uuid-abc");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(
            sub_dir.join("helper.json"),
            r#"{
  "sessionId":"helper-sub",
  "projectHash":"h",
  "kind":"subagent",
  "summary":"found it in auth.rs",
  "startTime":"2026-04-17T10:05:00Z",
  "lastUpdated":"2026-04-17T10:10:00Z",
  "messages":[
    {"id":"s1","timestamp":"2026-04-17T10:05:00Z","type":"user","content":[{"text":"search for auth bug"}]}
  ]
}"#,
        )
        .unwrap();

        let io = ConvoIO::with_resolver(PathResolver::new(temp.path()).with_gemini_dir(&gemini));
        (temp, io)
    }

    #[test]
    fn test_read_session_real_world_layout() {
        let (_t, io) = setup_main_with_sibling_subagent();
        let convo = io.read_session("/p", "session-2026-04-17-b26d").unwrap();
        assert_eq!(convo.main.session_id, "b26d-full-uuid-abc");
        assert_eq!(convo.main.kind.as_deref(), Some("main"));
        assert_eq!(convo.main.messages.len(), 2);
        assert_eq!(convo.sub_agents.len(), 1);
        assert_eq!(convo.sub_agents[0].session_id, "helper-sub");
        assert_eq!(
            convo.sub_agents[0].summary.as_deref(),
            Some("found it in auth.rs")
        );
        assert_eq!(convo.project_path.as_deref(), Some("/abs/p"));
    }

    #[test]
    fn test_read_session_metadata_real_world_layout() {
        let (_t, io) = setup_main_with_sibling_subagent();
        let meta = io
            .read_session_metadata("/p", "session-2026-04-17-b26d")
            .unwrap();
        // 2 main + 1 sub-agent
        assert_eq!(meta.message_count, 3);
        assert_eq!(meta.sub_agent_count, 1);
        assert!(meta.started_at.is_some());
        assert!(meta.last_activity.is_some());
    }

    #[test]
    fn test_list_session_metadata_real_world() {
        let (_t, io) = setup_main_with_sibling_subagent();
        let metas = io.list_session_metadata("/p").unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].session_uuid, "session-2026-04-17-b26d");
        assert_eq!(metas[0].sub_agent_count, 1);
    }

    #[test]
    fn test_read_session_main_without_sibling_dir() {
        // Main file exists but no sub-agent UUID dir — sub_agents stays empty.
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let chats = gemini.join("tmp/p/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        fs::write(
            chats.join("session-solo.json"),
            r#"{"sessionId":"solo-uuid","projectHash":"h","kind":"main","messages":[
  {"id":"u","timestamp":"ts","type":"user","content":"hi"}
]}"#,
        )
        .unwrap();

        let io = ConvoIO::with_resolver(PathResolver::new(temp.path()).with_gemini_dir(&gemini));
        let convo = io.read_session("/p", "session-solo").unwrap();
        assert_eq!(convo.main.session_id, "solo-uuid");
        assert!(convo.sub_agents.is_empty());
    }

    #[test]
    fn test_session_exists_main_file_case() {
        let (_t, io) = setup_main_with_sibling_subagent();
        assert!(io.session_exists("/p", "session-2026-04-17-b26d").unwrap());
        assert!(!io.session_exists("/p", "nope").unwrap());
    }

    #[test]
    fn test_list_sessions_real_world_has_no_duplicates() {
        let (_t, io) = setup_main_with_sibling_subagent();
        let sessions = io.list_sessions("/p").unwrap();
        // One main file → one session listed. Sibling UUID dir must not
        // show up as its own session.
        assert_eq!(sessions, vec!["session-2026-04-17-b26d".to_string()]);
    }

    // ── Small accessors ───────────────────────────────────────────────

    #[test]
    fn test_resolver_accessor() {
        let (_t, io) = setup();
        assert!(io.resolver().exists());
    }

    #[test]
    fn test_gemini_dir_path_accessor() {
        let (temp, io) = setup();
        let p = io.gemini_dir_path();
        assert_eq!(p, temp.path().join(".gemini"));
    }

    #[test]
    fn test_exists_accessor() {
        let (_t, io) = setup();
        assert!(io.exists());
        let missing = ConvoIO::with_resolver(PathResolver::new("/nowhere"));
        assert!(!missing.exists());
    }

    #[test]
    fn test_read_all_chats_returns_all_files() {
        let (_t, io) = setup();
        let chats = io.read_all_chats("/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(chats.len(), 2);
        let names: Vec<_> = chats.iter().map(|(s, _)| s.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"sub-s"));
    }
}
