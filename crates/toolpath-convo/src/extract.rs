//! Reconstruct a [`ConversationView`] from a toolpath [`Path`] using the
//! conversation sub-protocol.
//!
//! The sub-protocol uses three structural change types:
//!
//! - **`conversation.init`** — sets session metadata (provider, session ID)
//! - **`conversation.append`** — adds a turn (user or assistant message)
//! - **`tool.invoke`** — attaches a tool invocation to a parent turn

use std::collections::{HashMap, HashSet};

use chrono::DateTime;
use toolpath::v1::{Path, Step};

use crate::{ConversationView, Role, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn};

/// Extract a [`ConversationView`] from a toolpath [`Path`] document.
///
/// Steps are walked in order (they are already topologically sorted in the
/// path). Structural changes with types `conversation.init`,
/// `conversation.append`, and `tool.invoke` are recognized; everything else
/// is silently skipped.
pub fn extract_conversation(path: &Path) -> ConversationView {
    let mut view = ConversationView {
        id: String::new(),
        started_at: None,
        last_activity: None,
        turns: Vec::new(),
        total_usage: None,
        provider_id: None,
        files_changed: Vec::new(),
        session_ids: Vec::new(),
    };

    // Map from step ID → index into view.turns, for parent lookups.
    let mut step_to_turn: HashMap<&str, usize> = HashMap::new();
    // Track files_changed for dedup in insertion order.
    let mut files_seen: HashSet<String> = HashSet::new();

    for step in &path.steps {
        for (artifact_key, artifact_change) in &step.change {
            let structural = match &artifact_change.structural {
                Some(s) => s,
                None => continue,
            };

            match structural.change_type.as_str() {
                "conversation.init" => {
                    handle_init(&mut view, artifact_key, &structural.extra);
                }
                "conversation.append" => {
                    let turn = build_turn(step, &structural.extra);
                    let idx = view.turns.len();
                    step_to_turn.insert(&step.step.id, idx);
                    view.turns.push(turn);
                }
                "tool.invoke" => {
                    let invocation = build_tool_invocation(&structural.extra);

                    // Track files_changed for file_write tools with non agent:// keys.
                    let category = parse_category(structural.extra.get("category"));
                    if category == Some(ToolCategory::FileWrite)
                        && !artifact_key.starts_with("agent://")
                        && files_seen.insert(artifact_key.clone())
                    {
                        view.files_changed.push(artifact_key.clone());
                    }

                    // Attach to parent turn.
                    if let Some(parent_id) = step.step.parents.first()
                        && let Some(&turn_idx) = step_to_turn.get(parent_id.as_str())
                    {
                        view.turns[turn_idx].tool_uses.push(invocation);
                    }
                }
                _ => {
                    // Unknown structural change type — silently skip.
                }
            }
        }
    }

    // Compute total_usage by summing across turns.
    let mut has_any_usage = false;
    let mut total = TokenUsage::default();
    for turn in &view.turns {
        if let Some(usage) = &turn.token_usage {
            has_any_usage = true;
            total.input_tokens = add_opt(total.input_tokens, usage.input_tokens);
            total.output_tokens = add_opt(total.output_tokens, usage.output_tokens);
            total.cache_read_tokens = add_opt(total.cache_read_tokens, usage.cache_read_tokens);
            total.cache_write_tokens = add_opt(total.cache_write_tokens, usage.cache_write_tokens);
        }
    }
    if has_any_usage {
        view.total_usage = Some(total);
    }

    // Parse timestamps from first/last turns.
    if let Some(first) = view.turns.first() {
        view.started_at = DateTime::parse_from_rfc3339(&first.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }
    if let Some(last) = view.turns.last() {
        view.last_activity = DateTime::parse_from_rfc3339(&last.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }

    view
}

fn handle_init(
    view: &mut ConversationView,
    artifact_key: &str,
    extra: &HashMap<String, serde_json::Value>,
) {
    // Artifact key: agent://<provider>/<session-id>
    if let Some(rest) = artifact_key.strip_prefix("agent://") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            view.provider_id = Some(parts[0].to_string());
            view.id = parts[1].to_string();
        }
    }

    // Also check extra for explicit values.
    if let Some(serde_json::Value::String(v)) = extra.get("version") {
        // Store version in session_ids as a convention, or just note it.
        // For now, version is informational and not mapped to ConversationView fields.
        let _ = v;
    }
}

fn build_turn(step: &Step, extra: &HashMap<String, serde_json::Value>) -> Turn {
    let role = if let Some(serde_json::Value::String(r)) = extra.get("role") {
        parse_role(r)
    } else {
        role_from_actor(&step.step.actor)
    };

    let text = extra
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let thinking = extra
        .get("thinking")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let model = extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let stop_reason = extra
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let token_usage = build_token_usage(extra);

    let parent_id = step.step.parents.first().cloned();

    Turn {
        id: step.step.id.clone(),
        parent_id,
        role,
        timestamp: step.step.timestamp.clone(),
        text,
        thinking,
        tool_uses: Vec::new(),
        model,
        stop_reason,
        token_usage,
        environment: None,
        delegations: Vec::new(),
        extra: HashMap::new(),
    }
}

fn build_token_usage(extra: &HashMap<String, serde_json::Value>) -> Option<TokenUsage> {
    let input = extra.get("input_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);
    let output = extra.get("output_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);
    let cache_read = extra.get("cache_read_tokens").and_then(|v| v.as_u64()).map(|n| n as u32);
    let cache_write = extra
        .get("cache_write_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    if input.is_some() || output.is_some() || cache_read.is_some() || cache_write.is_some() {
        Some(TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        })
    } else {
        None
    }
}

fn build_tool_invocation(extra: &HashMap<String, serde_json::Value>) -> ToolInvocation {
    let id = extra
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let name = extra
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let input = extra
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let is_error = extra
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let result_content = extra.get("result").and_then(|v| v.as_str());
    let result = result_content.map(|content| ToolResult {
        content: content.to_string(),
        is_error,
    });

    let category = parse_category(extra.get("category"));

    ToolInvocation {
        id,
        name,
        input,
        result,
        category,
    }
}

fn parse_category(value: Option<&serde_json::Value>) -> Option<ToolCategory> {
    value
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
}

fn parse_role(s: &str) -> Role {
    match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        other => Role::Other(other.to_string()),
    }
}

fn role_from_actor(actor: &str) -> Role {
    if actor.contains("/tool:") {
        // Tool step — shouldn't be a turn, but if it is, treat as Other.
        Role::Other("tool".to_string())
    } else if actor.starts_with("human:") {
        Role::User
    } else if actor.starts_with("agent:") {
        Role::Assistant
    } else if actor.starts_with("tool:") {
        Role::System
    } else {
        Role::Other(actor.to_string())
    }
}

fn add_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, StructuralChange};

    fn make_path(steps: Vec<Step>) -> Path {
        let head = steps
            .last()
            .map(|s| s.step.id.clone())
            .unwrap_or_default();
        Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head,
            },
            steps,
            meta: None,
        }
    }

    fn make_step(
        id: &str,
        actor: &str,
        timestamp: &str,
        parents: Vec<&str>,
        changes: Vec<(&str, &str, HashMap<String, serde_json::Value>)>,
    ) -> Step {
        let mut change = HashMap::new();
        for (key, change_type, extra) in changes {
            change.insert(
                key.to_string(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: change_type.to_string(),
                        extra,
                    }),
                },
            );
        }
        Step {
            step: toolpath::v1::StepIdentity {
                id: id.to_string(),
                parents: parents.into_iter().map(String::from).collect(),
                actor: actor.to_string(),
                timestamp: timestamp.to_string(),
            },
            change,
            meta: None,
        }
    }

    fn extras(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn test_empty_path() {
        let path = make_path(vec![]);
        let view = extract_conversation(&path);
        assert!(view.id.is_empty());
        assert!(view.turns.is_empty());
        assert!(view.total_usage.is_none());
        assert!(view.started_at.is_none());
        assert!(view.last_activity.is_none());
        assert!(view.files_changed.is_empty());
    }

    #[test]
    fn test_init_sets_metadata() {
        let path = make_path(vec![make_step(
            "step-001",
            "tool:claude-code",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-abc",
                "conversation.init",
                extras(&[("version", serde_json::json!("1.0"))]),
            )],
        )]);

        let view = extract_conversation(&path);
        assert_eq!(view.id, "sess-abc");
        assert_eq!(view.provider_id.as_deref(), Some("claude-code"));
    }

    #[test]
    fn test_simple_conversation() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "tool:claude-code",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.init",
                    HashMap::new(),
                )],
            ),
            make_step(
                "step-002",
                "human:alex",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("Fix the bug")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("I'll fix that.")),
                        ("model", serde_json::json!("claude-opus-4-6")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert_eq!(view.turns[0].id, "step-002");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(view.turns[1].model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn test_tool_invocations_attached_to_parent() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Let me read the file.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/main.rs"})),
                        ("result", serde_json::json!("fn main() {}")),
                        ("is_error", serde_json::json!(false)),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].tool_uses.len(), 1);
        assert_eq!(view.turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(view.turns[0].tool_uses[0].name, "Read");
        assert_eq!(
            view.turns[0].tool_uses[0].category,
            Some(ToolCategory::FileRead)
        );
        assert!(view.turns[0].tool_uses[0].result.is_some());
        assert!(!view.turns[0].tool_uses[0].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_token_usage_extracted_and_totaled() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("hi")),
                        ("input_tokens", serde_json::json!(100)),
                        ("output_tokens", serde_json::json!(50)),
                        ("cache_read_tokens", serde_json::json!(80)),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("more")),
                        ("input_tokens", serde_json::json!(200)),
                        ("output_tokens", serde_json::json!(100)),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(300));
        assert_eq!(total.output_tokens, Some(150));
        assert_eq!(total.cache_read_tokens, Some(80));
        assert!(total.cache_write_tokens.is_none());
    }

    #[test]
    fn test_thinking_blocks_extracted() {
        let path = make_path(vec![make_step(
            "step-001",
            "agent:claude-opus-4-6",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-1",
                "conversation.append",
                extras(&[
                    ("role", serde_json::json!("assistant")),
                    ("text", serde_json::json!("The answer is 42.")),
                    ("thinking", serde_json::json!("Let me think about this carefully...")),
                ]),
            )],
        )]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 1);
        assert_eq!(
            view.turns[0].thinking.as_deref(),
            Some("Let me think about this carefully...")
        );
    }

    #[test]
    fn test_parent_chain_preserved() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("first")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("second")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert!(view.turns[0].parent_id.is_none());
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("step-001"));
    }

    #[test]
    fn test_unknown_structural_change_skipped() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "some.future.type",
                    extras(&[("data", serde_json::json!("whatever"))]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // Only the conversation.append step becomes a turn.
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].text, "hello");
    }

    #[test]
    fn test_role_fallback_from_actor() {
        // No "role" extra — should infer from actor pattern.
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("hello"))]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("hi back"))]),
                )],
            ),
            make_step(
                "step-003",
                "tool:system-prompt",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("system message"))]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[2].role, Role::System);
    }

    #[test]
    fn test_multiple_tool_invocations_same_turn() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Let me check two files.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/main.rs"})),
                        ("result", serde_json::json!("fn main() {}")),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:02Z",
                vec!["step-001"],
                vec![(
                    "src/lib.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-002")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/lib.rs"})),
                        ("result", serde_json::json!("pub mod foo;")),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].tool_uses.len(), 2);
        assert_eq!(view.turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(view.turns[0].tool_uses[1].id, "tu-002");
    }

    #[test]
    fn test_files_changed_from_file_write_tools() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Writing files.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Edit",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Edit")),
                        ("input", serde_json::json!({})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6/tool:Edit",
                "2026-01-01T00:00:02Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-002")),
                        ("name", serde_json::json!("Edit")),
                        ("input", serde_json::json!({})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // Deduped — src/main.rs appears only once.
        assert_eq!(view.files_changed, vec!["src/main.rs"]);
    }

    #[test]
    fn test_timestamps_parsed() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T10:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T10:05:00Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("hi")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert!(view.started_at.is_some());
        assert!(view.last_activity.is_some());
        assert!(view.last_activity.unwrap() > view.started_at.unwrap());
    }

    #[test]
    fn test_steps_without_structural_changes_skipped() {
        let path = make_path(vec![make_step(
            "step-001",
            "human:alex",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![], // no changes at all
        )]);

        let view = extract_conversation(&path);
        assert!(view.turns.is_empty());
    }

    #[test]
    fn test_agent_url_tool_not_in_files_changed() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Searching...")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:WebSearch",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1/tool/network/tu-001",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("WebSearch")),
                        ("input", serde_json::json!({"query": "rust async"})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // agent:// URL should NOT appear in files_changed even with file_write category.
        assert!(view.files_changed.is_empty());
    }
}
