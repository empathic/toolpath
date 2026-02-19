//! Implementation of `toolpath-convo` traits for Claude conversations.

use crate::ClaudeConvo;
use crate::types::{
    ContentPart, Conversation, ConversationEntry, Message, MessageContent, MessageRole,
};
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError, Role, TokenUsage,
    ToolInvocation, ToolResult, Turn, WatcherEvent,
};

// ── Conversion helpers ───────────────────────────────────────────────

fn claude_role_to_role(role: &MessageRole) -> Role {
    match role {
        MessageRole::User => Role::User,
        MessageRole::Assistant => Role::Assistant,
        MessageRole::System => Role::System,
    }
}

fn message_to_turn(entry: &ConversationEntry, msg: &Message) -> Turn {
    let text = msg.text();

    let thinking = msg.thinking().map(|parts| parts.join("\n"));

    let tool_uses = msg
        .tool_uses()
        .into_iter()
        .map(|tu| {
            let result = find_tool_result_in_parts(msg, tu.id);
            ToolInvocation {
                id: tu.id.to_string(),
                name: tu.name.to_string(),
                input: tu.input.clone(),
                result,
            }
        })
        .collect();

    let token_usage = msg.usage.as_ref().map(|u| TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
    });

    Turn {
        id: entry.uuid.clone(),
        parent_id: entry.parent_uuid.clone(),
        role: claude_role_to_role(&msg.role),
        timestamp: entry.timestamp.clone(),
        text,
        thinking,
        tool_uses,
        model: msg.model.clone(),
        stop_reason: msg.stop_reason.clone(),
        token_usage,
        extra: Default::default(),
    }
}

fn find_tool_result_in_parts(msg: &Message, tool_use_id: &str) -> Option<ToolResult> {
    let parts = match &msg.content {
        Some(MessageContent::Parts(parts)) => parts,
        _ => return None,
    };
    parts.iter().find_map(|p| match p {
        ContentPart::ToolResult {
            tool_use_id: id,
            content,
            is_error,
        } if id == tool_use_id => Some(ToolResult {
            content: content.text(),
            is_error: *is_error,
        }),
        _ => None,
    })
}

fn entry_to_turn(entry: &ConversationEntry) -> Option<Turn> {
    entry
        .message
        .as_ref()
        .map(|msg| message_to_turn(entry, msg))
}

fn conversation_to_view(convo: &Conversation) -> ConversationView {
    let turns = convo.entries.iter().filter_map(entry_to_turn).collect();

    ConversationView {
        id: convo.session_id.clone(),
        started_at: convo.started_at,
        last_activity: convo.last_activity,
        turns,
    }
}

fn entry_to_watcher_event(entry: &ConversationEntry) -> WatcherEvent {
    match entry_to_turn(entry) {
        Some(turn) => WatcherEvent::Turn(Box::new(turn)),
        None => WatcherEvent::Progress {
            kind: entry.entry_type.clone(),
            data: serde_json::json!({
                "uuid": entry.uuid,
                "timestamp": entry.timestamp,
            }),
        },
    }
}

// ── ConversationProvider for ClaudeConvo ──────────────────────────────

impl ConversationProvider for ClaudeConvo {
    fn list_conversations(&self, project: &str) -> toolpath_convo::Result<Vec<String>> {
        crate::ClaudeConvo::list_conversations(self, project)
            .map_err(|e| ConvoError::Provider(e.to_string()))
    }

    fn load_conversation(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let convo = self
            .read_conversation(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(conversation_to_view(&convo))
    }

    fn load_metadata(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let meta = self
            .read_conversation_metadata(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: meta.session_id,
            started_at: meta.started_at,
            last_activity: meta.last_activity,
            message_count: meta.message_count,
            file_path: Some(meta.file_path),
        })
    }

    fn list_metadata(&self, project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_conversation_metadata(project)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.session_id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: Some(m.file_path),
            })
            .collect())
    }
}

// ── ConversationWatcher for ConversationWatcher ──────────────────────

#[cfg(feature = "watcher")]
impl toolpath_convo::ConversationWatcher for crate::watcher::ConversationWatcher {
    fn poll(&mut self) -> toolpath_convo::Result<Vec<WatcherEvent>> {
        let entries = crate::watcher::ConversationWatcher::poll(self)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(entries.iter().map(entry_to_watcher_event).collect())
    }

    fn seen_count(&self) -> usize {
        crate::watcher::ConversationWatcher::seen_count(self)
    }
}

// ── Public re-exports for convenience ────────────────────────────────

/// Convert a Claude [`Conversation`] directly into a [`ConversationView`].
///
/// This is useful when you already have a loaded `Conversation` and want
/// to convert it without going through the trait.
pub fn to_view(convo: &Conversation) -> ConversationView {
    conversation_to_view(convo)
}

/// Convert a single Claude [`ConversationEntry`] into a [`Turn`], if it
/// contains a message.
pub fn to_turn(entry: &ConversationEntry) -> Option<Turn> {
    entry_to_turn(entry)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathResolver;
    use std::fs;
    use tempfile::TempDir;

    fn setup_provider() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Fix the bug"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll fix that."},{"type":"thinking","thinking":"The bug is in auth"},{"type":"tool_use","id":"t1","name":"Read","input":{"file":"src/main.rs"}}],"model":"claude-opus-4-6","stopReason":"end_turn","usage":{"inputTokens":100,"outputTokens":50}}}"#,
            r#"{"uuid":"uuid-3","type":"user","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":"Thanks!"}}"#,
        ];
        fs::write(project_dir.join("session-1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn test_load_conversation() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        assert_eq!(view.id, "session-1");
        assert_eq!(view.turns.len(), 3);

        // First turn: user
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert!(view.turns[0].parent_id.is_none());

        // Second turn: assistant with thinking + tool use
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(
            view.turns[1].thinking.as_deref(),
            Some("The bug is in auth")
        );
        assert_eq!(view.turns[1].tool_uses.len(), 1);
        assert_eq!(view.turns[1].tool_uses[0].name, "Read");
        assert_eq!(view.turns[1].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(view.turns[1].stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("uuid-1"));

        // Token usage
        let usage = view.turns[1].token_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));

        // Third turn: user
        assert_eq!(view.turns[2].role, Role::User);
        assert_eq!(view.turns[2].text, "Thanks!");
    }

    #[test]
    fn test_list_conversations() {
        let (_temp, provider) = setup_provider();
        let ids = ConversationProvider::list_conversations(&provider, "/test/project").unwrap();
        assert_eq!(ids, vec!["session-1"]);
    }

    #[test]
    fn test_load_metadata() {
        let (_temp, provider) = setup_provider();
        let meta =
            ConversationProvider::load_metadata(&provider, "/test/project", "session-1").unwrap();
        assert_eq!(meta.id, "session-1");
        assert_eq!(meta.message_count, 3);
        assert!(meta.file_path.is_some());
    }

    #[test]
    fn test_list_metadata() {
        let (_temp, provider) = setup_provider();
        let metas = ConversationProvider::list_metadata(&provider, "/test/project").unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "session-1");
    }

    #[test]
    fn test_to_view() {
        let (_temp, manager) = setup_provider();
        let convo = manager
            .read_conversation("/test/project", "session-1")
            .unwrap();
        let view = to_view(&convo);
        assert_eq!(view.turns.len(), 3);
        assert_eq!(view.title(20).unwrap(), "Fix the bug");
    }

    #[test]
    fn test_to_turn_with_message() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        let turn = to_turn(&entry).unwrap();
        assert_eq!(turn.id, "u1");
        assert_eq!(turn.text, "hello");
        assert_eq!(turn.role, Role::User);
    }

    #[test]
    fn test_to_turn_without_message() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(to_turn(&entry).is_none());
    }

    #[test]
    fn test_entry_to_watcher_event_turn() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hi"}}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        assert!(matches!(event, WatcherEvent::Turn(_)));
    }

    #[test]
    fn test_entry_to_watcher_event_progress() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        assert!(matches!(event, WatcherEvent::Progress { .. }));
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#,
        ];
        fs::write(project_dir.join("session-1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // Use the trait explicitly (inherent poll returns ConversationEntry)
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], WatcherEvent::Turn(t) if t.role == Role::User));
        assert!(matches!(&events[1], WatcherEvent::Turn(t) if t.role == Role::Assistant));
        assert_eq!(toolpath_convo::ConversationWatcher::seen_count(&watcher), 2);

        // Second poll returns nothing
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert!(events.is_empty());
    }
}
