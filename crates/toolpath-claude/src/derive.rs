//! Derive Toolpath documents from Claude conversation logs.
//!
//! The conversation itself is treated as an artifact under change. Each turn
//! appends to `agent://claude/<session-id>` via a `conversation.append`
//! structural operation. Tool invocations produce separate steps with
//! `tool.invoke` structural changes.

use crate::provider::to_view;
use crate::types::{ContentPart, Conversation, MessageContent, MessageRole};
use serde_json::json;
use std::collections::HashMap;
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Identity, Path, PathIdentity, PathMeta, Step,
    StepIdentity, StructuralChange,
};

/// Configuration for deriving Toolpath documents from Claude conversations.
#[derive(Default)]
pub struct DeriveConfig {
    /// Override the project path used for `path.base.uri`.
    pub project_path: Option<String>,
    /// Include thinking blocks in the conversation artifact.
    pub include_thinking: bool,
}

/// Map a Claude tool name to a category string.
fn tool_category_str(name: &str) -> &'static str {
    match name {
        "Read" => "file_read",
        "Glob" | "Grep" => "file_search",
        "Write" | "Edit" | "NotebookEdit" => "file_write",
        "Bash" => "shell",
        "WebFetch" | "WebSearch" => "network",
        "Task" => "delegation",
        _ => "unknown",
    }
}

/// Whether a tool operates on files (uses `file_path` input as artifact key).
fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "Read" | "Write" | "Edit" | "Glob" | "Grep" | "NotebookEdit"
    )
}

/// A collected tool use from a content part.
struct ToolUseInfo {
    id: String,
    name: String,
    input: serde_json::Value,
}

/// Derive a single Toolpath Path from a Claude conversation.
///
/// The conversation is modeled as an artifact at `agent://claude/<session-id>`.
/// Each user or assistant turn produces a step whose `change` map contains
/// a `conversation.append` structural change on that artifact. Assistant turns
/// with tool uses additionally produce one step per tool type, each containing
/// `tool.invoke` structural changes.
pub fn derive_path(conversation: &Conversation, config: &DeriveConfig) -> Path {
    let session_short = safe_prefix(&conversation.session_id, 8);
    let convo_artifact = format!("agent://claude/{}", conversation.session_id);

    // Build a ConversationView with cross-entry tool result assembly
    let view = to_view(conversation);
    let turn_by_id: HashMap<&str, &toolpath_convo::Turn> =
        view.turns.iter().map(|t| (t.id.as_str(), t)).collect();

    let mut steps = Vec::new();
    let mut last_step_id: Option<String> = None;
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();

    // Generate conversation.init step from first entry metadata
    let init_step = {
        let mut init_extra = HashMap::new();
        for entry in &conversation.entries {
            if let Some(cwd) = &entry.cwd {
                init_extra.insert("working_dir".to_string(), json!(cwd));
            }
            if let Some(branch) = &entry.git_branch {
                init_extra.insert("vcs_branch".to_string(), json!(branch));
            }
            if let Some(version) = &entry.version {
                init_extra.insert("version".to_string(), json!(version));
            }
            if !init_extra.is_empty() {
                break;
            }
        }

        if !init_extra.is_empty() {
            let mut changes = HashMap::new();
            changes.insert(
                convo_artifact.clone(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: "conversation.init".to_string(),
                        extra: init_extra,
                    }),
                },
            );

            let step = Step {
                step: StepIdentity {
                    id: format!("{}-init", conversation.session_id),
                    parents: vec![],
                    actor: "tool:claude-code".into(),
                    timestamp: conversation
                        .entries
                        .first()
                        .map(|e| e.timestamp.clone())
                        .unwrap_or_default(),
                },
                change: changes,
                meta: None,
            };
            last_step_id = Some(step.step.id.clone());
            Some(step)
        } else {
            None
        }
    };

    if let Some(init) = init_step {
        actors
            .entry("tool:claude-code".to_string())
            .or_insert_with(|| ActorDefinition {
                name: Some("Claude Code".to_string()),
                ..Default::default()
            });
        steps.push(init);
    }

    for (entry_idx, entry) in conversation.entries.iter().enumerate() {
        // Determine if this is a conversational entry (user/assistant with message)
        // or a non-message event entry
        let message = entry.message.as_ref();
        let is_conversational = message.is_some_and(|m| {
            matches!(m.role, MessageRole::User | MessageRole::Assistant)
        });

        if !is_conversational {
            // Event entry — capture as conversation.event step
            let step_id = if entry.uuid.is_empty() {
                format!("{}-event-{}", conversation.session_id, entry_idx)
            } else {
                entry.uuid.clone()
            };

            let parents = if let Some(parent) = &entry.parent_uuid {
                vec![parent.clone()]
            } else if let Some(ref last) = last_step_id {
                vec![last.clone()]
            } else {
                vec![]
            };

            // Register tool:claude-code actor for event entries
            actors
                .entry("tool:claude-code".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("Claude Code".to_string()),
                    ..Default::default()
                });

            let mut event_extra = HashMap::new();
            event_extra.insert("entry_type".to_string(), json!(entry.entry_type));

            if let Some(cwd) = &entry.cwd {
                event_extra.insert("cwd".to_string(), json!(cwd));
            }
            if let Some(version) = &entry.version {
                event_extra.insert("version".to_string(), json!(version));
            }
            if let Some(git_branch) = &entry.git_branch {
                event_extra.insert("git_branch".to_string(), json!(git_branch));
            }
            if let Some(user_type) = &entry.user_type {
                event_extra.insert("user_type".to_string(), json!(user_type));
            }
            if let Some(snapshot) = &entry.snapshot {
                event_extra.insert("snapshot".to_string(), snapshot.clone());
            }
            if let Some(tool_use_result) = &entry.tool_use_result {
                event_extra.insert("tool_use_result".to_string(), tool_use_result.clone());
            }
            if let Some(message_id) = &entry.message_id {
                event_extra.insert("message_id".to_string(), json!(message_id));
            }
            // Include system message text if present
            if let Some(msg) = message {
                let text = msg.text();
                if !text.is_empty() {
                    event_extra.insert("text".to_string(), json!(text));
                }
            }
            // Entry-level extras
            if !entry.extra.is_empty() {
                event_extra.insert("entry_extra".to_string(), json!(entry.extra));
            }

            let event_step = Step {
                step: StepIdentity {
                    id: step_id,
                    parents,
                    actor: "tool:claude-code".into(),
                    timestamp: entry.timestamp.clone(),
                },
                change: {
                    let mut m = HashMap::new();
                    m.insert(
                        convo_artifact.clone(),
                        ArtifactChange {
                            raw: None,
                            structural: Some(StructuralChange {
                                change_type: "conversation.event".to_string(),
                                extra: event_extra,
                            }),
                        },
                    );
                    m
                },
                meta: None,
            };

            // Event steps do NOT advance last_step_id
            steps.push(event_step);
            continue;
        }

        let message = message.unwrap();

        let (actor, role_str) = match message.role {
            MessageRole::User => {
                actors
                    .entry("human:user".to_string())
                    .or_insert_with(|| ActorDefinition {
                        name: Some("User".to_string()),
                        ..Default::default()
                    });
                ("human:user".to_string(), "user")
            }
            MessageRole::Assistant => {
                let (actor_key, model_str) = if let Some(model) = &message.model {
                    (format!("agent:{}", model), model.clone())
                } else {
                    ("agent:claude-code".to_string(), "claude-code".to_string())
                };
                actors.entry(actor_key.clone()).or_insert_with(|| {
                    let mut identities = vec![Identity {
                        system: "anthropic".to_string(),
                        id: model_str.clone(),
                    }];
                    if let Some(version) = &entry.version {
                        identities.push(Identity {
                            system: "claude-code".to_string(),
                            id: version.clone(),
                        });
                    }
                    ActorDefinition {
                        name: Some("Claude Code".to_string()),
                        provider: Some("anthropic".to_string()),
                        model: Some(model_str),
                        identities,
                        ..Default::default()
                    }
                });
                (actor_key, "assistant")
            }
            // is_conversational guarantees User or Assistant
            MessageRole::System => unreachable!(),
        };

        // Collect conversation text and tool uses from this turn
        let mut text_parts: Vec<String> = Vec::new();
        let mut thinking_parts: Vec<String> = Vec::new();
        let mut tool_use_infos: Vec<ToolUseInfo> = Vec::new();

        match &message.content {
            Some(MessageContent::Parts(parts)) => {
                for part in parts {
                    match part {
                        ContentPart::Text { text }
                            if !text.trim().is_empty() =>
                        {
                            text_parts.push(text.clone());
                        }
                        ContentPart::Thinking { thinking, .. } => {
                            if config.include_thinking && !thinking.trim().is_empty() {
                                thinking_parts.push(thinking.clone());
                            }
                        }
                        ContentPart::ToolUse { id, name, input } => {
                            tool_use_infos.push(ToolUseInfo {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some(MessageContent::Text(text)) if !text.trim().is_empty() => {
                text_parts.push(text.clone());
            }
            _ => {}
        }

        // Collect tool name list for the summary field
        let tool_names: Vec<String> = tool_use_infos.iter().map(|t| t.name.clone()).collect();

        // Skip entries with no conversation content and no tool uses
        if text_parts.is_empty() && thinking_parts.is_empty() && tool_use_infos.is_empty() {
            continue;
        }

        // Build the conversation artifact change
        let mut convo_extra = HashMap::new();
        convo_extra.insert("role".to_string(), json!(role_str));
        if !text_parts.is_empty() {
            let combined = text_parts.join("\n\n");
            convo_extra.insert("text".to_string(), json!(combined));
        }
        if !thinking_parts.is_empty() {
            let combined_thinking = thinking_parts.join("\n\n");
            convo_extra.insert("thinking".to_string(), json!(combined_thinking));
        }
        if !tool_names.is_empty() {
            convo_extra.insert("tool_uses".to_string(), json!(tool_names));
        }

        // Add model, stop_reason, and usage fields from the message
        if let Some(model) = &message.model {
            convo_extra.insert("model".to_string(), json!(model));
        }
        if let Some(stop_reason) = &message.stop_reason {
            convo_extra.insert("stop_reason".to_string(), json!(stop_reason));
        }
        if let Some(usage) = &message.usage {
            if let Some(input_tokens) = usage.input_tokens {
                convo_extra.insert("input_tokens".to_string(), json!(input_tokens));
            }
            if let Some(output_tokens) = usage.output_tokens {
                convo_extra.insert("output_tokens".to_string(), json!(output_tokens));
            }
            if let Some(cache_read) = usage.cache_read_input_tokens {
                convo_extra.insert("cache_read_tokens".to_string(), json!(cache_read));
            }
            if let Some(cache_write) = usage.cache_creation_input_tokens {
                convo_extra.insert("cache_write_tokens".to_string(), json!(cache_write));
            }
        }

        // Per-entry metadata for round-trip fidelity
        if let Some(cwd) = &entry.cwd {
            convo_extra.insert("cwd".to_string(), json!(cwd));
        }
        if let Some(version) = &entry.version {
            convo_extra.insert("version".to_string(), json!(version));
        }
        if let Some(git_branch) = &entry.git_branch {
            convo_extra.insert("git_branch".to_string(), json!(git_branch));
        }
        if let Some(user_type) = &entry.user_type {
            convo_extra.insert("user_type".to_string(), json!(user_type));
        }
        if let Some(request_id) = &entry.request_id {
            convo_extra.insert("request_id".to_string(), json!(request_id));
        }
        // Entry-level extras (isMeta, slug, entrypoint, promptId, etc.)
        if !entry.extra.is_empty() {
            convo_extra.insert("entry_extra".to_string(), json!(entry.extra));
        }

        let convo_change = ArtifactChange {
            raw: None,
            structural: Some(StructuralChange {
                change_type: "conversation.append".to_string(),
                extra: convo_extra,
            }),
        };

        let mut changes = HashMap::new();
        changes.insert(convo_artifact.clone(), convo_change);

        // Build conversation step using full UUID as step ID
        let step_id = entry.uuid.clone();
        let parents = if entry.is_sidechain {
            entry
                .parent_uuid
                .as_ref()
                .cloned()
                .into_iter()
                .collect()
        } else {
            last_step_id.iter().cloned().collect()
        };

        let step = Step {
            step: StepIdentity {
                id: step_id.clone(),
                parents,
                actor,
                timestamp: entry.timestamp.clone(),
            },
            change: changes,
            meta: None,
        };

        if !entry.is_sidechain {
            last_step_id = Some(step_id.clone());
        }
        steps.push(step);

        // Emit tool invocation steps (one per tool type, grouped)
        if !tool_use_infos.is_empty() {
            // Group tool uses by tool name, preserving order of first occurrence
            let mut tool_groups: Vec<(String, Vec<&ToolUseInfo>)> = Vec::new();
            let mut group_index: HashMap<String, usize> = HashMap::new();

            for tool_use in &tool_use_infos {
                if let Some(&idx) = group_index.get(&tool_use.name) {
                    tool_groups[idx].1.push(tool_use);
                } else {
                    let idx = tool_groups.len();
                    group_index.insert(tool_use.name.clone(), idx);
                    tool_groups.push((tool_use.name.clone(), vec![tool_use]));
                }
            }

            for (tool_name, uses) in &tool_groups {
                let tool_step_id = format!("{}-tool-{}", entry.uuid, tool_name);
                let tool_actor = format!("agent:claude-code/tool:{}", tool_name);

                // Register the tool actor
                actors
                    .entry(tool_actor.clone())
                    .or_insert_with(|| ActorDefinition {
                        name: Some(format!("Claude Code / {}", tool_name)),
                        ..Default::default()
                    });

                let mut tool_changes = HashMap::new();
                let category = tool_category_str(tool_name);

                for tool_use in uses {
                    // Determine artifact key
                    let artifact_key = if is_file_tool(tool_name) {
                        tool_use
                            .input
                            .get("file_path")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| {
                                format!(
                                    "agent://claude/{}/tool/{}/{}",
                                    conversation.session_id, category, tool_use.id
                                )
                            })
                    } else {
                        format!(
                            "agent://claude/{}/tool/{}/{}",
                            conversation.session_id, category, tool_use.id
                        )
                    };

                    let mut extra = HashMap::new();
                    extra.insert("tool_use_id".to_string(), json!(tool_use.id));
                    extra.insert("name".to_string(), json!(tool_use.name));
                    extra.insert("input".to_string(), tool_use.input.clone());
                    extra.insert("category".to_string(), json!(category));

                    // Look up assembled tool result from ConversationView
                    if let Some(turn) = turn_by_id.get(entry.uuid.as_str())
                        && let Some(invocation) =
                            turn.tool_uses.iter().find(|tu| tu.id == tool_use.id)
                        && let Some(result) = &invocation.result
                    {
                        extra.insert("result".to_string(), json!(result.content));
                        extra.insert("is_error".to_string(), json!(result.is_error));
                    }

                    tool_changes.insert(
                        artifact_key,
                        ArtifactChange {
                            raw: None,
                            structural: Some(StructuralChange {
                                change_type: "tool.invoke".to_string(),
                                extra,
                            }),
                        },
                    );
                }

                let tool_step = Step {
                    step: StepIdentity {
                        id: tool_step_id,
                        parents: vec![step_id.clone()],
                        actor: tool_actor,
                        timestamp: entry.timestamp.clone(),
                    },
                    change: tool_changes,
                    meta: None,
                };

                // Tool steps do NOT advance last_step_id
                steps.push(tool_step);
            }
        }
    }

    let head = last_step_id.unwrap_or_else(|| "empty".to_string());
    let base_uri = config
        .project_path
        .as_deref()
        .or(conversation.project_path.as_deref())
        .map(|p| format!("file://{}", p));

    Path {
        path: PathIdentity {
            id: format!("path-claude-{}", session_short),
            base: base_uri.map(|uri| Base { uri, ref_str: None }),
            head,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!("Claude session: {}", session_short)),
            source: Some("claude-code".to_string()),
            actors: if actors.is_empty() {
                None
            } else {
                Some(actors)
            },
            ..Default::default()
        }),
    }
}

/// Derive Toolpath Paths from multiple conversations in a project.
pub fn derive_project(conversations: &[Conversation], config: &DeriveConfig) -> Vec<Path> {
    conversations
        .iter()
        .map(|c| derive_path(c, config))
        .collect()
}

/// Truncate a string to at most `max` characters (not bytes), appending "..."
/// if truncated. Always cuts on a char boundary.
#[cfg(test)]
fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    }
}

/// Return the first `n` characters of a string, safe for any UTF-8 content.
fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentPart, ConversationEntry, Message, MessageContent, Usage};

    fn make_entry(
        uuid: &str,
        role: MessageRole,
        content: &str,
        timestamp: &str,
    ) -> ConversationEntry {
        ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: match role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
            }
            .to_string(),
            uuid: uuid.to_string(),
            timestamp: timestamp.to_string(),
            session_id: Some("test-session".to_string()),
            cwd: None,
            git_branch: None,
            version: None,
            message: Some(Message {
                role,
                content: Some(MessageContent::Text(content.to_string())),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        }
    }

    fn make_conversation(entries: Vec<ConversationEntry>) -> Conversation {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        for entry in entries {
            convo.add_entry(entry);
        }
        convo
    }

    // ── truncate ───────────────────────────────────────────────────────

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("hello world, this is long", 10);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn test_truncate_multibyte() {
        // Should not panic on multi-byte characters
        let s = "cafe\u{0301} re\u{0301}sume\u{0301} nai\u{0308}ve";
        let result = truncate(s, 8);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 8);
    }

    // ── safe_prefix ────────────────────────────────────────────────────

    #[test]
    fn test_safe_prefix_normal() {
        assert_eq!(safe_prefix("abcdef1234", 8), "abcdef12");
    }

    #[test]
    fn test_safe_prefix_short() {
        assert_eq!(safe_prefix("abc", 8), "abc");
    }

    #[test]
    fn test_safe_prefix_unicode() {
        assert_eq!(safe_prefix("\u{65E5}\u{672C}\u{8A9E}\u{30C6}\u{30B9}\u{30C8}", 3), "\u{65E5}\u{672C}\u{8A9E}");
    }

    // ── tool helpers ──────────────────────────────────────────────────

    #[test]
    fn test_tool_category_str() {
        assert_eq!(tool_category_str("Read"), "file_read");
        assert_eq!(tool_category_str("Write"), "file_write");
        assert_eq!(tool_category_str("Edit"), "file_write");
        assert_eq!(tool_category_str("Glob"), "file_search");
        assert_eq!(tool_category_str("Grep"), "file_search");
        assert_eq!(tool_category_str("Bash"), "shell");
        assert_eq!(tool_category_str("WebFetch"), "network");
        assert_eq!(tool_category_str("Task"), "delegation");
        assert_eq!(tool_category_str("SomethingElse"), "unknown");
    }

    #[test]
    fn test_is_file_tool() {
        assert!(is_file_tool("Read"));
        assert!(is_file_tool("Write"));
        assert!(is_file_tool("Edit"));
        assert!(is_file_tool("Glob"));
        assert!(is_file_tool("Grep"));
        assert!(is_file_tool("NotebookEdit"));
        assert!(!is_file_tool("Bash"));
        assert!(!is_file_tool("WebFetch"));
        assert!(!is_file_tool("Task"));
    }

    // ── derive_path ────────────────────────────────────────────────────

    #[test]
    fn test_derive_path_basic() {
        let entries = vec![
            make_entry(
                "uuid-1111-aaaa",
                MessageRole::User,
                "Hello",
                "2024-01-01T00:00:00Z",
            ),
            make_entry(
                "uuid-2222-bbbb",
                MessageRole::Assistant,
                "Hi there",
                "2024-01-01T00:00:01Z",
            ),
        ];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        assert!(path.path.id.starts_with("path-claude-"));
        assert_eq!(path.steps.len(), 2);
        // Step IDs are full UUIDs
        assert_eq!(path.steps[0].step.id, "uuid-1111-aaaa");
        assert_eq!(path.steps[1].step.id, "uuid-2222-bbbb");
        assert_eq!(path.steps[0].step.actor, "human:user");
        assert!(path.steps[1].step.actor.starts_with("agent:"));
    }

    #[test]
    fn test_derive_path_step_parents() {
        let entries = vec![
            make_entry(
                "uuid-1111",
                MessageRole::User,
                "Hello",
                "2024-01-01T00:00:00Z",
            ),
            make_entry(
                "uuid-2222",
                MessageRole::Assistant,
                "Hi",
                "2024-01-01T00:00:01Z",
            ),
            make_entry(
                "uuid-3333",
                MessageRole::User,
                "More",
                "2024-01-01T00:00:02Z",
            ),
        ];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // Parents are full UUIDs
        assert!(path.steps[1].step.parents.contains(&"uuid-1111".to_string()));
        assert!(path.steps[2].step.parents.contains(&"uuid-2222".to_string()));
    }

    #[test]
    fn test_derive_path_conversation_artifact() {
        let entries = vec![make_entry(
            "uuid-1111",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // Artifact key uses agent:// scheme
        let convo_key = format!("agent://claude/{}", convo.session_id);
        assert!(path.steps[0].change.contains_key(&convo_key));

        let change = &path.steps[0].change[&convo_key];
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "conversation.append");
        assert_eq!(structural.extra["role"], "user");
    }

    #[test]
    fn test_derive_path_no_meta_intent() {
        let entries = vec![make_entry(
            "uuid-1111",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // meta.intent should NOT be set (we removed it as redundant)
        assert!(path.steps[0].meta.is_none());
    }

    #[test]
    fn test_derive_path_actors() {
        let entries = vec![
            make_entry(
                "uuid-1111",
                MessageRole::User,
                "Hello",
                "2024-01-01T00:00:00Z",
            ),
            make_entry(
                "uuid-2222",
                MessageRole::Assistant,
                "Hi",
                "2024-01-01T00:00:01Z",
            ),
        ];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();

        assert!(actors.contains_key("human:user"));
        // Assistant actor depends on model (None in our test)
        assert!(actors.contains_key("agent:claude-code"));
    }

    #[test]
    fn test_derive_path_with_project_path_config() {
        let convo = make_conversation(vec![make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )]);
        let config = DeriveConfig {
            project_path: Some("/my/project".to_string()),
            ..Default::default()
        };

        let path = derive_path(&convo, &config);
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///my/project");
    }

    #[test]
    fn test_derive_path_skips_empty_content() {
        let mut entry = make_entry("uuid-1111", MessageRole::User, "", "2024-01-01T00:00:00Z");
        // Empty text, no tool uses, no file changes -> should be skipped
        entry.message.as_mut().unwrap().content = Some(MessageContent::Text("   ".to_string()));

        let convo = make_conversation(vec![entry]);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);
        assert!(path.steps.is_empty());
    }

    #[test]
    fn test_derive_path_captures_system_messages_as_events() {
        let entries = vec![
            make_entry(
                "uuid-1111",
                MessageRole::System,
                "System prompt",
                "2024-01-01T00:00:00Z",
            ),
            make_entry(
                "uuid-2222",
                MessageRole::User,
                "Hello",
                "2024-01-01T00:00:01Z",
            ),
        ];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);
        // System message captured as event, plus user message
        assert_eq!(path.steps.len(), 2);
        // First step is the system event
        assert_eq!(path.steps[0].step.actor, "tool:claude-code");
        let convo_key = format!("agent://claude/{}", convo.session_id);
        let structural = path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(structural.change_type, "conversation.event");
        assert_eq!(structural.extra["entry_type"], "system");
        assert_eq!(structural.extra["text"], "System prompt");
        // Second step is the user message
        assert_eq!(path.steps[1].step.actor, "human:user");
    }

    #[test]
    fn test_derive_path_with_tool_use() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let entry = ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-tool".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Let me write that".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Write".to_string(),
                        input: serde_json::json!({"file_path": "/tmp/test.rs"}),
                    },
                ])),
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        };
        convo.add_entry(entry);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // Now produces 2 steps: conversation + tool
        assert_eq!(path.steps.len(), 2);

        // Conversation step has the conversation artifact
        let convo_key = format!("agent://claude/{}", convo.session_id);
        assert!(path.steps[0].change.contains_key(&convo_key));

        // Tool step has the file artifact with tool.invoke
        assert_eq!(path.steps[1].step.id, "uuid-tool-tool-Write");
        assert_eq!(path.steps[1].step.actor, "agent:claude-code/tool:Write");
        assert!(path.steps[1].step.parents.contains(&"uuid-tool".to_string()));
        assert!(path.steps[1].change.contains_key("/tmp/test.rs"));

        let tool_change = &path.steps[1].change["/tmp/test.rs"];
        let structural = tool_change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "tool.invoke");
        assert_eq!(structural.extra["name"], "Write");
        assert_eq!(structural.extra["tool_use_id"], "t1");
        assert_eq!(structural.extra["category"], "file_write");
    }

    #[test]
    fn test_derive_path_sidechain_uses_parent_uuid() {
        let mut convo = Conversation::new("test-session-12345678".to_string());

        let e1 = make_entry(
            "uuid-main-11",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        let e2 = make_entry(
            "uuid-main-22",
            MessageRole::Assistant,
            "Hi",
            "2024-01-01T00:00:01Z",
        );
        let mut e3 = make_entry(
            "uuid-side-33",
            MessageRole::User,
            "Side",
            "2024-01-01T00:00:02Z",
        );
        e3.is_sidechain = true;
        e3.parent_uuid = Some("uuid-main-11".to_string());

        convo.add_entry(e1);
        convo.add_entry(e2);
        convo.add_entry(e3);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        assert_eq!(path.steps.len(), 3);
        // Sidechain step should reference e1's full UUID as parent
        let sidechain_step = &path.steps[2];
        assert!(sidechain_step
            .step
            .parents
            .contains(&"uuid-main-11".to_string()));
    }

    // ── derive_project ─────────────────────────────────────────────────

    #[test]
    fn test_derive_project() {
        let c1 = make_conversation(vec![make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )]);
        let mut c2 = Conversation::new("session-2".to_string());
        c2.add_entry(make_entry(
            "uuid-2",
            MessageRole::User,
            "World",
            "2024-01-02T00:00:00Z",
        ));

        let config = DeriveConfig::default();
        let paths = derive_project(&[c1, c2], &config);

        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_derive_path_head_is_last_non_sidechain() {
        let entries = vec![
            make_entry(
                "uuid-1111",
                MessageRole::User,
                "Hello",
                "2024-01-01T00:00:00Z",
            ),
            make_entry(
                "uuid-2222",
                MessageRole::Assistant,
                "Hi",
                "2024-01-01T00:00:01Z",
            ),
        ];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // Head should point to the last conversation step (full UUID)
        assert_eq!(path.path.head, "uuid-2222");
    }

    // ── new tests for enriched derive ──────────────────────────────────

    #[test]
    fn test_derive_path_tool_invocation_actors() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Working".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/foo.rs"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });
        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("agent:claude-code/tool:Read"));
    }

    #[test]
    fn test_derive_path_token_usage() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-usage".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text("Response".to_string())),
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                id: None,
                message_type: None,
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                usage: Some(Usage {
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    cache_creation_input_tokens: Some(10),
                    cache_read_input_tokens: Some(80),
                    cache_creation: None,
                    service_tier: None,
                }),
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let change = &path.steps[0].change[&convo_key];
        let extra = &change.structural.as_ref().unwrap().extra;

        assert_eq!(extra["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(extra["stop_reason"], "end_turn");
        assert_eq!(extra["input_tokens"], 100);
        assert_eq!(extra["output_tokens"], 50);
        assert_eq!(extra["cache_read_tokens"], 80);
        assert_eq!(extra["cache_write_tokens"], 10);
    }

    #[test]
    fn test_derive_path_full_text_no_truncation() {
        let long_text = "a".repeat(5000);
        let entries = vec![make_entry(
            "uuid-long",
            MessageRole::User,
            &long_text,
            "2024-01-01T00:00:00Z",
        )];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let change = &path.steps[0].change[&convo_key];
        let text = change.structural.as_ref().unwrap().extra["text"]
            .as_str()
            .unwrap();
        assert_eq!(text.len(), 5000);
        assert!(!text.ends_with("..."));
    }

    #[test]
    fn test_derive_path_multiple_tool_uses_same_type() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-multi".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Reading files".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/foo.rs"}),
                    },
                    ContentPart::ToolUse {
                        id: "t2".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/bar.rs"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // 1 conversation step + 1 tool step (both Reads grouped)
        assert_eq!(path.steps.len(), 2);
        assert_eq!(path.steps[1].step.id, "uuid-multi-tool-Read");
        // Two artifact changes in the tool step
        assert_eq!(path.steps[1].change.len(), 2);
        assert!(path.steps[1].change.contains_key("/foo.rs"));
        assert!(path.steps[1].change.contains_key("/bar.rs"));
    }

    #[test]
    fn test_derive_path_multiple_tool_uses_different_types() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-diff".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Working".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/foo.rs"}),
                    },
                    ContentPart::ToolUse {
                        id: "t2".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "cargo test"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // 1 conversation step + 2 tool steps (Read and Bash)
        assert_eq!(path.steps.len(), 3);
        assert_eq!(path.steps[1].step.id, "uuid-diff-tool-Read");
        assert_eq!(path.steps[2].step.id, "uuid-diff-tool-Bash");

        // Bash tool uses agent:// URI since it's not a file tool
        let bash_change = &path.steps[2].change;
        assert_eq!(bash_change.len(), 1);
        let bash_key = bash_change.keys().next().unwrap();
        assert!(bash_key.starts_with("agent://claude/"));
        assert!(bash_key.contains("/tool/shell/"));
    }

    #[test]
    fn test_derive_path_non_file_tool_artifact_key() {
        let mut convo = Conversation::new("sess-123".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-bash".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Running".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "tu-42".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let tool_step = &path.steps[1];
        let expected_key = "agent://claude/sess-123/tool/shell/tu-42";
        assert!(tool_step.change.contains_key(expected_key));
    }

    #[test]
    fn test_derive_path_thinking_included_when_configured() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-think".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Thinking {
                        thinking: "Let me think about this".to_string(),
                        signature: None,
                    },
                    ContentPart::Text {
                        text: "Here is my answer".to_string(),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        // With thinking enabled
        let config = DeriveConfig {
            include_thinking: true,
            ..Default::default()
        };
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;
        assert_eq!(extra["thinking"], "Let me think about this");
        // Text should be separate from thinking
        assert_eq!(extra["text"], "Here is my answer");
    }

    #[test]
    fn test_derive_path_thinking_excluded_by_default() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-think2".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Thinking {
                        thinking: "Secret thoughts".to_string(),
                        signature: None,
                    },
                    ContentPart::Text {
                        text: "Answer".to_string(),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;
        assert!(!extra.contains_key("thinking"));
    }

    #[test]
    fn test_derive_path_tool_step_does_not_advance_parent_chain() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-a1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Writing".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Write".to_string(),
                        input: serde_json::json!({"file_path": "/f.rs"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });
        convo.add_entry(make_entry(
            "uuid-u2",
            MessageRole::User,
            "Next",
            "2024-01-01T00:00:01Z",
        ));

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // Steps: conversation(uuid-a1), tool(uuid-a1-tool-Write), conversation(uuid-u2)
        assert_eq!(path.steps.len(), 3);
        // The user step's parent should be the conversation step, not the tool step
        assert_eq!(path.steps[2].step.parents, vec!["uuid-a1".to_string()]);
    }

    #[test]
    fn test_derive_path_tool_input_preserved() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let input_json = serde_json::json!({
            "file_path": "/src/main.rs",
            "content": "fn main() {}\n"
        });
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-inp".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Writing".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "t1".to_string(),
                        name: "Write".to_string(),
                        input: input_json.clone(),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let tool_step = &path.steps[1];
        let change = &tool_step.change["/src/main.rs"];
        let extra = &change.structural.as_ref().unwrap().extra;
        assert_eq!(extra["input"], input_json);
    }

    // ── tool result assembly ──────────────────────────────────────────

    #[test]
    fn test_derive_path_tool_result_assembled() {
        use crate::types::ToolResultContent;

        let mut convo = Conversation::new("test-session-12345678".to_string());

        // Assistant entry with a tool use
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-assist-1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Let me read that file".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "tu-read-1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/src/lib.rs"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        // Tool-result-only user entry
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "user".to_string(),
            uuid: "uuid-result-1".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Parts(vec![ContentPart::ToolResult {
                    tool_use_id: "tu-read-1".to_string(),
                    content: ToolResultContent::Text("fn main() {}".to_string()),
                    is_error: false,
                }])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // Should produce 2 steps: conversation + tool (tool-result-only entry skipped)
        assert_eq!(path.steps.len(), 2);

        // The tool step should have the result assembled
        let tool_step = &path.steps[1];
        assert_eq!(tool_step.step.id, "uuid-assist-1-tool-Read");
        let change = &tool_step.change["/src/lib.rs"];
        let extra = &change.structural.as_ref().unwrap().extra;
        assert_eq!(extra["result"], "fn main() {}");
        assert_eq!(extra["is_error"], false);
    }

    #[test]
    fn test_derive_path_tool_result_error() {
        use crate::types::ToolResultContent;

        let mut convo = Conversation::new("test-session-12345678".to_string());

        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-assist-err".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![
                    ContentPart::Text {
                        text: "Running command".to_string(),
                    },
                    ContentPart::ToolUse {
                        id: "tu-bash-1".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "cargo test"}),
                    },
                ])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        // Tool result with error
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "user".to_string(),
            uuid: "uuid-result-err".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Parts(vec![ContentPart::ToolResult {
                    tool_use_id: "tu-bash-1".to_string(),
                    content: ToolResultContent::Text("compilation failed".to_string()),
                    is_error: true,
                }])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let tool_step = &path.steps[1];
        let bash_key = tool_step.change.keys().next().unwrap();
        let extra = &tool_step.change[bash_key].structural.as_ref().unwrap().extra;
        assert_eq!(extra["result"], "compilation failed");
        assert_eq!(extra["is_error"], true);
    }

    // ── conversation.init step ────────────────────────────────────────

    #[test]
    fn test_derive_path_init_step_with_cwd() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.cwd = Some("/home/user/project".to_string());
        entry.version = Some("1.2.3".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // Should have init step + conversation step
        assert_eq!(path.steps.len(), 2);

        let init = &path.steps[0];
        assert_eq!(init.step.id, "test-session-12345678-init");
        assert_eq!(init.step.actor, "tool:claude-code");
        assert!(init.step.parents.is_empty());

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let structural = init.change[&convo_key].structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "conversation.init");
        assert_eq!(structural.extra["working_dir"], "/home/user/project");
        assert_eq!(structural.extra["version"], "1.2.3");
    }

    #[test]
    fn test_derive_path_init_step_is_parent_of_first() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.cwd = Some("/project".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // The first conversation step should have init as parent
        assert_eq!(path.steps.len(), 2);
        assert_eq!(
            path.steps[1].step.parents,
            vec!["test-session-12345678-init".to_string()]
        );
    }

    #[test]
    fn test_derive_path_init_step_with_git_branch() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.git_branch = Some("feature/foo".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        assert_eq!(path.steps.len(), 2);
        let init = &path.steps[0];
        let convo_key = format!("agent://claude/{}", convo.session_id);
        let structural = init.change[&convo_key].structural.as_ref().unwrap();
        assert_eq!(structural.extra["vcs_branch"], "feature/foo");
    }

    #[test]
    fn test_derive_path_no_init_step_without_metadata() {
        // Standard make_entry has no cwd/version/git_branch
        let entries = vec![make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        // No init step should be generated
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.steps[0].step.id, "uuid-1");
    }

    // ── per-entry metadata capture ──────────────────────────────────

    #[test]
    fn test_derive_path_captures_cwd_and_git_branch() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-meta-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.cwd = Some("/home/user/project".to_string());
        entry.git_branch = Some("main".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // Find the conversation.append step (skip init step)
        let convo_key = format!("agent://claude/{}", convo.session_id);
        let append_step = path
            .steps
            .iter()
            .find(|s| {
                s.change
                    .get(&convo_key)
                    .and_then(|c| c.structural.as_ref())
                    .is_some_and(|sc| sc.change_type == "conversation.append")
            })
            .expect("should have a conversation.append step");
        let extra = &append_step.change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        assert_eq!(extra["cwd"], "/home/user/project");
        assert_eq!(extra["git_branch"], "main");
    }

    #[test]
    fn test_derive_path_captures_version() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-meta-2",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.version = Some("1.5.0".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let append_step = path
            .steps
            .iter()
            .find(|s| {
                s.change
                    .get(&convo_key)
                    .and_then(|c| c.structural.as_ref())
                    .is_some_and(|sc| sc.change_type == "conversation.append")
            })
            .expect("should have a conversation.append step");
        let extra = &append_step.change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        assert_eq!(extra["version"], "1.5.0");
    }

    #[test]
    fn test_derive_path_captures_user_type_and_request_id() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "uuid-meta-3".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text("Response".to_string())),
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: Some("external".to_string()),
            request_id: Some("req-abc-123".to_string()),
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        assert_eq!(extra["user_type"], "external");
        assert_eq!(extra["request_id"], "req-abc-123");
    }

    #[test]
    fn test_derive_path_captures_entry_extra() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry_extra = HashMap::new();
        entry_extra.insert(
            "entrypoint".to_string(),
            serde_json::json!("cli"),
        );
        entry_extra.insert(
            "isMeta".to_string(),
            serde_json::json!(true),
        );
        entry_extra.insert(
            "slug".to_string(),
            serde_json::json!("my-slug"),
        );

        convo.add_entry(ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "user".to_string(),
            uuid: "uuid-meta-4".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test-session".to_string()),
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text("Hello".to_string())),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: entry_extra,
        });

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        let entry_extra_val = extra.get("entry_extra").expect("entry_extra should be present");
        assert_eq!(entry_extra_val["entrypoint"], "cli");
        assert_eq!(entry_extra_val["isMeta"], true);
        assert_eq!(entry_extra_val["slug"], "my-slug");
    }

    #[test]
    fn test_derive_path_missing_metadata_not_included() {
        // Standard make_entry has no cwd/version/git_branch/user_type/request_id/extra
        let entries = vec![make_entry(
            "uuid-meta-5",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        )];
        let convo = make_conversation(entries);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        // None of the per-entry metadata fields should be present
        assert!(!extra.contains_key("cwd"));
        assert!(!extra.contains_key("version"));
        assert!(!extra.contains_key("git_branch"));
        assert!(!extra.contains_key("user_type"));
        assert!(!extra.contains_key("request_id"));
        assert!(!extra.contains_key("entry_extra"));
    }

    #[test]
    fn test_derive_path_init_step_actor_registered() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut entry = make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        );
        entry.cwd = Some("/project".to_string());
        convo.add_entry(entry);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("tool:claude-code"));
        assert_eq!(
            actors["tool:claude-code"].name.as_deref(),
            Some("Claude Code")
        );
    }

    // ── conversation.event steps (non-message entries) ────────────────

    fn make_event_entry(
        uuid: &str,
        entry_type: &str,
        timestamp: &str,
    ) -> ConversationEntry {
        ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: entry_type.to_string(),
            uuid: uuid.to_string(),
            timestamp: timestamp.to_string(),
            session_id: Some("test-session".to_string()),
            cwd: None,
            git_branch: None,
            version: None,
            message: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_derive_path_attachment_entry_captured_as_event() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(make_entry(
            "uuid-1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        ));
        convo.add_entry(make_event_entry(
            "uuid-attach-1",
            "attachment",
            "2024-01-01T00:00:01Z",
        ));
        convo.add_entry(make_entry(
            "uuid-2",
            MessageRole::Assistant,
            "Hi",
            "2024-01-01T00:00:02Z",
        ));

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // 3 steps: user, attachment event, assistant
        assert_eq!(path.steps.len(), 3);

        let event_step = &path.steps[1];
        assert_eq!(event_step.step.id, "uuid-attach-1");
        assert_eq!(event_step.step.actor, "tool:claude-code");

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let structural = event_step.change[&convo_key]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(structural.change_type, "conversation.event");
        assert_eq!(structural.extra["entry_type"], "attachment");
    }

    #[test]
    fn test_derive_path_system_entry_captured_as_event() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(make_entry(
            "uuid-sys",
            MessageRole::System,
            "Turn duration: 5s",
            "2024-01-01T00:00:00Z",
        ));

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        assert_eq!(path.steps.len(), 1);
        let event_step = &path.steps[0];
        assert_eq!(event_step.step.actor, "tool:claude-code");

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let structural = event_step.change[&convo_key]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(structural.change_type, "conversation.event");
        assert_eq!(structural.extra["entry_type"], "system");
        assert_eq!(structural.extra["text"], "Turn duration: 5s");
    }

    #[test]
    fn test_derive_path_empty_uuid_entry_gets_synthetic_id() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut event = make_event_entry("", "permission-mode", "2024-01-01T00:00:00Z");
        event.uuid = String::new();
        convo.add_entry(event);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        assert_eq!(path.steps.len(), 1);
        // Synthetic ID: {session_id}-event-{index}
        assert_eq!(
            path.steps[0].step.id,
            "test-session-12345678-event-0"
        );
    }

    #[test]
    fn test_derive_path_event_steps_dont_advance_parent_chain() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(make_entry(
            "uuid-u1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        ));
        convo.add_entry(make_event_entry(
            "uuid-attach",
            "attachment",
            "2024-01-01T00:00:01Z",
        ));
        convo.add_entry(make_entry(
            "uuid-a1",
            MessageRole::Assistant,
            "Hi",
            "2024-01-01T00:00:02Z",
        ));

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        assert_eq!(path.steps.len(), 3);
        // The assistant step's parent should be the USER step, not the event step
        assert_eq!(
            path.steps[2].step.parents,
            vec!["uuid-u1".to_string()]
        );
        // The head should be the assistant step, not the event step
        assert_eq!(path.path.head, "uuid-a1");
    }

    #[test]
    fn test_derive_path_event_step_extras_contain_metadata() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut event = make_event_entry(
            "uuid-ev1",
            "file-history-snapshot",
            "2024-01-01T00:00:00Z",
        );
        event.cwd = Some("/home/user/project".to_string());
        event.version = Some("1.5.0".to_string());
        event.git_branch = Some("main".to_string());
        event.user_type = Some("external".to_string());
        event.snapshot = Some(serde_json::json!({"files": ["/src/main.rs"]}));
        event.message_id = Some("msg-123".to_string());
        convo.add_entry(event);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // First step is init (because cwd is present), second is the event
        let convo_key = format!("agent://claude/{}", convo.session_id);
        // Find the event step (skip init)
        let event_step = path
            .steps
            .iter()
            .find(|s| {
                s.change
                    .get(&convo_key)
                    .and_then(|c| c.structural.as_ref())
                    .is_some_and(|sc| sc.change_type == "conversation.event")
            })
            .expect("should have a conversation.event step");
        let extra = &event_step.change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        assert_eq!(extra["entry_type"], "file-history-snapshot");
        assert_eq!(extra["cwd"], "/home/user/project");
        assert_eq!(extra["version"], "1.5.0");
        assert_eq!(extra["git_branch"], "main");
        assert_eq!(extra["user_type"], "external");
        assert_eq!(extra["snapshot"], serde_json::json!({"files": ["/src/main.rs"]}));
        assert_eq!(extra["message_id"], "msg-123");
    }

    #[test]
    fn test_derive_path_event_entry_extra_preserved() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut event = make_event_entry(
            "uuid-ev2",
            "attachment",
            "2024-01-01T00:00:00Z",
        );
        let mut extras = HashMap::new();
        extras.insert("hookName".to_string(), serde_json::json!("pre-tool-use"));
        extras.insert("toolName".to_string(), serde_json::json!("Bash"));
        event.extra = extras;
        convo.add_entry(event);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        let entry_extra = extra.get("entry_extra").expect("entry_extra should be present");
        assert_eq!(entry_extra["hookName"], "pre-tool-use");
        assert_eq!(entry_extra["toolName"], "Bash");
    }

    #[test]
    fn test_derive_path_event_with_parent_uuid() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        convo.add_entry(make_entry(
            "uuid-u1",
            MessageRole::User,
            "Hello",
            "2024-01-01T00:00:00Z",
        ));
        let mut event = make_event_entry(
            "uuid-ev-parent",
            "attachment",
            "2024-01-01T00:00:01Z",
        );
        event.parent_uuid = Some("uuid-u1".to_string());
        convo.add_entry(event);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        // Event step should use its own parent_uuid
        assert_eq!(
            path.steps[1].step.parents,
            vec!["uuid-u1".to_string()]
        );
    }

    #[test]
    fn test_derive_path_event_with_tool_use_result() {
        let mut convo = Conversation::new("test-session-12345678".to_string());
        let mut event = make_event_entry(
            "uuid-ev-tur",
            "attachment",
            "2024-01-01T00:00:00Z",
        );
        event.tool_use_result = Some(serde_json::json!({
            "tool_use_id": "tu-123",
            "content": "hook output"
        }));
        convo.add_entry(event);

        let config = DeriveConfig::default();
        let path = derive_path(&convo, &config);

        let convo_key = format!("agent://claude/{}", convo.session_id);
        let extra = &path.steps[0].change[&convo_key]
            .structural
            .as_ref()
            .unwrap()
            .extra;

        assert_eq!(extra["tool_use_result"]["tool_use_id"], "tu-123");
        assert_eq!(extra["tool_use_result"]["content"], "hook output");
    }
}
