//! OpenClaw session schema (format version 3).
//!
//! OpenClaw stores each session as JSONL: the first line is a
//! [`SessionHeader`]; every subsequent line is an [`Entry`], forming a tree
//! via `id` / `parentId`. The visible head is moved by `leaf` entries.
//!
//! All structs carry an `extra` catch-all to preserve unknown fields for
//! forward compatibility. See `docs/agents/formats/openclaw/` for the
//! field-level reference.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The only session-format version this crate understands.
pub const SUPPORTED_VERSION: u32 = 3;

/// Shared fields on every non-header entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntryBase {
    /// Entry id, unique within the file (an 8-char UUIDv7 prefix).
    pub id: String,
    /// Parent entry id; `None` for a root entry.
    #[serde(rename = "parentId", default)]
    pub parent_id: Option<String>,
    /// Entry time (ISO-8601 string).
    pub timestamp: String,
    /// `"side"` marks a row that advances the raw cursor without selecting a
    /// model-visible branch.
    #[serde(
        rename = "appendMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub append_mode: Option<String>,
}

/// OpenClaw session file header (first line of a `.jsonl` file).
///
/// ```json
/// {"type":"session","version":3,"id":"...","timestamp":"...","cwd":"...","parentSession":"..."}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionHeader {
    /// Format version. The reader hard-rejects anything but `3`.
    #[serde(default)]
    pub version: u32,
    /// Session id (a UUID); matches the transcript filename stem.
    pub id: String,
    /// Session creation time (ISO-8601 string).
    pub timestamp: String,
    /// Working directory the session ran in.
    pub cwd: String,
    /// Path to a parent session file when this session was forked.
    #[serde(
        rename = "parentSession",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_session: Option<String>,
    /// Forward-compat catch-all.
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A single entry in an OpenClaw session JSONL.
///
/// Tagged by the `type` discriminant. The `session` variant matches the file
/// header; every other variant carries an [`EntryBase`] (id / parentId /
/// timestamp) flattened into the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    /// The header line (see [`SessionHeader`]).
    Session(SessionHeader),
    /// A conversational turn.
    Message {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// The role-tagged message payload.
        message: AgentMessage,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A model / provider switch.
    ModelChange {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// Provider id (e.g. `anthropic`).
        provider: String,
        /// Model id.
        #[serde(rename = "modelId")]
        model_id: String,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A reasoning-budget toggle.
    ThinkingLevelChange {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// The new thinking level.
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A context-compaction boundary.
    Compaction {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// Markdown summary of the dropped history.
        summary: String,
        /// Entries before this id are represented by `summary`.
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        /// Estimated context tokens before compaction.
        #[serde(rename = "tokensBefore")]
        tokens_before: u64,
        /// `{ readFiles, modifiedFiles }`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        /// True if produced by an app hook.
        #[serde(rename = "fromHook", default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A summary of an abandoned branch.
    BranchSummary {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// Entry id of the abandoned branch's source (`"root"` when null).
        #[serde(rename = "fromId")]
        from_id: String,
        /// Markdown summary of the abandoned branch.
        summary: String,
        /// `{ readFiles, modifiedFiles }`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        /// True if produced by an app hook.
        #[serde(rename = "fromHook", default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A harness/app marker not replayed into model context.
    Custom {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// Namespacing type.
        #[serde(rename = "customType")]
        custom_type: String,
        /// Opaque payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Harness/app content that IS replayable into context.
    CustomMessage {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// Namespacing type.
        #[serde(rename = "customType")]
        custom_type: String,
        /// The replayable content.
        content: MessageContent,
        /// UI visibility.
        display: bool,
        /// Opaque structured payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A display label for a target entry (last write wins).
    Label {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// The entry this label applies to.
        #[serde(rename = "targetId", default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// The label text (`None`/empty clears).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// The session name/title (last write wins).
    SessionInfo {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// The session name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Moves the visible-head pointer.
    Leaf {
        /// id / parentId / timestamp.
        #[serde(flatten)]
        base: EntryBase,
        /// The entry the branch currently points at (the visible head).
        #[serde(rename = "targetId", default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Overrides the raw parent for the next append.
        #[serde(
            rename = "appendParentId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        append_parent_id: Option<String>,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

impl Entry {
    /// The `EntryBase` (id / parentId / timestamp) for non-header entries.
    pub fn base(&self) -> Option<&EntryBase> {
        match self {
            Entry::Session(_) => None,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. }
            | Entry::SessionInfo { base, .. }
            | Entry::Leaf { base, .. } => Some(base),
        }
    }
}

/// Content field for user / custom-role messages: either a bare string or a
/// list of content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// A bare string.
    Text(String),
    /// A list of content blocks.
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    /// Collapse to a single text string (joining text blocks, ignoring others).
    pub fn text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// One element of a message's `content` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    /// Visible text.
    Text {
        /// The text.
        text: String,
        /// Forward-compat catch-all (e.g. `textSignature`).
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Inline image.
    Image {
        /// Base64 image data.
        data: String,
        /// MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Model reasoning.
    Thinking {
        /// The reasoning text.
        thinking: String,
        /// Forward-compat catch-all (e.g. `thinkingSignature`, `redacted`).
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// A tool invocation.
    ToolCall {
        /// Correlates with the later `toolResult.toolCallId`.
        id: String,
        /// Tool name.
        name: String,
        /// Free-form JSON args.
        arguments: serde_json::Value,
        /// Forward-compat catch-all (e.g. `thoughtSignature`, `executionMode`).
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

/// Restricted content block set for `toolResult` messages (text or image only).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolResultContent {
    /// Text result.
    Text {
        /// The text.
        text: String,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Image result.
    Image {
        /// Base64 image data.
        data: String,
        /// MIME type.
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

impl ToolResultContent {
    /// The text of this block, if it is a text block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolResultContent::Text { text, .. } => Some(text),
            ToolResultContent::Image { .. } => None,
        }
    }
}

/// Assistant stop reason. Unknown values round-trip through [`StopReason::Other`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", untagged)]
pub enum StopReason {
    /// A reason OpenClaw enumerates.
    Known(KnownStopReason),
    /// An unrecognized reason string.
    Other(String),
}

/// Enumerated stop reasons defined by OpenClaw.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum KnownStopReason {
    /// Normal completion.
    Stop,
    /// Hit the output length limit.
    Length,
    /// Ended on a tool call.
    ToolUse,
    /// Errored.
    Error,
    /// Aborted.
    Aborted,
}

/// A message inside a `message` entry. Role-tagged union.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum AgentMessage {
    /// Human / channel input.
    User {
        /// String or block content.
        content: MessageContent,
        /// Epoch-ms timestamp.
        timestamp: u64,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Model output.
    Assistant {
        /// Block content (text / thinking / toolCall).
        content: Vec<ContentBlock>,
        /// API id (e.g. `anthropic-messages`).
        api: String,
        /// Provider id.
        provider: String,
        /// Requested model.
        model: String,
        /// Concrete served model when it differs.
        #[serde(
            rename = "responseModel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        response_model: Option<String>,
        /// Token usage for this call.
        usage: Usage,
        /// Why the turn ended.
        #[serde(rename = "stopReason")]
        stop_reason: StopReason,
        /// Error text when the turn failed.
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        /// Epoch-ms timestamp.
        timestamp: u64,
        /// Forward-compat catch-all (errorCode/Type/Body, diagnostics, …).
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Result of a prior `toolCall`.
    ToolResult {
        /// Links back to `toolCall.id`.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Text / image result content.
        content: Vec<ToolResultContent>,
        /// Opaque structured payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        /// Error flag (error text rides in `content`).
        #[serde(rename = "isError")]
        is_error: bool,
        /// Epoch-ms timestamp.
        timestamp: u64,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Harness shell execution.
    BashExecution {
        /// The command.
        command: String,
        /// Captured output.
        output: String,
        /// Exit code.
        #[serde(rename = "exitCode", default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        /// Epoch-ms timestamp.
        timestamp: u64,
        /// Forward-compat catch-all.
        #[serde(default, flatten)]
        extra: HashMap<String, serde_json::Value>,
    },
}

impl AgentMessage {
    /// The visible text of this message.
    pub fn text(&self) -> String {
        match self {
            AgentMessage::User { content, .. } => content.text(),
            AgentMessage::Assistant { content, .. } => content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(ToolResultContent::as_text)
                .collect::<Vec<_>>()
                .join("\n"),
            AgentMessage::BashExecution { output, .. } => output.clone(),
        }
    }

    /// The concatenated reasoning text, if any.
    pub fn thinking(&self) -> Option<String> {
        if let AgentMessage::Assistant { content, .. } = self {
            let t = content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !t.is_empty() {
                return Some(t);
            }
        }
        None
    }

    /// The tool-call blocks `(id, name, arguments)` in an assistant message.
    pub fn tool_calls(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        match self {
            AgentMessage::Assistant { content, .. } => content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall {
                        id, name, arguments, ..
                    } => Some((id.as_str(), name.as_str(), arguments)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The model id, for assistant messages.
    pub fn model(&self) -> Option<&str> {
        match self {
            AgentMessage::Assistant { model, .. } => Some(model),
            _ => None,
        }
    }
}

/// Per-message token accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    /// Prompt tokens.
    #[serde(default)]
    pub input: u64,
    /// Completion tokens.
    #[serde(default)]
    pub output: u64,
    /// Prompt-cache read tokens.
    #[serde(default, rename = "cacheRead")]
    pub cache_read: u64,
    /// Prompt-cache write tokens.
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: u64,
    /// Headline total.
    #[serde(default, rename = "totalTokens")]
    pub total_tokens: u64,
    /// Per-class cost breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostBreakdown>,
}

/// Dollar cost breakdown accompanying [`Usage`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostBreakdown {
    /// Input cost.
    #[serde(default)]
    pub input: f64,
    /// Output cost.
    #[serde(default)]
    pub output: f64,
    /// Cache-read cost.
    #[serde(default, rename = "cacheRead")]
    pub cache_read: f64,
    /// Cache-write cost.
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: f64,
    /// Total cost.
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
    fn header_roundtrip() {
        let j = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-06-30T12:00:00Z","cwd":"/p"}"#;
        let e: Entry = serde_json::from_str(j).unwrap();
        let h = match e {
            Entry::Session(h) => h,
            _ => panic!("expected session header"),
        };
        assert_eq!(h.version, 3);
        assert_eq!(h.id, "s1");
        assert!(h.parent_session.is_none());
        let back: SessionHeader = roundtrip(&h);
        assert_eq!(back.cwd, "/p");
    }

    #[test]
    fn user_content_string_or_array() {
        let s: AgentMessage =
            serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":1}"#).unwrap();
        assert_eq!(s.text(), "hi");
        let a: AgentMessage = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1}"#,
        )
        .unwrap();
        assert_eq!(a.text(), "hi");
    }

    #[test]
    fn assistant_message_with_blocks_roundtrip() {
        let j = json!({
            "type":"message","id":"e1","parentId":"e0","timestamp":"2026-06-30T12:00:05Z",
            "message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"hm","thinkingSignature":"sig"},
                {"type":"text","text":"hi"},
                {"type":"toolCall","id":"c1","name":"read_file","arguments":{"path":"x"}}],
              "api":"anthropic-messages","provider":"anthropic","model":"claude-x",
              "usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3},
              "stopReason":"toolUse","timestamp":1751284805000u64}
        });
        let e: Entry = serde_json::from_value(j).unwrap();
        match &e {
            Entry::Message {
                message: AgentMessage::Assistant { content, usage, .. },
                ..
            } => {
                assert_eq!(content.len(), 3);
                assert_eq!(usage.total_tokens, 3);
            }
            _ => panic!("wrong variant"),
        }
        let m = match &e {
            Entry::Message { message, .. } => message,
            _ => unreachable!(),
        };
        assert_eq!(m.text(), "hi");
        assert_eq!(m.thinking().as_deref(), Some("hm"));
        assert_eq!(m.tool_calls().len(), 1);
        assert_eq!(m.tool_calls()[0].1, "read_file");
        // thinkingSignature survives via `extra`
        let back: Entry = roundtrip(&e);
        assert!(matches!(back, Entry::Message { .. }));
    }

    #[test]
    fn tool_result_roundtrip() {
        let j = json!({
            "type":"message","id":"e2","parentId":"e1","timestamp":"t",
            "message":{"role":"toolResult","toolCallId":"c1","toolName":"read_file",
                "content":[{"type":"text","text":"file body"}],"isError":false,"timestamp":1}
        });
        let e: Entry = serde_json::from_value(j).unwrap();
        match &e {
            Entry::Message {
                message:
                    AgentMessage::ToolResult {
                        tool_call_id,
                        is_error,
                        ..
                    },
                ..
            } => {
                assert_eq!(tool_call_id, "c1");
                assert!(!is_error);
            }
            _ => panic!("wrong variant"),
        }
        assert!(matches!(roundtrip(&e), Entry::Message { .. }));
    }

    #[test]
    fn leaf_and_compaction_roundtrip() {
        let leaf: Entry = serde_json::from_str(
            r#"{"type":"leaf","id":"l1","parentId":"e2","timestamp":"t","targetId":"e2"}"#,
        )
        .unwrap();
        match &leaf {
            Entry::Leaf { target_id, .. } => assert_eq!(target_id.as_deref(), Some("e2")),
            _ => panic!("wrong variant"),
        }
        assert!(matches!(roundtrip(&leaf), Entry::Leaf { .. }));

        let comp: Entry = serde_json::from_value(json!({
            "type":"compaction","id":"k1","parentId":"e2","timestamp":"t",
            "summary":"## Goal","firstKeptEntryId":"e1","tokensBefore":5400u64,
            "details":{"readFiles":["x"],"modifiedFiles":[]},"fromHook":false
        }))
        .unwrap();
        match &comp {
            Entry::Compaction {
                first_kept_entry_id,
                tokens_before,
                ..
            } => {
                assert_eq!(first_kept_entry_id, "e1");
                assert_eq!(*tokens_before, 5400);
            }
            _ => panic!("wrong variant"),
        }
        assert!(matches!(roundtrip(&comp), Entry::Compaction { .. }));
    }

    #[test]
    fn session_info_and_label_roundtrip() {
        let si: Entry = serde_json::from_str(
            r#"{"type":"session_info","id":"s","parentId":null,"timestamp":"t","name":"My chat"}"#,
        )
        .unwrap();
        match &si {
            Entry::SessionInfo { name, .. } => assert_eq!(name.as_deref(), Some("My chat")),
            _ => panic!("wrong variant"),
        }
        let lab: Entry = serde_json::from_str(
            r#"{"type":"label","id":"l","parentId":"e1","timestamp":"t","targetId":"e1","label":"pin"}"#,
        )
        .unwrap();
        match &lab {
            Entry::Label { target_id, label, .. } => {
                assert_eq!(target_id.as_deref(), Some("e1"));
                assert_eq!(label.as_deref(), Some("pin"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn append_mode_side_roundtrips() {
        let e: Entry = serde_json::from_str(
            r#"{"type":"message","id":"e","parentId":null,"timestamp":"t","appendMode":"side",
                "message":{"role":"user","content":"x","timestamp":1}}"#,
        )
        .unwrap();
        assert_eq!(e.base().unwrap().append_mode.as_deref(), Some("side"));
    }

    #[test]
    fn stop_reason_unknown_roundtrips() {
        let sr: StopReason = serde_json::from_str("\"weird\"").unwrap();
        assert_eq!(sr, StopReason::Other("weird".into()));
        let known: StopReason = serde_json::from_str("\"toolUse\"").unwrap();
        assert_eq!(known, StopReason::Known(KnownStopReason::ToolUse));
    }
}
