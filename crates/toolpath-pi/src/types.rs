//! Pi session schema (format version 3).
//!
//! Pi is a terminal coding agent that stores sessions as JSONL files. The first
//! line is a [`SessionHeader`]; every subsequent line is an [`Entry`], forming
//! a tree via `id` / `parentId`.
//!
//! All structs carry an `extra` catch-all to preserve unknown fields for
//! forward compatibility.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Shared fields on every non-header entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntryBase {
    pub id: String,
    #[serde(rename = "parentId", default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// Pi session file header (first line of a `.jsonl` file).
///
/// The header JSON looks like:
/// ```json
/// {"type":"session","version":3,"id":"...","timestamp":"...","cwd":"...","parentSession":"..."}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionHeader {
    #[serde(default)]
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(
        rename = "parentSession",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_session: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A single entry in a Pi session JSONL.
///
/// Tagged by the `type` discriminant. The `session` variant matches the file
/// header; every other variant carries an [`EntryBase`] (id / parentId /
/// timestamp) flattened into the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Session(SessionHeader),
    Message {
        #[serde(flatten)]
        base: EntryBase,
        message: AgentMessage,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    ModelChange {
        #[serde(flatten)]
        base: EntryBase,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    ThinkingLevelChange {
        #[serde(flatten)]
        base: EntryBase,
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Compaction {
        #[serde(flatten)]
        base: EntryBase,
        summary: String,
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(rename = "fromHook", default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    BranchSummary {
        #[serde(flatten)]
        base: EntryBase,
        #[serde(rename = "fromId")]
        from_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(rename = "fromHook", default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Custom {
        #[serde(flatten)]
        base: EntryBase,
        #[serde(rename = "customType")]
        custom_type: String,
        data: serde_json::Map<String, serde_json::Value>,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    CustomMessage {
        #[serde(flatten)]
        base: EntryBase,
        #[serde(rename = "customType")]
        custom_type: String,
        content: MessageContent,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Label {
        #[serde(flatten)]
        base: EntryBase,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

/// Content field for user / custom-role messages: either a bare string or a
/// list of content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// A tool-use request inside an assistant content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// One element of a message's `content` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Thinking {
        thinking: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

/// Restricted content block set for `toolResult` messages. Per the Pi schema,
/// tool results may only carry text or image content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolResultContent {
    Text {
        text: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

/// Assistant stop reason. Unknown values round-trip through [`StopReason::Other`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", untagged)]
pub enum StopReason {
    Known(KnownStopReason),
    Other(String),
}

/// Enumerated stop reasons defined by Pi. Kept inside [`StopReason`] so that
/// unknown strings (e.g. from future Pi versions) still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum KnownStopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

/// A message inside a `message` entry. Role-tagged union.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum AgentMessage {
    User {
        content: MessageContent,
        timestamp: u64,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Assistant {
        content: Vec<ContentBlock>,
        api: String,
        provider: String,
        model: String,
        usage: Usage,
        #[serde(rename = "stopReason")]
        stop_reason: StopReason,
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        timestamp: u64,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<ToolResultContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(rename = "isError")]
        is_error: bool,
        timestamp: u64,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    BashExecution {
        command: String,
        output: String,
        #[serde(rename = "exitCode")]
        exit_code: Option<i64>,
        cancelled: bool,
        truncated: bool,
        #[serde(
            rename = "fullOutputPath",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        full_output_path: Option<String>,
        #[serde(
            rename = "excludeFromContext",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        exclude_from_context: Option<bool>,
        timestamp: u64,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        content: MessageContent,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        timestamp: u64,
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    BranchSummary {
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    CompactionSummary {
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

/// Per-turn token accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default, rename = "cacheRead")]
    pub cache_read: u64,
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: u64,
    #[serde(default, rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(default)]
    pub cost: CostBreakdown,
}

/// Dollar cost breakdown accompanying [`Usage`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostBreakdown {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: f64,
    #[serde(default)]
    pub total: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
        let s = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&s).expect("deserialize")
    }

    #[test]
    fn test_session_header_roundtrip() {
        let header = SessionHeader {
            version: 3,
            id: "abc123".into(),
            timestamp: "2026-04-16T00:00:00.000Z".into(),
            cwd: "/tmp/project".into(),
            parent_session: Some("/tmp/parent.jsonl".into()),
            extra: HashMap::new(),
        };
        let back: SessionHeader = roundtrip(&header);
        assert_eq!(back.version, 3);
        assert_eq!(back.id, "abc123");
        assert_eq!(back.cwd, "/tmp/project");
        assert_eq!(back.parent_session.as_deref(), Some("/tmp/parent.jsonl"));
    }

    #[test]
    fn test_session_header_without_parent() {
        let header = SessionHeader {
            version: 3,
            id: "x".into(),
            timestamp: "t".into(),
            cwd: "/".into(),
            parent_session: None,
            extra: HashMap::new(),
        };
        let s = serde_json::to_string(&header).unwrap();
        assert!(!s.contains("parentSession"));
        let back: SessionHeader = serde_json::from_str(&s).unwrap();
        assert!(back.parent_session.is_none());
    }

    #[test]
    fn test_entry_message_user_string_content() {
        let raw = json!({
            "type": "message",
            "id": "aa11bb22",
            "parentId": null,
            "timestamp": "2026-04-16T00:00:00.000Z",
            "message": {
                "role": "user",
                "content": "hello",
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw.clone()).unwrap();
        match &entry {
            Entry::Message { base, message, .. } => {
                assert_eq!(base.id, "aa11bb22");
                match message {
                    AgentMessage::User {
                        content, timestamp, ..
                    } => {
                        assert_eq!(*timestamp, 1_700_000_000_000);
                        match content {
                            MessageContent::Text(s) => assert_eq!(s, "hello"),
                            _ => panic!("wrong content variant"),
                        }
                    }
                    _ => panic!("wrong role"),
                }
            }
            _ => panic!("wrong entry type"),
        }
        let back: Entry = roundtrip(&entry);
        assert!(matches!(back, Entry::Message { .. }));
    }

    #[test]
    fn test_entry_message_user_blocks_content() {
        let raw = json!({
            "type": "message",
            "id": "aa11bb23",
            "parentId": "aa11bb22",
            "timestamp": "2026-04-16T00:00:00.000Z",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match entry {
            Entry::Message {
                message: AgentMessage::User { content, .. },
                ..
            } => match content {
                MessageContent::Blocks(blocks) => {
                    assert_eq!(blocks.len(), 1);
                    assert!(matches!(&blocks[0], ContentBlock::Text { text, .. } if text == "hi"));
                }
                _ => panic!("expected blocks"),
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_entry_message_assistant() {
        let raw = json!({
            "type": "message",
            "id": "bb00",
            "parentId": "aa00",
            "timestamp": "t",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "ok"},
                    {"type": "toolCall", "id": "tc1", "name": "read", "arguments": {"path": "/x"}}
                ],
                "api": "anthropic",
                "provider": "anthropic",
                "model": "claude-opus",
                "usage": {
                    "input": 10, "output": 20, "cacheRead": 1, "cacheWrite": 2,
                    "totalTokens": 33,
                    "cost": {"input": 0.001, "output": 0.002, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.003}
                },
                "stopReason": "toolUse",
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Message {
                message:
                    AgentMessage::Assistant {
                        content,
                        usage,
                        stop_reason,
                        ..
                    },
                ..
            } => {
                assert_eq!(content.len(), 2);
                assert_eq!(usage.total_tokens, 33);
                assert_eq!(*stop_reason, StopReason::Known(KnownStopReason::ToolUse));
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_message_tool_result() {
        let raw = json!({
            "type": "message",
            "id": "cc00",
            "parentId": "bb00",
            "timestamp": "t",
            "message": {
                "role": "toolResult",
                "toolCallId": "tc1",
                "toolName": "read",
                "content": [{"type": "text", "text": "file contents"}],
                "isError": false,
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Message {
                message:
                    AgentMessage::ToolResult {
                        tool_call_id,
                        tool_name,
                        content,
                        is_error,
                        ..
                    },
                ..
            } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(tool_name, "read");
                assert_eq!(content.len(), 1);
                assert!(!is_error);
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_message_bash_execution() {
        let raw = json!({
            "type": "message",
            "id": "dd00",
            "parentId": null,
            "timestamp": "t",
            "message": {
                "role": "bashExecution",
                "command": "ls",
                "output": "a\nb\n",
                "exitCode": 0,
                "cancelled": false,
                "truncated": false,
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Message {
                message:
                    AgentMessage::BashExecution {
                        command,
                        exit_code,
                        cancelled,
                        ..
                    },
                ..
            } => {
                assert_eq!(command, "ls");
                assert_eq!(*exit_code, Some(0));
                assert!(!cancelled);
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_message_custom() {
        let raw = json!({
            "type": "message",
            "id": "ee00",
            "parentId": null,
            "timestamp": "t",
            "message": {
                "role": "custom",
                "customType": "note",
                "content": "a note",
                "display": true,
                "timestamp": 1_700_000_000_000u64
            }
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Message {
                message:
                    AgentMessage::Custom {
                        custom_type,
                        display,
                        ..
                    },
                ..
            } => {
                assert_eq!(custom_type, "note");
                assert!(*display);
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_model_change() {
        let raw = json!({
            "type": "model_change",
            "id": "ff00",
            "parentId": null,
            "timestamp": "t",
            "provider": "anthropic",
            "modelId": "claude-opus-4-7"
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::ModelChange {
                provider, model_id, ..
            } => {
                assert_eq!(provider, "anthropic");
                assert_eq!(model_id, "claude-opus-4-7");
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_thinking_level_change() {
        let raw = json!({
            "type": "thinking_level_change",
            "id": "aa00",
            "parentId": null,
            "timestamp": "t",
            "thinkingLevel": "high"
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::ThinkingLevelChange { thinking_level, .. } => {
                assert_eq!(thinking_level, "high");
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_compaction() {
        let raw = json!({
            "type": "compaction",
            "id": "c000",
            "parentId": null,
            "timestamp": "t",
            "summary": "sum",
            "firstKeptEntryId": "bb00",
            "tokensBefore": 100000,
            "fromHook": false
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Compaction {
                summary,
                first_kept_entry_id,
                tokens_before,
                from_hook,
                ..
            } => {
                assert_eq!(summary, "sum");
                assert_eq!(first_kept_entry_id, "bb00");
                assert_eq!(*tokens_before, 100000);
                assert_eq!(*from_hook, Some(false));
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_branch_summary() {
        let raw = json!({
            "type": "branch_summary",
            "id": "bs00",
            "parentId": null,
            "timestamp": "t",
            "fromId": "aa00",
            "summary": "branched off"
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::BranchSummary {
                from_id, summary, ..
            } => {
                assert_eq!(from_id, "aa00");
                assert_eq!(summary, "branched off");
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_custom() {
        let raw = json!({
            "type": "custom",
            "id": "cu00",
            "parentId": null,
            "timestamp": "t",
            "customType": "telemetry",
            "data": {"k": "v"}
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::Custom {
                custom_type, data, ..
            } => {
                assert_eq!(custom_type, "telemetry");
                assert_eq!(data.get("k").and_then(|v| v.as_str()), Some("v"));
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_entry_custom_message() {
        let raw = json!({
            "type": "custom_message",
            "id": "cm00",
            "parentId": null,
            "timestamp": "t",
            "customType": "hint",
            "content": "some hint",
            "display": true
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        match &entry {
            Entry::CustomMessage {
                custom_type,
                display,
                content,
                ..
            } => {
                assert_eq!(custom_type, "hint");
                assert!(*display);
                assert!(matches!(content, MessageContent::Text(s) if s == "some hint"));
            }
            _ => panic!("wrong variant"),
        }
        let _: Entry = roundtrip(&entry);
    }

    #[test]
    fn test_content_block_text() {
        let v: ContentBlock =
            serde_json::from_value(json!({"type": "text", "text": "hi"})).unwrap();
        assert!(matches!(&v, ContentBlock::Text { text, .. } if text == "hi"));
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"type\":\"text\""));
    }

    #[test]
    fn test_content_block_image() {
        let v: ContentBlock = serde_json::from_value(json!({
            "type": "image",
            "data": "ZGF0YQ==",
            "mimeType": "image/png"
        }))
        .unwrap();
        match &v {
            ContentBlock::Image {
                data, mime_type, ..
            } => {
                assert_eq!(data, "ZGF0YQ==");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("wrong variant"),
        }
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("mimeType"));
    }

    #[test]
    fn test_content_block_thinking() {
        let v: ContentBlock =
            serde_json::from_value(json!({"type": "thinking", "thinking": "hmm"})).unwrap();
        assert!(matches!(&v, ContentBlock::Thinking { thinking, .. } if thinking == "hmm"));
    }

    #[test]
    fn test_content_block_tool_call() {
        let v: ContentBlock = serde_json::from_value(json!({
            "type": "toolCall",
            "id": "tc1",
            "name": "read",
            "arguments": {"path": "/x"}
        }))
        .unwrap();
        match &v {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(id, "tc1");
                assert_eq!(name, "read");
                assert_eq!(arguments.get("path").and_then(|p| p.as_str()), Some("/x"));
            }
            _ => panic!("wrong variant"),
        }
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"type\":\"toolCall\""));
    }

    #[test]
    fn test_stop_reason_variants() {
        let cases = [
            ("\"stop\"", StopReason::Known(KnownStopReason::Stop)),
            ("\"length\"", StopReason::Known(KnownStopReason::Length)),
            ("\"toolUse\"", StopReason::Known(KnownStopReason::ToolUse)),
            ("\"error\"", StopReason::Known(KnownStopReason::Error)),
            ("\"aborted\"", StopReason::Known(KnownStopReason::Aborted)),
        ];
        for (s, expected) in cases {
            let parsed: StopReason = serde_json::from_str(s).unwrap();
            assert_eq!(parsed, expected);
            let back = serde_json::to_string(&parsed).unwrap();
            assert_eq!(back, s);
        }
        let other: StopReason = serde_json::from_str("\"someFutureReason\"").unwrap();
        assert_eq!(other, StopReason::Other("someFutureReason".into()));
        let back = serde_json::to_string(&other).unwrap();
        assert_eq!(back, "\"someFutureReason\"");
    }

    #[test]
    fn test_usage_roundtrip() {
        let u = Usage {
            input: 100,
            output: 200,
            cache_read: 5,
            cache_write: 6,
            total_tokens: 311,
            cost: CostBreakdown {
                input: 0.1,
                output: 0.2,
                cache_read: 0.01,
                cache_write: 0.02,
                total: 0.33,
            },
        };
        let s = serde_json::to_string(&u).unwrap();
        assert!(s.contains("cacheRead"));
        assert!(s.contains("cacheWrite"));
        assert!(s.contains("totalTokens"));
        let back: Usage = serde_json::from_str(&s).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn test_extra_fields_preserved() {
        let raw = json!({
            "type": "model_change",
            "id": "ff00",
            "parentId": null,
            "timestamp": "t",
            "provider": "anthropic",
            "modelId": "claude-opus",
            "futureField": {"nested": 42}
        });
        let entry: Entry = serde_json::from_value(raw).unwrap();
        let back = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            back.get("futureField")
                .and_then(|v| v.get("nested"))
                .and_then(|n| n.as_i64()),
            Some(42)
        );
    }

    #[test]
    fn test_parse_real_fixture_line_by_line() {
        let jsonl = r#"{"type":"session","version":3,"id":"sess-1","timestamp":"2026-04-16T00:00:00.000Z","cwd":"/tmp/proj"}
{"type":"message","id":"aa000001","parentId":null,"timestamp":"2026-04-16T00:00:01.000Z","message":{"role":"user","content":"hello","timestamp":1700000000000}}
{"type":"message","id":"aa000002","parentId":"aa000001","timestamp":"2026-04-16T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"},{"type":"toolCall","id":"tc1","name":"read","arguments":{"path":"/x"}}],"api":"anthropic","provider":"anthropic","model":"claude-opus","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"input":0.001,"output":0.0005,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0015}},"stopReason":"toolUse","timestamp":1700000001000}}
{"type":"message","id":"aa000003","parentId":"aa000002","timestamp":"2026-04-16T00:00:03.000Z","message":{"role":"toolResult","toolCallId":"tc1","toolName":"read","content":[{"type":"text","text":"file body"}],"isError":false,"timestamp":1700000002000}}"#;

        let mut entries: Vec<Entry> = Vec::new();
        for line in jsonl.lines() {
            let e: Entry = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("failed to parse line `{}`: {}", line, e);
            });
            entries.push(e);
        }
        assert_eq!(entries.len(), 4);
        assert!(matches!(entries[0], Entry::Session(_)));
        assert!(matches!(
            &entries[1],
            Entry::Message {
                message: AgentMessage::User { .. },
                ..
            }
        ));
        assert!(matches!(
            &entries[2],
            Entry::Message {
                message: AgentMessage::Assistant { .. },
                ..
            }
        ));
        assert!(matches!(
            &entries[3],
            Entry::Message {
                message: AgentMessage::ToolResult { .. },
                ..
            }
        ));
    }
}
