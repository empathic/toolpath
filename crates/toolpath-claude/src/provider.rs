//! Implementation of `toolpath-convo` traits for Claude conversations.
//!
//! Handles cross-entry tool result assembly: Claude's JSONL format writes
//! tool invocations and their results as separate entries. This module
//! pairs them by `tool_use_id` so consumers get complete `Turn` values
//! with `ToolInvocation.result` populated.

use crate::ClaudeConvo;
use crate::types::{Conversation, ConversationEntry, Message, MessageContent, MessageRole};
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

/// Convert a single entry to a Turn without cross-entry assembly.
/// Tool results within the same message are still matched.
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
        crate::types::ContentPart::ToolResult {
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

/// Returns true if this entry is a tool-result-only user message
/// (no human-authored text, only tool_result parts).
fn is_tool_result_only(entry: &ConversationEntry) -> bool {
    let Some(msg) = &entry.message else {
        return false;
    };
    msg.role == MessageRole::User && msg.text().is_empty() && !msg.tool_results().is_empty()
}

/// Merge tool results from a tool-result-only message into existing turns.
///
/// Matches by `tool_use_id` — scans backwards through turns to find the
/// `ToolInvocation` with a matching `id` for each result. This handles
/// cases where a single result entry carries results for tool uses from
/// different assistant turns.
///
/// Returns true if any results were merged.
fn merge_tool_results(turns: &mut [Turn], msg: &Message) -> bool {
    let mut merged = false;
    for tr in msg.tool_results() {
        for turn in turns.iter_mut().rev() {
            if let Some(invocation) = turn
                .tool_uses
                .iter_mut()
                .find(|tu| tu.id == tr.tool_use_id && tu.result.is_none())
            {
                invocation.result = Some(ToolResult {
                    content: tr.content.text(),
                    is_error: tr.is_error,
                });
                merged = true;
                break;
            }
        }
    }
    merged
}

fn entry_to_turn(entry: &ConversationEntry) -> Option<Turn> {
    entry
        .message
        .as_ref()
        .map(|msg| message_to_turn(entry, msg))
}

/// Convert a full conversation to a view with cross-entry tool result assembly.
///
/// Tool-result-only user entries are absorbed into the preceding assistant
/// turn's `ToolInvocation.result` fields rather than emitted as separate turns.
fn conversation_to_view(convo: &Conversation) -> ConversationView {
    let mut turns: Vec<Turn> = Vec::new();

    for entry in &convo.entries {
        let Some(msg) = &entry.message else {
            continue;
        };

        // Tool-result-only user entries get merged into existing turns
        if is_tool_result_only(entry) {
            merge_tool_results(&mut turns, msg);
            continue;
        }

        turns.push(message_to_turn(entry, msg));
    }

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

// ── ConversationWatcher with eager emit + TurnUpdated ────────────────

#[cfg(feature = "watcher")]
impl toolpath_convo::ConversationWatcher for crate::watcher::ConversationWatcher {
    fn poll(&mut self) -> toolpath_convo::Result<Vec<WatcherEvent>> {
        let entries = crate::watcher::ConversationWatcher::poll(self)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;

        let mut events: Vec<WatcherEvent> = Vec::new();

        for entry in &entries {
            let Some(msg) = &entry.message else {
                events.push(entry_to_watcher_event(entry));
                continue;
            };

            if is_tool_result_only(entry) {
                // Find matching turns in previously emitted events and in
                // our assembled state, merge results, emit TurnUpdated.
                // Walk events in reverse to find the turn to update.
                let mut updated_turn: Option<Turn> = None;

                // Search backwards through events emitted this poll cycle
                for event in events.iter_mut().rev() {
                    if let WatcherEvent::Turn(turn) | WatcherEvent::TurnUpdated(turn) = event
                        && turn.tool_uses.iter().any(|tu| {
                            tu.result.is_none()
                                && msg.tool_results().iter().any(|tr| tr.tool_use_id == tu.id)
                        })
                    {
                        // Merge results into this turn
                        let mut updated = (**turn).clone();
                        merge_tool_results(std::slice::from_mut(&mut updated), msg);
                        updated_turn = Some(updated.clone());
                        // Also update the existing event in-place so later
                        // result entries can find the right state
                        **turn = updated;
                        break;
                    }
                }

                if let Some(turn) = updated_turn {
                    events.push(WatcherEvent::TurnUpdated(Box::new(turn)));
                }
                // If no matching turn found, the tool-result-only entry
                // is silently dropped (the matching turn was emitted in a
                // prior poll cycle and can't be updated from here).
                continue;
            }

            events.push(entry_to_watcher_event(entry));
        }

        Ok(events)
    }

    fn seen_count(&self) -> usize {
        crate::watcher::ConversationWatcher::seen_count(self)
    }
}

// ── Public re-exports for convenience ────────────────────────────────

/// Convert a Claude [`Conversation`] directly into a [`ConversationView`].
///
/// This performs cross-entry tool result assembly: tool-result-only user
/// entries are merged into the preceding assistant turn rather than emitted
/// as separate turns.
pub fn to_view(convo: &Conversation) -> ConversationView {
    conversation_to_view(convo)
}

/// Convert a single Claude [`ConversationEntry`] into a [`Turn`], if it
/// contains a message.
///
/// Note: this does *not* perform cross-entry assembly. For assembled
/// results, use [`to_view`] instead.
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
            r#"{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll fix that."},{"type":"thinking","thinking":"The bug is in auth"},{"type":"tool_use","id":"t1","name":"Read","input":{"file":"src/main.rs"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            r#"{"uuid":"uuid-3","type":"user","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main() { println!(\"hello\"); }","is_error":false}]}}"#,
            r#"{"uuid":"uuid-4","type":"assistant","parentUuid":"uuid-3","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"I see the issue. Let me fix it."},{"type":"tool_use","id":"t2","name":"Edit","input":{"file":"src/main.rs","content":"fixed"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":100}}}"#,
            r#"{"uuid":"uuid-5","type":"user","parentUuid":"uuid-4","timestamp":"2024-01-01T00:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"File written successfully","is_error":false}]}}"#,
            r#"{"uuid":"uuid-6","type":"assistant","parentUuid":"uuid-5","timestamp":"2024-01-01T00:00:05Z","message":{"role":"assistant","content":"Done! The bug is fixed.","model":"claude-opus-4-6","stop_reason":"end_turn"}}"#,
            r#"{"uuid":"uuid-7","type":"user","parentUuid":"uuid-6","timestamp":"2024-01-01T00:00:06Z","message":{"role":"user","content":"Thanks!"}}"#,
        ];
        fs::write(project_dir.join("session-1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn test_load_conversation_assembles_tool_results() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        assert_eq!(view.id, "session-1");
        // 7 entries collapse to 5 turns (2 tool-result-only entries absorbed)
        assert_eq!(view.turns.len(), 5);

        // Turn 0: user "Fix the bug"
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert!(view.turns[0].parent_id.is_none());

        // Turn 1: assistant with tool use + assembled result
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(
            view.turns[1].thinking.as_deref(),
            Some("The bug is in auth")
        );
        assert_eq!(view.turns[1].tool_uses.len(), 1);
        assert_eq!(view.turns[1].tool_uses[0].name, "Read");
        assert_eq!(view.turns[1].tool_uses[0].id, "t1");
        // Key assertion: result is populated from the next entry
        let result = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("fn main()"));
        assert_eq!(view.turns[1].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(view.turns[1].stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("uuid-1"));

        // Token usage
        let usage = view.turns[1].token_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));

        // Turn 2: second assistant with tool use + assembled result
        assert_eq!(view.turns[2].role, Role::Assistant);
        assert_eq!(view.turns[2].text, "I see the issue. Let me fix it.");
        assert_eq!(view.turns[2].tool_uses[0].name, "Edit");
        let result2 = view.turns[2].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(result2.content, "File written successfully");

        // Turn 3: final assistant (no tools)
        assert_eq!(view.turns[3].role, Role::Assistant);
        assert_eq!(view.turns[3].text, "Done! The bug is fixed.");
        assert!(view.turns[3].tool_uses.is_empty());

        // Turn 4: user "Thanks!"
        assert_eq!(view.turns[4].role, Role::User);
        assert_eq!(view.turns[4].text, "Thanks!");
    }

    #[test]
    fn test_no_phantom_empty_turns() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        // No turns should have empty text with User role (phantom turns)
        for turn in &view.turns {
            if turn.role == Role::User {
                assert!(
                    !turn.text.is_empty(),
                    "Found phantom empty user turn: {:?}",
                    turn.id
                );
            }
        }
    }

    #[test]
    fn test_tool_result_error_flag() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read a file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"/nonexistent"}}],"stop_reason":"tool_use"}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"File not found","is_error":true}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2); // user + assistant (tool-result absorbed)
        let result = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "File not found");
    }

    #[test]
    fn test_multiple_tool_uses_single_result_entry() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Check two files"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading both..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a.rs"}},{"type":"tool_use","id":"t2","name":"Read","input":{"path":"b.rs"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file a contents","is_error":false},{"type":"tool_result","tool_use_id":"t2","content":"file b contents","is_error":false}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[1].tool_uses.len(), 2);

        let r1 = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(r1.content, "file a contents");

        let r2 = view.turns[1].tool_uses[1].result.as_ref().unwrap();
        assert_eq!(r2.content, "file b contents");
    }

    #[test]
    fn test_conversation_without_tool_use_unchanged() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there!"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].text, "Hello");
        assert_eq!(view.turns[1].text, "Hi there!");
    }

    #[test]
    fn test_assistant_turn_without_result_has_none() {
        // Tool use at end of conversation with no result entry
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read a file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert!(view.turns[1].tool_uses[0].result.is_none());
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
        assert_eq!(meta.message_count, 7);
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
        assert_eq!(view.turns.len(), 5);
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
    fn test_watcher_trait_basic() {
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

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait_assembles_tool_results() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read the file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main() {}","is_error":false}]}}"#,
            r#"{"uuid":"u4","type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":"Done!"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "s1".to_string(),
        );

        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();

        // Should get: Turn(user), Turn(assistant), TurnUpdated(assistant), Turn(assistant)
        assert_eq!(events.len(), 4);

        // First: user turn
        assert!(matches!(&events[0], WatcherEvent::Turn(t) if t.role == Role::User));

        // Second: assistant turn emitted eagerly (result may not be populated yet in the event)
        assert!(matches!(&events[1], WatcherEvent::Turn(t) if t.role == Role::Assistant));

        // Third: TurnUpdated with results merged
        match &events[2] {
            WatcherEvent::TurnUpdated(turn) => {
                assert_eq!(turn.id, "u2");
                assert_eq!(turn.tool_uses.len(), 1);
                let result = turn.tool_uses[0].result.as_ref().unwrap();
                assert_eq!(result.content, "fn main() {}");
                assert!(!result.is_error);
            }
            other => panic!("Expected TurnUpdated, got {:?}", other),
        }

        // Fourth: final assistant turn
        assert!(matches!(&events[3], WatcherEvent::Turn(t) if t.text == "Done!"));
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait_incremental_tool_results() {
        // Simulate tool results arriving in a different poll cycle than the tool use
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Start with just the user message and assistant tool use
        let entries_phase1 = vec![
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
        ];
        fs::write(
            project_dir.join("s1.jsonl"),
            entries_phase1.join("\n") + "\n",
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "s1".to_string(),
        );

        // First poll: get user + assistant turns
        let events1 = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert_eq!(events1.len(), 2);
        // Assistant turn emitted eagerly with result: None
        if let WatcherEvent::Turn(t) = &events1[1] {
            assert!(t.tool_uses[0].result.is_none());
        } else {
            panic!("Expected Turn");
        }

        // Now append the tool result entry
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(project_dir.join("s1.jsonl"))
            .unwrap();
        writeln!(file, r#"{{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"fn main() {{}}","is_error":false}}]}}}}"#).unwrap();

        // Second poll: tool-result-only entry arrives
        let events2 = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        // The tool-result-only entry can't find its matching turn in this poll
        // cycle (it was emitted in the previous one), so it's silently absorbed.
        // This is a known limitation of the eager-emit approach for cross-poll
        // boundaries — the batch path (to_view) handles this correctly.
        // Consumers needing full fidelity across poll boundaries should
        // periodically do a full load_conversation.
        assert!(events2.is_empty() || events2.iter().all(|e| !matches!(e, WatcherEvent::Turn(_))));
    }

    #[test]
    fn test_merge_tool_results_by_id() {
        // Verify that merge matches by tool_use_id, not position
        let mut turns = vec![Turn {
            id: "t1".into(),
            parent_id: None,
            role: Role::Assistant,
            timestamp: "2024-01-01T00:00:00Z".into(),
            text: "test".into(),
            thinking: None,
            tool_uses: vec![
                ToolInvocation {
                    id: "tool-a".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                    result: None,
                },
                ToolInvocation {
                    id: "tool-b".into(),
                    name: "Write".into(),
                    input: serde_json::json!({}),
                    result: None,
                },
            ],
            model: None,
            stop_reason: None,
            token_usage: None,
            extra: Default::default(),
        }];

        // Create a message with results in reversed order
        let msg: Message = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-b","content":"write result","is_error":false},{"type":"tool_result","tool_use_id":"tool-a","content":"read result","is_error":true}]}"#,
        )
        .unwrap();

        let merged = merge_tool_results(&mut turns, &msg);
        assert!(merged);

        // Results should match by ID regardless of order
        assert_eq!(
            turns[0].tool_uses[0].result.as_ref().unwrap().content,
            "read result"
        );
        assert!(turns[0].tool_uses[0].result.as_ref().unwrap().is_error);

        assert_eq!(
            turns[0].tool_uses[1].result.as_ref().unwrap().content,
            "write result"
        );
        assert!(!turns[0].tool_uses[1].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_is_tool_result_only() {
        // Tool-result-only entry
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}]}}"#,
        )
        .unwrap();
        assert!(is_tool_result_only(&entry));

        // Regular user entry with text
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u2","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));

        // Entry without message
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u3","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));

        // Assistant entry (never tool-result-only)
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u4","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"hi"}}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));
    }
}
