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
use serde_json::json;
use std::collections::HashMap;
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

    // Emit permission-mode preamble
    let perm_entry = ConversationEntry {
        uuid: String::new(),
        entry_type: "permission-mode".into(),
        timestamp: String::new(),
        session_id: Some(view.id.clone()),
        parent_uuid: None,
        is_sidechain: false,
        message: None,
        cwd: None,
        git_branch: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: {
            let mut m = HashMap::new();
            m.insert("permissionMode".to_string(), json!("default"));
            m
        },
    };
    convo.add_entry(perm_entry);

    for turn in &view.turns {
        match &turn.role {
            Role::User => {
                let mut entry = user_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                convo.add_entry(entry);
            }
            Role::Assistant => {
                let mut assistant_entry = assistant_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut assistant_entry, turn);
                convo.add_entry(assistant_entry);

                // Emit a separate tool-result user entry if any tool uses have results
                if let Some(mut result_entry) = tool_result_entry(turn, &view.id) {
                    apply_turn_metadata(&mut result_entry, turn);
                    convo.add_entry(result_entry);
                }
            }
            Role::System => {
                let mut entry = system_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                convo.add_entry(entry);
            }
            Role::Other(_) => {
                let mut entry = other_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                convo.add_entry(entry);
            }
        }
    }

    Ok(convo)
}

/// Apply Claude-specific metadata from a [`Turn`] onto a [`ConversationEntry`].
///
/// Populates `cwd` and `git_branch` from [`Turn::environment`], and
/// `version`, `user_type`, `request_id` from `Turn::extra["claude"]`.
/// Remaining keys from the `"claude"` extras are merged into the entry's
/// own `extra` map so they serialize as top-level fields (via `#[serde(flatten)]`).
fn apply_turn_metadata(entry: &mut ConversationEntry, turn: &Turn) {
    // From Turn.environment
    if let Some(env) = &turn.environment {
        if entry.cwd.is_none() {
            entry.cwd = env.working_dir.clone();
        }
        if entry.git_branch.is_none() {
            entry.git_branch = env.vcs_branch.clone();
        }
    }

    // From Turn.extra["claude"]
    if let Some(claude) = turn.extra.get("claude").and_then(|v| v.as_object()) {
        if let Some(v) = claude.get("version").and_then(|v| v.as_str()) {
            entry.version = entry.version.take().or_else(|| Some(v.to_string()));
        }
        if let Some(v) = claude.get("user_type").and_then(|v| v.as_str()) {
            entry.user_type = entry.user_type.take().or_else(|| Some(v.to_string()));
        }
        if let Some(v) = claude.get("request_id").and_then(|v| v.as_str()) {
            entry.request_id = entry.request_id.take().or_else(|| Some(v.to_string()));
        }
        // Merge remaining fields into entry.extra
        for (k, v) in claude {
            match k.as_str() {
                "version" | "user_type" | "request_id" => {} // Already handled above
                _ => {
                    entry.extra.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
    }
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

    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
    extra.insert(
        "sourceToolAssistantUUID".to_string(),
        json!(turn.id),
    );

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
        extra,
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

    /// Helper: skip the permission-mode preamble and return remaining entries.
    fn content_entries(convo: &Conversation) -> &[ConversationEntry] {
        assert!(
            !convo.entries.is_empty(),
            "expected at least the permission-mode entry"
        );
        assert_eq!(convo.entries[0].entry_type, "permission-mode");
        &convo.entries[1..]
    }

    // ── Permission-mode preamble ─────────────────────────────────────

    #[test]
    fn test_permission_mode_entry_is_first() {
        let view = make_view("sess-1", vec![user_turn("u1", "Hello")]);
        let convo = ClaudeProjector.project(&view).unwrap();

        assert!(convo.entries.len() >= 2); // perm + user
        let perm = &convo.entries[0];
        assert_eq!(perm.entry_type, "permission-mode");
        assert_eq!(perm.uuid, "");
        assert_eq!(perm.timestamp, "");
        assert_eq!(perm.session_id.as_deref(), Some("sess-1"));
        assert!(perm.message.is_none());
        assert_eq!(perm.extra.get("permissionMode"), Some(&json!("default")));
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
        let entries = content_entries(&convo);
        assert_eq!(entries.len(), 2);

        let user_entry = &entries[0];
        assert_eq!(user_entry.entry_type, "user");
        assert_eq!(user_entry.uuid, "u1");
        let msg = user_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.text(), "Hello");

        let asst_entry = &entries[1];
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

        let entry = &content_entries(&convo)[0];
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

        let entries = content_entries(&convo);
        // One assistant entry (no results → no tool-result entry)
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
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

        let entry = &content_entries(&convo)[0];
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

        let entries = content_entries(&convo);
        // user + assistant + tool-result user
        assert_eq!(entries.len(), 3);

        let result_entry = &entries[2];
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

        let entries = content_entries(&convo);
        // Only the assistant entry, no tool-result entry
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "assistant");
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

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
        let usage = msg.usage.as_ref().unwrap();

        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.cache_read_input_tokens, Some(500));
        assert_eq!(usage.cache_creation_input_tokens, Some(200));
    }

    // ── Test 8: Session ID and parent chain preserved ─────────────────

    #[test]
    fn test_session_id_and_parent_chain_preserved() {
        let mut t2 = assistant_turn("a1", "Reply");
        t2.parent_id = Some("u1".to_string());
        let mut t3 = user_turn("u2", "Second");
        t3.parent_id = Some("a1".to_string());

        let view = make_view(
            "my-session",
            vec![user_turn("u1", "First"), t2, t3],
        );
        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(convo.session_id, "my-session");
        for entry in &convo.entries {
            assert_eq!(entry.session_id.as_deref(), Some("my-session"));
        }

        let entries = content_entries(&convo);
        assert_eq!(entries[0].parent_uuid, None);
        assert_eq!(entries[1].parent_uuid.as_deref(), Some("u1"));
        assert_eq!(entries[2].parent_uuid.as_deref(), Some("a1"));
    }

    // ── Test 9: Stop reason and model preserved ───────────────────────

    #[test]
    fn test_stop_reason_and_model_preserved() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.model = Some("claude-opus-4-6".to_string());
        turn.stop_reason = Some("end_turn".to_string());

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
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

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
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

        let entries = content_entries(&convo);
        // assistant + tool-result entry
        assert_eq!(entries.len(), 2);

        let result_entry = &entries[1];
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

        let entries = content_entries(&convo);
        // assistant + tool-result entry (only t1 has a result)
        assert_eq!(entries.len(), 2);
        let result_entry = &entries[1];
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

    // ── Metadata: user entries get cwd, gitBranch, version, userType ─

    #[test]
    fn test_user_entry_metadata_from_turn() {
        let mut turn = user_turn("u1", "Hello");
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/home/user/project".to_string()),
            vcs_branch: Some("main".to_string()),
            vcs_revision: None,
        });
        turn.extra.insert(
            "claude".to_string(),
            json!({
                "version": "2.1.37",
                "user_type": "external",
                "entrypoint": "cli",
            }),
        );

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        assert_eq!(entry.cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(entry.git_branch.as_deref(), Some("main"));
        assert_eq!(entry.version.as_deref(), Some("2.1.37"));
        assert_eq!(entry.user_type.as_deref(), Some("external"));
        assert_eq!(entry.extra.get("entrypoint"), Some(&json!("cli")));
    }

    // ── Metadata: assistant entries get requestId ─────────────────────

    #[test]
    fn test_assistant_entry_metadata_request_id() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.extra.insert(
            "claude".to_string(),
            json!({
                "request_id": "req_abc123",
                "version": "2.1.37",
            }),
        );

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        assert_eq!(entry.request_id.as_deref(), Some("req_abc123"));
        assert_eq!(entry.version.as_deref(), Some("2.1.37"));
    }

    // ── Metadata: extras (entrypoint, isMeta, slug) appear ───────────

    #[test]
    fn test_entry_extras_appear_in_projected_entries() {
        let mut turn = user_turn("u1", "Hello");
        turn.extra.insert(
            "claude".to_string(),
            json!({
                "entrypoint": "cli",
                "isMeta": true,
                "slug": "my-slug",
            }),
        );

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        assert_eq!(entry.extra.get("entrypoint"), Some(&json!("cli")));
        assert_eq!(entry.extra.get("isMeta"), Some(&json!(true)));
        assert_eq!(entry.extra.get("slug"), Some(&json!("my-slug")));
    }

    // ── Tool result entries inherit metadata from parent turn ─────────

    #[test]
    fn test_tool_result_entry_inherits_metadata() {
        let mut turn = assistant_turn("a1", "Reading.");
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/project".to_string()),
            vcs_branch: Some("dev".to_string()),
            vcs_revision: None,
        });
        turn.extra.insert(
            "claude".to_string(),
            json!({
                "version": "2.1.37",
                "user_type": "external",
                "entrypoint": "cli",
            }),
        );
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
            result: Some(ToolResult {
                content: "contents".to_string(),
                is_error: false,
            }),
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        assert_eq!(entries.len(), 2);

        let result_entry = &entries[1];
        assert_eq!(result_entry.cwd.as_deref(), Some("/project"));
        assert_eq!(result_entry.git_branch.as_deref(), Some("dev"));
        assert_eq!(result_entry.version.as_deref(), Some("2.1.37"));
        assert_eq!(result_entry.user_type.as_deref(), Some("external"));
        assert_eq!(result_entry.extra.get("entrypoint"), Some(&json!("cli")));
        // sourceToolAssistantUUID should be the parent turn's ID
        assert_eq!(
            result_entry.extra.get("sourceToolAssistantUUID"),
            Some(&json!("a1"))
        );
    }

    // ── Missing metadata fields don't appear (no nulls) ──────────────

    #[test]
    fn test_missing_metadata_no_nulls_in_json() {
        let turn = user_turn("u1", "Hello");
        // No environment, no extra — metadata fields should be absent

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        let json_str = serde_json::to_string(entry).unwrap();
        // None fields with skip_serializing_if should not appear
        assert!(!json_str.contains("\"version\""));
        assert!(!json_str.contains("\"userType\""));
        assert!(!json_str.contains("\"requestId\""));
        assert!(!json_str.contains("\"gitBranch\""));
    }
}
