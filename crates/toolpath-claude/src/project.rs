//! [`ClaudeProjector`] — maps a [`ConversationView`] back to a Claude
//! [`Conversation`].
//!
//! This is the inverse of [`crate::provider::to_view`]: where `to_view`
//! reads a Claude JSONL conversation into a provider-agnostic view,
//! `ClaudeProjector` serializes that view back into the Claude wire format.

use crate::types::{
    ContentPart, Conversation, ConversationEntry, Message, MessageContent, MessageRole,
    ToolResultContent, Usage,
};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Result, Role, ToolInvocation, Turn,
};

// ── ClaudeProjector ───────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Claude [`Conversation`].
///
/// Maps the provider-agnostic view back into Claude's JSONL wire format.
/// Assistant turns with tool uses will produce a separate tool-result user
/// entry after each assistant entry (one entry per assistant turn that has
/// tool uses with results).
///
/// # Example
///
/// ```rust
/// use toolpath_claude::project::ClaudeProjector;
/// use toolpath_convo::{ConversationView, ConversationProjector};
///
/// let view = ConversationView {
///     id: "my-session".to_string(),
///     started_at: None,
///     last_activity: None,
///     turns: vec![],
///     total_usage: None,
///     provider_id: None,
///     files_changed: vec![],
///     session_ids: vec![],
/// };
///
/// let projector = ClaudeProjector;
/// let convo = projector.project(&view).unwrap();
/// assert_eq!(convo.session_id, "my-session");
/// ```
pub struct ClaudeProjector;

impl ConversationProjector for ClaudeProjector {
    type Output = Conversation;

    fn project(&self, view: &ConversationView) -> Result<Conversation> {
        project_view(view).map_err(|e| ConvoError::Provider(e.to_string()))
    }
}

// ── Projection logic ─────────────────────────────────────────────────

fn project_view(view: &ConversationView) -> std::result::Result<Conversation, String> {
    let mut convo = Conversation::new(view.id.clone());

    for turn in &view.turns {
        match &turn.role {
            Role::User => {
                convo.add_entry(user_turn_to_entry(turn, &view.id));
            }
            Role::Assistant => {
                let assistant_entry = assistant_turn_to_entry(turn, &view.id);
                convo.add_entry(assistant_entry);

                // Emit a separate tool-result user entry if any tool uses have results
                if let Some(result_entry) = tool_result_entry(turn, &view.id) {
                    convo.add_entry(result_entry);
                }
            }
            Role::System => {
                convo.add_entry(system_turn_to_entry(turn, &view.id));
            }
            Role::Other(_) => {
                convo.add_entry(other_turn_to_entry(turn, &view.id));
            }
        }
    }

    Ok(convo)
}

/// Build a `ConversationEntry` for a user turn.
fn user_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    let content = MessageContent::Text(turn.text.clone());

    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: turn
            .environment
            .as_ref()
            .and_then(|e| e.working_dir.clone()),
        git_branch: turn
            .environment
            .as_ref()
            .and_then(|e| e.vcs_branch.clone()),
        message: Some(Message {
            role: MessageRole::User,
            content: Some(content),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build a `ConversationEntry` for an assistant turn.
fn assistant_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    let content = build_assistant_content(turn);

    let usage = turn.token_usage.as_ref().map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        // TokenUsage uses cache_write_tokens; Usage uses cache_creation_input_tokens
        cache_creation_input_tokens: u.cache_write_tokens,
        cache_read_input_tokens: u.cache_read_tokens,
        cache_creation: None,
        service_tier: None,
    });

    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "assistant".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::Assistant,
            content: Some(content),
            model: turn.model.clone(),
            id: None,
            message_type: None,
            stop_reason: turn.stop_reason.clone(),
            stop_sequence: None,
            usage,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build the `MessageContent` for an assistant turn.
///
/// If the turn has ONLY text (no thinking, no tool_uses): returns
/// `MessageContent::Text`. Otherwise builds `MessageContent::Parts`.
fn build_assistant_content(turn: &Turn) -> MessageContent {
    let has_thinking = turn.thinking.is_some();
    let has_tool_uses = !turn.tool_uses.is_empty();

    if !has_thinking && !has_tool_uses {
        // Simple text-only assistant response
        return MessageContent::Text(turn.text.clone());
    }

    let mut parts: Vec<ContentPart> = Vec::new();

    if let Some(thinking) = &turn.thinking {
        parts.push(ContentPart::Thinking {
            thinking: thinking.clone(),
            signature: None,
        });
    }

    if !turn.text.is_empty() {
        parts.push(ContentPart::Text {
            text: turn.text.clone(),
        });
    }

    for tu in &turn.tool_uses {
        parts.push(ContentPart::ToolUse {
            id: tu.id.clone(),
            name: tu.name.clone(),
            input: tu.input.clone(),
        });
    }

    MessageContent::Parts(parts)
}

/// Build a tool-result user entry for tool uses that have results.
///
/// Returns `None` if no tool use has a result.
fn tool_result_entry(turn: &Turn, session_id: &str) -> Option<ConversationEntry> {
    let result_parts: Vec<ContentPart> = turn
        .tool_uses
        .iter()
        .filter_map(build_tool_result_part)
        .collect();

    if result_parts.is_empty() {
        return None;
    }

    Some(ConversationEntry {
        uuid: format!("{}-result", turn.id),
        parent_uuid: Some(turn.id.clone()),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts(result_parts)),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    })
}

/// Build a `ContentPart::ToolResult` from a `ToolInvocation` if it has a result.
fn build_tool_result_part(tu: &ToolInvocation) -> Option<ContentPart> {
    tu.result.as_ref().map(|r| ContentPart::ToolResult {
        tool_use_id: tu.id.clone(),
        content: ToolResultContent::Text(r.content.clone()),
        is_error: r.is_error,
    })
}

/// Build a user entry for a System turn.
fn system_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build a user entry for an Other-role turn.
fn other_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{EnvironmentSnapshot, TokenUsage, ToolResult};

    fn make_view(id: &str, turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: id.to_string(),
            started_at: None,
            last_activity: None,
            turns,
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
        }
    }

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            role: Role::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            extra: Default::default(),
        }
    }

    fn assistant_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            role: Role::Assistant,
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            extra: Default::default(),
        }
    }

    // ── Test 1: Basic conversation (user + assistant, no tools) ───────

    #[test]
    fn test_basic_conversation_entry_count_and_content() {
        let view = make_view(
            "sess-1",
            vec![user_turn("u1", "Hello"), assistant_turn("a1", "Hi there!")],
        );
        let projector = ClaudeProjector;
        let convo = projector.project(&view).unwrap();

        assert_eq!(convo.session_id, "sess-1");
        assert_eq!(convo.entries.len(), 2);

        let user_entry = &convo.entries[0];
        assert_eq!(user_entry.entry_type, "user");
        assert_eq!(user_entry.uuid, "u1");
        let msg = user_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.text(), "Hello");

        let asst_entry = &convo.entries[1];
        assert_eq!(asst_entry.entry_type, "assistant");
        assert_eq!(asst_entry.uuid, "a1");
        let msg = asst_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.text(), "Hi there!");
        // Simple text: should be MessageContent::Text, not Parts
        assert!(matches!(msg.content, Some(MessageContent::Text(_))));
    }

    // ── Test 2: User turn with environment → cwd and git_branch ──────

    #[test]
    fn test_user_turn_with_environment() {
        let mut turn = user_turn("u1", "Hello");
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/my/project".to_string()),
            vcs_branch: Some("feat/auth".to_string()),
            vcs_revision: None,
        });

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &convo.entries[0];
        assert_eq!(entry.cwd.as_deref(), Some("/my/project"));
        assert_eq!(entry.git_branch.as_deref(), Some("feat/auth"));
    }

    // ── Test 3: Assistant with thinking + text + tool_use → Parts ────

    #[test]
    fn test_assistant_thinking_text_tool_use_produces_parts() {
        let mut turn = assistant_turn("a1", "I'll read the file.");
        turn.thinking = Some("Hmm, need to read the file first.".to_string());
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "src/main.rs"}),
            result: None,
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // One assistant entry (no results → no tool-result entry)
        assert_eq!(convo.entries.len(), 1);
        let entry = &convo.entries[0];
        let msg = entry.message.as_ref().unwrap();

        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 3);
                // Order: Thinking, Text, ToolUse
                assert!(matches!(parts[0], ContentPart::Thinking { .. }));
                assert!(matches!(parts[1], ContentPart::Text { .. }));
                assert!(matches!(parts[2], ContentPart::ToolUse { .. }));

                if let ContentPart::Thinking { thinking, .. } = &parts[0] {
                    assert_eq!(thinking, "Hmm, need to read the file first.");
                }
                if let ContentPart::Text { text } = &parts[1] {
                    assert_eq!(text, "I'll read the file.");
                }
                if let ContentPart::ToolUse { id, name, .. } = &parts[2] {
                    assert_eq!(id, "t1");
                    assert_eq!(name, "Read");
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Test 4: Simple text-only assistant → MessageContent::Text ────

    #[test]
    fn test_simple_text_only_assistant_produces_text_not_parts() {
        let turn = assistant_turn("a1", "Just a plain answer.");

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &convo.entries[0];
        let msg = entry.message.as_ref().unwrap();
        assert!(
            matches!(&msg.content, Some(MessageContent::Text(t)) if t == "Just a plain answer.")
        );
    }

    // ── Test 5: Tool results emitted as separate user entries ─────────

    #[test]
    fn test_tool_results_emitted_as_separate_user_entries() {
        let mut turn = assistant_turn("a1", "Reading file.");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "src/main.rs"}),
            result: Some(ToolResult {
                content: "fn main() {}".to_string(),
                is_error: false,
            }),
            category: None,
        }];

        let view = make_view("sess-1", vec![user_turn("u1", "Go"), turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // user + assistant + tool-result user
        assert_eq!(convo.entries.len(), 3);

        let result_entry = &convo.entries[2];
        assert_eq!(result_entry.entry_type, "user");
        assert_eq!(result_entry.uuid, "a1-result");
        assert_eq!(result_entry.parent_uuid.as_deref(), Some("a1"));

        let msg = result_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::User);

        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t1");
                        assert_eq!(content.text(), "fn main() {}");
                        assert!(!is_error);
                    }
                    other => panic!("Expected ToolResult, got {:?}", other),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Test 6: No tool result entry when tool uses have no results ───

    #[test]
    fn test_no_tool_result_entry_when_no_results() {
        let mut turn = assistant_turn("a1", "Reading...");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
            result: None, // no result
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // Only the assistant entry, no tool-result entry
        assert_eq!(convo.entries.len(), 1);
        assert_eq!(convo.entries[0].entry_type, "assistant");
    }

    // ── Test 7: Token usage mapped correctly (cache field name swap) ──

    #[test]
    fn test_token_usage_mapped_correctly_with_cache_swap() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: Some(500),  // → cache_read_input_tokens
            cache_write_tokens: Some(200), // → cache_creation_input_tokens
        });

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = convo.entries[0].message.as_ref().unwrap();
        let usage = msg.usage.as_ref().unwrap();

        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.cache_read_input_tokens, Some(500));
        assert_eq!(usage.cache_creation_input_tokens, Some(200));
    }

    // ── Test 8: Session ID and parent chain preserved ─────────────────

    #[test]
    fn test_session_id_and_parent_chain_preserved() {
        let mut t1 = user_turn("u1", "First");
        let mut t2 = assistant_turn("a1", "Reply");
        t2.parent_id = Some("u1".to_string());
        let mut t3 = user_turn("u2", "Second");
        t3.parent_id = Some("a1".to_string());

        let view = make_view("my-session", vec![t1, t2, t3]);
        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(convo.session_id, "my-session");
        for entry in &convo.entries {
            assert_eq!(entry.session_id.as_deref(), Some("my-session"));
        }

        assert_eq!(convo.entries[0].parent_uuid, None);
        assert_eq!(convo.entries[1].parent_uuid.as_deref(), Some("u1"));
        assert_eq!(convo.entries[2].parent_uuid.as_deref(), Some("a1"));
    }

    // ── Test 9: Stop reason and model preserved ───────────────────────

    #[test]
    fn test_stop_reason_and_model_preserved() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.model = Some("claude-opus-4-6".to_string());
        turn.stop_reason = Some("end_turn".to_string());

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = convo.entries[0].message.as_ref().unwrap();
        assert_eq!(msg.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(msg.stop_reason.as_deref(), Some("end_turn"));
    }

    // ── Additional edge case: is_sidechain always false ───────────────

    #[test]
    fn test_is_sidechain_always_false() {
        let view = make_view(
            "sess-1",
            vec![user_turn("u1", "Hi"), assistant_turn("a1", "Hello")],
        );
        let convo = ClaudeProjector.project(&view).unwrap();

        for entry in &convo.entries {
            assert!(!entry.is_sidechain);
        }
    }

    // ── Additional edge case: empty text assistant with tool use ──────

    #[test]
    fn test_assistant_no_text_only_tool_use_produces_parts() {
        let mut turn = assistant_turn("a1", "");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            result: None,
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = convo.entries[0].message.as_ref().unwrap();
        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                // Empty text not included, just the ToolUse
                assert_eq!(parts.len(), 1);
                assert!(matches!(parts[0], ContentPart::ToolUse { .. }));
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Additional: multiple tool uses, all with results ─────────────

    #[test]
    fn test_multiple_tool_uses_all_with_results() {
        let mut turn = assistant_turn("a1", "Reading two files.");
        turn.tool_uses = vec![
            ToolInvocation {
                id: "t1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file a".to_string(),
                    is_error: false,
                }),
                category: None,
            },
            ToolInvocation {
                id: "t2".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file b".to_string(),
                    is_error: true,
                }),
                category: None,
            },
        ];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // assistant + tool-result entry
        assert_eq!(convo.entries.len(), 2);

        let result_entry = &convo.entries[1];
        let msg = result_entry.message.as_ref().unwrap();
        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t1");
                        assert_eq!(content.text(), "file a");
                        assert!(!is_error);
                    }
                    _ => panic!("Expected ToolResult at index 0"),
                }
                match &parts[1] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t2");
                        assert_eq!(content.text(), "file b");
                        assert!(is_error);
                    }
                    _ => panic!("Expected ToolResult at index 1"),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Additional: mixed results (some with, some without) ──────────

    #[test]
    fn test_partial_tool_results_only_emits_those_with_results() {
        let mut turn = assistant_turn("a1", "Using tools.");
        turn.tool_uses = vec![
            ToolInvocation {
                id: "t1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file content".to_string(),
                    is_error: false,
                }),
                category: None,
            },
            ToolInvocation {
                id: "t2".to_string(),
                name: "Write".to_string(),
                input: serde_json::json!({}),
                result: None, // no result for this one
                category: None,
            },
        ];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // assistant + tool-result entry (only t1 has a result)
        assert_eq!(convo.entries.len(), 2);
        let result_entry = &convo.entries[1];
        let msg = result_entry.message.as_ref().unwrap();
        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                // Only one result (t1), not two
                assert_eq!(parts.len(), 1);
                if let ContentPart::ToolResult { tool_use_id, .. } = &parts[0] {
                    assert_eq!(tool_use_id, "t1");
                } else {
                    panic!("Expected ToolResult");
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }
}
