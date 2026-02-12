//! Derive Toolpath documents from Claude conversation logs.
//!
//! The conversation itself is treated as an artifact under change. Each turn
//! appends to `claude://<session-id>` via a `conversation.append` structural
//! operation. File mutations from tool use (Write, Edit) appear as sibling
//! artifacts in the same step's `change` map.

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

/// Derive a single Toolpath Path from a Claude conversation.
///
/// The conversation is modeled as an artifact at `claude://<session-id>`.
/// Each user or assistant turn produces a step whose `change` map contains
/// a `conversation.append` structural change on that artifact, plus any
/// file-level artifacts touched by tool use.
pub fn derive_path(conversation: &Conversation, config: &DeriveConfig) -> Path {
    let session_short = safe_prefix(&conversation.session_id, 8);
    let convo_artifact = format!("claude://{}", conversation.session_id);

    let mut steps = Vec::new();
    let mut last_step_id: Option<String> = None;
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();

    for entry in &conversation.entries {
        if entry.uuid.is_empty() {
            continue;
        }

        let message = match &entry.message {
            Some(m) => m,
            None => continue,
        };

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
            MessageRole::System => continue,
        };

        // Collect conversation text and file changes from this turn
        let mut file_changes: HashMap<String, ArtifactChange> = HashMap::new();
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_uses: Vec<String> = Vec::new();

        match &message.content {
            Some(MessageContent::Parts(parts)) => {
                for part in parts {
                    match part {
                        ContentPart::Text { text } => {
                            if !text.trim().is_empty() {
                                text_parts.push(text.clone());
                            }
                        }
                        ContentPart::Thinking { thinking, .. } => {
                            if config.include_thinking && !thinking.trim().is_empty() {
                                text_parts.push(format!("[thinking] {}", thinking));
                            }
                        }
                        ContentPart::ToolUse { name, input, .. } => {
                            tool_uses.push(name.clone());
                            if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str())
                            {
                                match name.as_str() {
                                    "Write" | "Edit" => {
                                        file_changes.insert(
                                            file_path.to_string(),
                                            ArtifactChange {
                                                raw: None,
                                                structural: None,
                                            },
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(MessageContent::Text(text)) => {
                if !text.trim().is_empty() {
                    text_parts.push(text.clone());
                }
            }
            None => {}
        }

        // Skip entries with no conversation content and no file changes
        if text_parts.is_empty() && tool_uses.is_empty() && file_changes.is_empty() {
            continue;
        }

        // Build the conversation artifact change
        let mut convo_extra = HashMap::new();
        convo_extra.insert("role".to_string(), json!(role_str));
        if !text_parts.is_empty() {
            let combined = text_parts.join("\n\n");
            convo_extra.insert("text".to_string(), json!(truncate(&combined, 2000)));
        }
        if !tool_uses.is_empty() {
            convo_extra.insert("tool_uses".to_string(), json!(tool_uses.clone()));
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
        changes.extend(file_changes);

        // Build step — no meta.intent; the conversation content already
        // lives in the structural change and adding it again is redundant.
        let step_id = format!("step-{}", safe_prefix(&entry.uuid, 8));
        let parents = if entry.is_sidechain {
            entry
                .parent_uuid
                .as_ref()
                .map(|p| vec![format!("step-{}", safe_prefix(p, 8))])
                .unwrap_or_default()
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
            last_step_id = Some(step_id);
        }
        steps.push(step);
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
    use crate::types::{ContentPart, ConversationEntry, Message, MessageContent};

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
        let s = "café résumé naïve";
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
        assert_eq!(safe_prefix("日本語テスト", 3), "日本語");
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

        // Second step should have first as parent
        assert!(path.steps[1].step.parents.contains(&path.steps[0].step.id));
        // Third step should have second as parent
        assert!(path.steps[2].step.parents.contains(&path.steps[1].step.id));
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

        // Each step should have the conversation artifact
        let convo_key = format!("claude://{}", convo.session_id);
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
        // Empty text, no tool uses, no file changes → should be skipped
        entry.message.as_mut().unwrap().content = Some(MessageContent::Text("   ".to_string()));

        let convo = make_conversation(vec![entry]);
        let config = DeriveConfig::default();

        let path = derive_path(&convo, &config);
        assert!(path.steps.is_empty());
    }

    #[test]
    fn test_derive_path_skips_system_messages() {
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
        // System message should be skipped
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.steps[0].step.actor, "human:user");
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

        assert_eq!(path.steps.len(), 1);
        // Should have both the conversation artifact and the file artifact
        assert!(path.steps[0].change.contains_key("/tmp/test.rs"));
        let convo_key = format!("claude://{}", convo.session_id);
        assert!(path.steps[0].change.contains_key(&convo_key));
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
        // Sidechain step should reference e1 as parent, not e2
        let sidechain_step = &path.steps[2];
        let expected_parent = format!("step-{}", safe_prefix("uuid-main-11", 8));
        assert!(sidechain_step.step.parents.contains(&expected_parent));
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

        // Head should point to the last step
        assert_eq!(path.path.head, path.steps.last().unwrap().step.id);
    }
}
