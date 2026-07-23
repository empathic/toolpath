//! On-disk schema for Codex CLI rollout JSONL files.
//!
//! Mirrors the upstream Rust types in
//! `openai/codex:codex-rs/protocol/src/{protocol,models}.rs` closely
//! enough for fidelity, but uses `Value` fallbacks on enum variants we
//! don't exhaustively enumerate so unknown future payloads survive
//! round-trip. Each top-level line is a `RolloutLine`:
//!
//! ```json
//! {"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{...}}
//! ```
//!
//! The `type` field discriminates at the outer level. `payload.type`
//! discriminates inside `response_item` and `event_msg` payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

// ── Rollout line (top-level wrapper) ────────────────────────────────

/// One JSONL line from a rollout file — a tagged payload with a
/// timestamp. The struct preserves unknown fields and unknown `type`
/// tags verbatim so round-trip re-serialization stays faithful.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutLine {
    pub timestamp: String,

    /// The outer discriminator: `session_meta`, `turn_context`,
    /// `response_item`, `event_msg`, `session_state`, `compacted`, or
    /// an unknown future value.
    #[serde(rename = "type")]
    pub kind: String,

    /// Variant-specific payload. Type-dispatched view via
    /// [`RolloutLine::item`].
    pub payload: Value,

    /// Forward-compat catch-all for unknown top-level fields.
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

impl RolloutLine {
    /// Interpret `kind` + `payload` into a typed view. Unknown variants
    /// become `RolloutItem::Unknown`, preserving the raw payload.
    pub fn item(&self) -> RolloutItem {
        match self.kind.as_str() {
            "session_meta" => match serde_json::from_value(self.payload.clone()) {
                Ok(v) => RolloutItem::SessionMeta(Box::new(v)),
                Err(_) => RolloutItem::Unknown {
                    kind: self.kind.clone(),
                    payload: self.payload.clone(),
                },
            },
            "turn_context" => match serde_json::from_value(self.payload.clone()) {
                Ok(v) => RolloutItem::TurnContext(Box::new(v)),
                Err(_) => RolloutItem::Unknown {
                    kind: self.kind.clone(),
                    payload: self.payload.clone(),
                },
            },
            "response_item" => RolloutItem::ResponseItem(ResponseItem::from_value(&self.payload)),
            "event_msg" => RolloutItem::EventMsg(EventMsg::from_value(&self.payload)),
            "session_state" => RolloutItem::SessionState(self.payload.clone()),
            "compacted" => RolloutItem::Compacted(self.payload.clone()),
            _ => RolloutItem::Unknown {
                kind: self.kind.clone(),
                payload: self.payload.clone(),
            },
        }
    }

    /// Parse the outer timestamp into a `DateTime<Utc>` if well-formed.
    pub fn parsed_timestamp(&self) -> Option<DateTime<Utc>> {
        self.timestamp.parse::<DateTime<Utc>>().ok()
    }
}

/// Typed view of a [`RolloutLine::payload`] by `kind`.
#[derive(Debug, Clone)]
pub enum RolloutItem {
    SessionMeta(Box<SessionMeta>),
    TurnContext(Box<TurnContext>),
    ResponseItem(ResponseItem),
    EventMsg(EventMsg),
    SessionState(Value),
    Compacted(Value),
    Unknown { kind: String, payload: Value },
}

// ── Session metadata ────────────────────────────────────────────────

/// First line of every rollout file; `session_meta` payload.
///
/// Matches `SessionMeta` (+ git denormalization) from
/// `codex-rs/protocol/src/protocol.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// UUIDv7 session id.
    pub id: String,

    /// ISO-8601 timestamp for session creation (distinct from the
    /// outer line timestamp, which is the write time).
    pub timestamp: String,

    /// Working directory at session creation.
    pub cwd: PathBuf,

    /// Who launched Codex (`codex-tui`, `codex-exec`, IDE plugins).
    pub originator: String,

    pub cli_version: String,

    /// Entry point: `cli`, `vscode`, etc.
    pub source: String,

    /// Parent session if this was forked (multi-agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,

    /// Embedded system prompt for the session. Typically large (~20 KB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<BaseInstructions>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<String>,

    /// Git state at session start. Populated when cwd is inside a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,

    /// Forward-compat catch-all.
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseInstructions {
    pub text: String,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

// ── Turn context ────────────────────────────────────────────────────

/// Per-turn context snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub turn_id: String,
    pub cwd: PathBuf,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<Value>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// `read-only` | `workspace-write` | `danger-full-access`
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_roots: Vec<PathBuf>,
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub exclude_tmpdir_env_var: bool,
    #[serde(default)]
    pub exclude_slash_tmp: bool,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

// ── Response items (model output) ───────────────────────────────────

/// Items emitted by the model — messages, reasoning, and tool calls.
///
/// Codex's upstream enum has more variants (`LocalShellCall`,
/// `WebSearchCall`, `ImageGenerationCall`, etc.). We strongly-type
/// the ones we see in the wild and leave the rest as raw `Value` via
/// [`ResponseItem::Other`]. This keeps the crate future-proof: a new
/// variant we don't handle still round-trips losslessly.
#[derive(Debug, Clone)]
pub enum ResponseItem {
    Message(Message),
    Reasoning(Reasoning),
    FunctionCall(FunctionCall),
    FunctionCallOutput(FunctionCallOutput),
    CustomToolCall(CustomToolCall),
    CustomToolCallOutput(CustomToolCallOutput),
    Other { kind: String, payload: Value },
}

impl ResponseItem {
    /// Dispatch on the inner `type` field.
    pub fn from_value(payload: &Value) -> Self {
        let kind = payload
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let attempt = match kind.as_str() {
            "message" => serde_json::from_value::<Message>(payload.clone())
                .ok()
                .map(ResponseItem::Message),
            "reasoning" => serde_json::from_value::<Reasoning>(payload.clone())
                .ok()
                .map(ResponseItem::Reasoning),
            "function_call" => serde_json::from_value::<FunctionCall>(payload.clone())
                .ok()
                .map(ResponseItem::FunctionCall),
            "function_call_output" => serde_json::from_value::<FunctionCallOutput>(payload.clone())
                .ok()
                .map(ResponseItem::FunctionCallOutput),
            "custom_tool_call" => serde_json::from_value::<CustomToolCall>(payload.clone())
                .ok()
                .map(ResponseItem::CustomToolCall),
            "custom_tool_call_output" => {
                serde_json::from_value::<CustomToolCallOutput>(payload.clone())
                    .ok()
                    .map(ResponseItem::CustomToolCallOutput)
            }
            _ => None,
        };
        attempt.unwrap_or(ResponseItem::Other {
            kind,
            payload: payload.clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// `"developer"`, `"user"`, or `"assistant"`.
    pub role: String,

    pub content: Vec<ContentPart>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,

    /// `"commentary"`, `"final"`, etc. on assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    InputText {
        text: String,
        #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
        extra: HashMap<String, Value>,
    },
    OutputText {
        text: String,
        #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
        extra: HashMap<String, Value>,
    },
    /// Forward-compat for new content types (images, etc.).
    #[serde(other)]
    Unknown,
}

impl ContentPart {
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentPart::InputText { text, .. } | ContentPart::OutputText { text, .. } => {
                Some(text)
            }
            ContentPart::Unknown => None,
        }
    }
}

impl Message {
    /// Flattened visible text of all parts, separated by newlines.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| p.text())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Public summary entries (often empty in production logs).
    #[serde(default)]
    pub summary: Vec<Value>,

    /// Public reasoning content (often null).
    #[serde(default)]
    pub content: Option<Value>,

    /// Opaque encrypted reasoning blob. Preserved verbatim for
    /// round-trip fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,

    /// Raw JSON string as written by the model. Intentionally not
    /// eagerly parsed — some models emit malformed JSON and we want
    /// byte-equivalent round-trip.
    pub arguments: String,

    pub call_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

impl FunctionCall {
    /// Try to parse `arguments` as JSON; returns `Value::Null` on failure.
    pub fn arguments_as_json(&self) -> Value {
        serde_json::from_str(&self.arguments).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCallOutput {
    pub call_id: String,

    /// Textual tool output (often a multi-line summary).
    pub output: String,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomToolCall {
    pub name: String,

    /// Free-form input string (e.g. V4A patch body for `apply_patch`).
    /// Not guaranteed to be JSON.
    pub input: String,

    pub call_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomToolCallOutput {
    pub call_id: String,

    pub output: String,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

// ── Event messages (CLI-side events) ────────────────────────────────

/// Non-model events: task lifecycle, exec results, patch application,
/// token accounting, and a long tail of less common variants. The
/// crate strongly-types the common ones and captures the rest as raw
/// `Value`.
#[derive(Debug, Clone)]
pub enum EventMsg {
    TaskStarted(Value),
    TaskComplete(Value),
    UserMessage(Value),
    AgentMessage(Value),
    TokenCount(Box<TokenCountEvent>),
    ExecCommandEnd(Box<ExecCommandEnd>),
    PatchApplyEnd(Box<PatchApplyEnd>),
    Other { kind: String, payload: Value },
}

impl EventMsg {
    pub fn from_value(payload: &Value) -> Self {
        let kind = payload
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let attempt = match kind.as_str() {
            "task_started" => Some(EventMsg::TaskStarted(payload.clone())),
            "task_complete" => Some(EventMsg::TaskComplete(payload.clone())),
            "user_message" => Some(EventMsg::UserMessage(payload.clone())),
            "agent_message" => Some(EventMsg::AgentMessage(payload.clone())),
            "token_count" => serde_json::from_value::<TokenCountEvent>(payload.clone())
                .ok()
                .map(|v| EventMsg::TokenCount(Box::new(v))),
            "exec_command_end" => serde_json::from_value::<ExecCommandEnd>(payload.clone())
                .ok()
                .map(|v| EventMsg::ExecCommandEnd(Box::new(v))),
            "patch_apply_end" => serde_json::from_value::<PatchApplyEnd>(payload.clone())
                .ok()
                .map(|v| EventMsg::PatchApplyEnd(Box::new(v))),
            _ => None,
        };
        attempt.unwrap_or(EventMsg::Other {
            kind,
            payload: payload.clone(),
        })
    }

    /// The upstream `payload.type` tag value, regardless of variant.
    pub fn kind(&self) -> &str {
        match self {
            EventMsg::TaskStarted(_) => "task_started",
            EventMsg::TaskComplete(_) => "task_complete",
            EventMsg::UserMessage(_) => "user_message",
            EventMsg::AgentMessage(_) => "agent_message",
            EventMsg::TokenCount(_) => "token_count",
            EventMsg::ExecCommandEnd(_) => "exec_command_end",
            EventMsg::PatchApplyEnd(_) => "patch_apply_end",
            EventMsg::Other { kind, .. } => kind.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCountEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<TokenCountInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCountInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_token_usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_token_usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecCommandEnd {
    pub call_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,

    pub command: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,

    #[serde(default)]
    pub parsed_cmd: Vec<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(default)]
    pub stdout: String,

    #[serde(default)]
    pub stderr: String,

    #[serde(default)]
    pub aggregated_output: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Value>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub formatted_output: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchApplyEnd {
    pub call_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,

    #[serde(default)]
    pub stdout: String,

    #[serde(default)]
    pub stderr: String,

    #[serde(default)]
    pub success: bool,

    /// Per-file manifest. Keyed by absolute file path.
    #[serde(default)]
    pub changes: HashMap<String, PatchChange>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

/// One file's change within a [`PatchApplyEnd`]. Three `type` values
/// documented upstream: `add`, `update`, `delete`. We strongly-type
/// the common two (`add` has `content`, `update` has `unified_diff`
/// and optional `move_path`) and leave room for more.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatchChange {
    Add {
        content: String,
        #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
        extra: HashMap<String, Value>,
    },
    Update {
        unified_diff: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_path: Option<String>,
        #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
        extra: HashMap<String, Value>,
    },
    Delete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_content: Option<String>,
        #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
        extra: HashMap<String, Value>,
    },
    /// Catch-all for future change types.
    #[serde(other)]
    Unknown,
}

// ── Compaction marker ───────────────────────────────────────────────

/// Payload of a `compacted` rollout line — Codex's context-compaction
/// boundary marker. The reader treats this payload as a raw `Value` and
/// consumes only `message` (as the summary); this struct exists so the
/// projector can emit a well-shaped `compacted` line that re-parses
/// cleanly. `replacement_history` is the wholesale-replaced prefix Codex
/// records but we don't reconstruct, and `window_id` (when present)
/// identifies the context window. Both are emitted as `null`/absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactedItem {
    /// The summary text that replaced the condensed prefix. Often empty
    /// in real captures.
    #[serde(default)]
    pub message: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_history: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,

    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

// ── Logical session wrapping ────────────────────────────────────────

/// A parsed session: the sequence of lines plus derived first-line
/// metadata and a reference to the file on disk.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub file_path: PathBuf,
    pub lines: Vec<RolloutLine>,
}

impl Session {
    /// The first `session_meta` item if one exists (Codex always
    /// writes it as line 1, but we guard against truncation).
    pub fn meta(&self) -> Option<SessionMeta> {
        self.lines.iter().find_map(|l| match l.item() {
            RolloutItem::SessionMeta(m) => Some(*m),
            _ => None,
        })
    }

    /// Iterate over typed items.
    pub fn items(&self) -> impl Iterator<Item = RolloutItem> + '_ {
        self.lines.iter().map(|l| l.item())
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.lines.iter().filter_map(|l| l.parsed_timestamp()).min()
    }

    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.lines.iter().filter_map(|l| l.parsed_timestamp()).max()
    }

    /// First user-message text in the session, if any.
    ///
    /// Prefers `event_msg.user_message` — Codex emits that for genuine
    /// user input as seen by the TUI. `response_item.message` with
    /// `role: "user"` is less reliable: Codex routinely injects
    /// synthetic user messages (`<environment_context>`, `AGENTS.md`
    /// contents) ahead of the real prompt. Falls back to the first
    /// `response_item` user message only when no `user_message` event
    /// is present.
    pub fn first_user_text(&self) -> Option<String> {
        for line in &self.lines {
            if line.kind == "event_msg"
                && line.payload.get("type").and_then(|v| v.as_str()) == Some("user_message")
                && let Some(msg) = line.payload.get("message").and_then(|v| v.as_str())
                && !msg.is_empty()
            {
                return Some(msg.to_string());
            }
        }
        self.items().find_map(|item| match item {
            RolloutItem::ResponseItem(ResponseItem::Message(m)) if m.role == "user" => {
                let t = m.text();
                if t.is_empty() { None } else { Some(t) }
            }
            _ => None,
        })
    }
}

/// Lightweight session metadata (no full parse).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub file_path: PathBuf,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub cwd: Option<PathBuf>,
    pub cli_version: Option<String>,
    pub first_user_message: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    /// Total line count in the rollout file (approximates `message_count`).
    pub line_count: usize,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_META: &str = r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{"id":"019dabc6-8fef-7681-a054-b5bb75fcb97d","timestamp":"2026-04-20T16:43:30.171Z","cwd":"/tmp/proj","originator":"codex-tui","cli_version":"0.118.0","source":"cli","model_provider":"openai","git":{"commit_hash":"abc","branch":"main","repository_url":"git@example:x/y.git"}}}"#;

    #[test]
    fn rollout_line_parses_session_meta() {
        let line: RolloutLine = serde_json::from_str(SAMPLE_META).unwrap();
        assert_eq!(line.kind, "session_meta");
        match line.item() {
            RolloutItem::SessionMeta(m) => {
                assert_eq!(m.id, "019dabc6-8fef-7681-a054-b5bb75fcb97d");
                assert_eq!(m.cwd.to_str().unwrap(), "/tmp/proj");
                assert_eq!(m.git.as_ref().unwrap().branch.as_deref(), Some("main"));
            }
            _ => panic!("expected SessionMeta"),
        }
    }

    #[test]
    fn rollout_line_preserves_unknown_kind() {
        let raw = r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"new_future_type","payload":{"foo":"bar"}}"#;
        let line: RolloutLine = serde_json::from_str(raw).unwrap();
        match line.item() {
            RolloutItem::Unknown { kind, payload } => {
                assert_eq!(kind, "new_future_type");
                assert_eq!(payload["foo"], "bar");
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn response_item_message_variants() {
        let raw = r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}],"phase":"commentary"}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        match ResponseItem::from_value(&v) {
            ResponseItem::Message(m) => {
                assert_eq!(m.role, "assistant");
                assert_eq!(m.text(), "hello");
                assert_eq!(m.phase.as_deref(), Some("commentary"));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn response_item_reasoning_keeps_encrypted_content() {
        let raw =
            r#"{"type":"reasoning","summary":[],"content":null,"encrypted_content":"gAAA..."}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        match ResponseItem::from_value(&v) {
            ResponseItem::Reasoning(r) => {
                assert_eq!(r.encrypted_content.as_deref(), Some("gAAA..."));
                assert!(r.summary.is_empty());
            }
            _ => panic!("expected Reasoning"),
        }
    }

    #[test]
    fn response_item_function_call_keeps_raw_arguments() {
        let raw = r#"{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}","call_id":"call_1"}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        match ResponseItem::from_value(&v) {
            ResponseItem::FunctionCall(c) => {
                assert_eq!(c.name, "exec_command");
                assert_eq!(c.call_id, "call_1");
                assert_eq!(c.arguments_as_json()["cmd"], "pwd");
            }
            _ => panic!("expected FunctionCall"),
        }
    }

    #[test]
    fn response_item_unknown_survives() {
        let raw = r#"{"type":"web_search_call","query":"rust","call_id":"c"}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        match ResponseItem::from_value(&v) {
            ResponseItem::Other { kind, payload } => {
                assert_eq!(kind, "web_search_call");
                assert_eq!(payload["query"], "rust");
            }
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn event_msg_variants_dispatch() {
        let token = serde_json::json!({
            "type":"token_count",
            "info":{"total_token_usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120}}
        });
        match EventMsg::from_value(&token) {
            EventMsg::TokenCount(tc) => {
                let usage = tc
                    .info
                    .as_ref()
                    .unwrap()
                    .total_token_usage
                    .as_ref()
                    .unwrap();
                assert_eq!(usage.input_tokens, Some(100));
                assert_eq!(usage.output_tokens, Some(20));
            }
            _ => panic!("expected TokenCount"),
        }
    }

    #[test]
    fn patch_apply_end_change_variants() {
        let add = r#"{"type":"add","content":"hello\n"}"#;
        let pc: PatchChange = serde_json::from_str(add).unwrap();
        assert!(matches!(pc, PatchChange::Add { .. }));

        let upd = r#"{"type":"update","unified_diff":"@@\n-x\n+y"}"#;
        let pc: PatchChange = serde_json::from_str(upd).unwrap();
        assert!(matches!(pc, PatchChange::Update { .. }));
    }

    #[test]
    fn patch_apply_end_unknown_change_type() {
        let raw = r#"{"type":"rename","from":"a","to":"b"}"#;
        let pc: PatchChange = serde_json::from_str(raw).unwrap();
        assert!(matches!(pc, PatchChange::Unknown));
    }

    #[test]
    fn session_meta_roundtrip() {
        let line: RolloutLine = serde_json::from_str(SAMPLE_META).unwrap();
        let back = serde_json::to_string(&line).unwrap();
        let orig: Value = serde_json::from_str(SAMPLE_META).unwrap();
        let back_v: Value = serde_json::from_str(&back).unwrap();
        // Field-by-field equality (order-independent)
        assert_eq!(orig, back_v);
    }

    #[test]
    fn rollout_line_preserves_unknown_toplevel_field() {
        let raw = r#"{"timestamp":"t","type":"session_meta","payload":{},"new_field":42}"#;
        let line: RolloutLine = serde_json::from_str(raw).unwrap();
        assert_eq!(line.extra.get("new_field"), Some(&serde_json::json!(42)));
        let back = serde_json::to_string(&line).unwrap();
        assert!(back.contains("new_field"));
    }

    #[test]
    fn content_part_unknown_survives() {
        let raw = r#"{"type":"image_url","url":"data:..."}"#;
        let cp: ContentPart = serde_json::from_str(raw).unwrap();
        assert!(matches!(cp, ContentPart::Unknown));
    }

    #[test]
    fn message_multi_part_text() {
        let m = Message {
            role: "user".into(),
            content: vec![
                ContentPart::InputText {
                    text: "one".into(),
                    extra: Default::default(),
                },
                ContentPart::InputText {
                    text: "two".into(),
                    extra: Default::default(),
                },
            ],
            id: None,
            end_turn: None,
            phase: None,
            extra: Default::default(),
        };
        assert_eq!(m.text(), "one\ntwo");
    }

    fn session_from_lines(lines: &[&str]) -> Session {
        let parsed: Vec<RolloutLine> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Session {
            id: "s".to_string(),
            file_path: PathBuf::from("/tmp/session.jsonl"),
            lines: parsed,
        }
    }

    #[test]
    fn first_user_text_prefers_user_message_event() {
        // Codex injects a synthetic `<environment_context>` user message
        // ahead of the real prompt; the TUI-facing event_msg.user_message
        // carries the real thing. first_user_text must return the latter.
        let s = session_from_lines(&[
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>\n</environment_context>"}]}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"build me a thing"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"build me a thing"}]}}"#,
        ]);
        assert_eq!(s.first_user_text().as_deref(), Some("build me a thing"));
    }

    #[test]
    fn first_user_text_falls_back_when_no_user_message_event() {
        // Sessions from CLI versions that don't emit user_message fall
        // back to the first response_item user turn.
        let s = session_from_lines(&[
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
        ]);
        assert_eq!(s.first_user_text().as_deref(), Some("hello"));
    }

    #[test]
    fn first_user_text_ignores_empty_user_message_event() {
        // An empty event_msg.user_message is not informative — fall back.
        let s = session_from_lines(&[
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":""}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real prompt"}]}}"#,
        ]);
        assert_eq!(s.first_user_text().as_deref(), Some("real prompt"));
    }
}
