//! Typed model of the Amp thread **export document** (`amp threads export`).
//!
//! ⚠️ **The export schema is undocumented and reverse-engineered.** See
//! `docs/agents/formats/amp/events.md`. Everything here is built for
//! wire-level round-trip fidelity: only the fields the provider needs are
//! typed; everything else — including explicit `null`s like `readAt` and
//! `openExpiresAt` — is preserved verbatim in `#[serde(flatten)]` extras.
//! Unknown content-block types degrade to [`Block::Unknown`] instead of
//! erroring, so a schema change degrades gracefully.
//!
//! Naming trap (see events.md): the export's `messageId` is a 1-based
//! **integer index**; the stable string id is `protocolMessageID` (`M-…`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

// ── Export document ──────────────────────────────────────────────────

/// The top-level export envelope.
///
/// `v` is a **revision counter**, not a schema version — do not branch
/// parsing on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadExport {
    /// Thread id (`T-<uuidv7-ish>`).
    pub id: String,

    /// Revision counter (mutation count, NOT a schema version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u64>,

    /// Server-generated title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Creation time, epoch **milliseconds**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,

    /// Last update, ISO 8601 (note: different type from `created`).
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,

    /// Environment stamp (`env.initial`: workspace trees + platform).
    /// Kept as raw JSON for fidelity; use the accessors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Value>,

    /// Thread metadata (visibility, agent state, …). Raw for fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,

    /// Skills activated during the thread.
    #[serde(
        rename = "activatedSkills",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub activated_skills: Option<Value>,

    /// The conversation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,

    /// Everything else on the envelope (`agentMode`, `creatorUserID`,
    /// `pinned`, the explicitly-null `openExpiresAt`, …).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ThreadExport {
    /// Session working directory: `env.initial.trees[0].uri`, scheme stripped.
    pub fn working_dir(&self) -> Option<String> {
        let uri = self
            .env
            .as_ref()?
            .pointer("/initial/trees/0/uri")?
            .as_str()?;
        Some(strip_file_scheme(uri).to_string())
    }

    /// The Amp build that created the thread
    /// (`env.initial.platform.clientVersion`) — the per-thread version
    /// anchor, distinct from whatever `amp --version` says today.
    pub fn client_version(&self) -> Option<String> {
        self.env
            .as_ref()?
            .pointer("/initial/platform/clientVersion")?
            .as_str()
            .map(str::to_string)
    }

    /// Creation time as a UTC datetime.
    pub fn created_at(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp_millis(self.created?)
    }

    /// Last-update time as a UTC datetime.
    pub fn updated_at_utc(&self) -> Option<DateTime<Utc>> {
        let ts = self.updated_at.as_deref()?;
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// First non-empty human prompt text. Tool-result `user` messages are
    /// plumbing, not prompts, and never match (they carry no text blocks).
    pub fn first_user_text(&self) -> Option<String> {
        self.messages
            .iter()
            .filter(|m| m.role == "user")
            .find_map(|m| {
                let text = m.text();
                (!text.trim().is_empty()).then_some(text)
            })
    }
}

// ── Messages ─────────────────────────────────────────────────────────

/// One element of `messages`. Roles are `user` | `assistant` only, but a
/// `user` message is not necessarily a human turn — tool results come back
/// as `user` messages carrying `tool_result` blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<Block>,

    /// The stable `M-<base62>` string id.
    #[serde(
        rename = "protocolMessageID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub protocol_message_id: Option<String>,

    /// 1-based integer index — NOT the message's id.
    #[serde(rename = "messageId", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,

    /// Per-message token usage (assistant messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MessageUsage>,

    /// Everything else (`meta`, `state`, `userState`, `readAt` — often an
    /// explicit `null` — `protocolMessageVersion`, …).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Message {
    /// Concatenated `text` block contents, in order.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let Block::Known(KnownBlock::Text(t)) = b {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&t.text);
            }
        }
        out
    }

    /// `state.stopReason` (assistant messages).
    pub fn stop_reason(&self) -> Option<&str> {
        self.extra.get("state")?.get("stopReason")?.as_str()
    }

    /// `meta.sentAt` (user messages), epoch milliseconds.
    pub fn sent_at_ms(&self) -> Option<i64> {
        self.extra.get("meta")?.get("sentAt")?.as_i64()
    }

    /// True when every content block is a `tool_result` — i.e. this `user`
    /// message is transport plumbing, not a human turn.
    pub fn is_tool_result_only(&self) -> bool {
        !self.content.is_empty()
            && self
                .content
                .iter()
                .all(|b| matches!(b, Block::Known(KnownBlock::ToolResult(_))))
    }
}

/// Per-message token usage, camelCase on the wire.
///
/// `totalInputTokens` is a derived sum (`input + cacheRead + cacheCreation`,
/// verified on all captured usage objects) and `maxInputTokens` is the
/// context-window capacity — both are preserved for round-tripping but MUST
/// NOT be summed into any spend total.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    #[serde(
        rename = "inputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<u32>,

    #[serde(
        rename = "outputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u32>,

    #[serde(
        rename = "cacheReadInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_read_input_tokens: Option<u32>,

    #[serde(
        rename = "cacheCreationInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_creation_input_tokens: Option<u32>,

    /// Context-window capacity. NOT spend.
    #[serde(
        rename = "maxInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_input_tokens: Option<u64>,

    /// Derived sum of the input-side counters. NOT an independent counter.
    #[serde(
        rename = "totalInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub total_input_tokens: Option<u64>,

    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MessageUsage {
    /// True when every real counter is zero or absent — a placeholder, not a
    /// measurement. Decodes to `None` (kind v1.1.0: never stamp zero-filled
    /// placeholders onto a step).
    pub fn is_usage_zero(&self) -> bool {
        self.input_tokens.unwrap_or(0) == 0
            && self.output_tokens.unwrap_or(0) == 0
            && self.cache_read_input_tokens.unwrap_or(0) == 0
            && self.cache_creation_input_tokens.unwrap_or(0) == 0
    }
}

// ── Content blocks ───────────────────────────────────────────────────

/// A content block. Known types parse into [`KnownBlock`]; anything else is
/// preserved verbatim as [`Block::Unknown`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Block {
    Known(KnownBlock),
    Unknown(Value),
}

/// The four observed block types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum KnownBlock {
    #[serde(rename = "text")]
    Text(TextBlock),
    #[serde(rename = "thinking")]
    Thinking(ThinkingBlock),
    #[serde(rename = "tool_use")]
    ToolUse(ToolUseBlock),
    #[serde(rename = "tool_result")]
    ToolResult(ToolResultBlock),
}

/// `{type: "text"}` — visible prose. Assistant text blocks also carry
/// `startTime`/`finalTime`/`blockState` (kept in `extra`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{type: "thinking"}` — a reasoning summary. **Frequently empty**: the
/// real chain of thought is sealed in the encrypted provider blob
/// (`signature` / `openAIReasoning.encryptedContent`, kept in `extra`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    #[serde(default)]
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{type: "tool_use"}`. `id` is Amp's own `TU-<base62>`;
/// `providerToolUseId` (in `extra`) is the upstream model provider's id.
/// **Results pair by `id`.**
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{type: "tool_result"}` — always in the **following** `user` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    #[serde(rename = "toolUseID")]
    pub tool_use_id: String,
    pub run: RunPayload,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The `run` object on a tool result. `status` is a lifecycle state, not a
/// success flag — it is `"done"` even for failed commands. Errors are only
/// signalled inside `result` (e.g. `shell_command`'s `exitCode`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPayload {
    /// Polymorphic, keyed by which tool ran: `shell_command` →
    /// `{output, exitCode}`; `apply_patch` → `{files: […], summary}`;
    /// `skill` → `{content: […]}`; `Task` → a plain string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ── Session wrapper ──────────────────────────────────────────────────

/// A fetched thread plus provenance about where it came from.
#[derive(Debug, Clone)]
pub struct Session {
    /// Thread id (`T-…`), from the export.
    pub id: String,
    /// The parsed export document.
    pub export: ThreadExport,
    /// Backing file when read from disk; `None` for a live CLI fetch.
    pub source_path: Option<PathBuf>,
}

impl Session {
    pub fn from_export(export: ThreadExport) -> Self {
        Self {
            id: export.id.clone(),
            export,
            source_path: None,
        }
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.export.created_at()
    }

    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.export.updated_at_utc()
    }

    pub fn cwd(&self) -> Option<String> {
        self.export.working_dir()
    }

    pub fn version(&self) -> Option<String> {
        self.export.client_version()
    }

    pub fn first_user_text(&self) -> Option<String> {
        self.export.first_user_text()
    }

    /// Number of messages in the thread (the analogue of a JSONL provider's
    /// line count).
    pub fn message_count(&self) -> usize {
        self.export.messages.len()
    }
}

/// Lightweight per-session metadata for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    /// The Amp build that created the thread (per-thread version anchor).
    pub version: Option<String>,
    pub first_user_message: Option<String>,
    /// Message count (kept name-compatible with the other providers' TSV
    /// column even though the export is a document, not JSONL).
    pub line_count: usize,
    /// Backing file when read from disk; `None` for a live CLI fetch.
    pub dir_path: Option<PathBuf>,
}

/// Strip a `file://` scheme, leaving the absolute path.
pub fn strip_file_scheme(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/real-session.json");

    fn real() -> ThreadExport {
        serde_json::from_str(REAL).expect("parse real fixture")
    }

    #[test]
    fn parses_real_envelope() {
        let ex = real();
        assert_eq!(ex.id, "T-019fa4db-29cf-70c9-8d9b-81524df70e52");
        assert_eq!(ex.v, Some(73));
        assert_eq!(ex.title.as_deref(), Some("Filesystem tool exercise"));
        assert_eq!(ex.created, Some(1785177254351));
        assert_eq!(ex.messages.len(), 24);
    }

    #[test]
    fn envelope_extras_preserved() {
        let ex = real();
        assert_eq!(
            ex.extra.get("agentMode").and_then(|v| v.as_str()),
            Some("medium")
        );
        // Explicit null must survive (round-trip fidelity).
        assert_eq!(ex.extra.get("openExpiresAt"), Some(&Value::Null));
        assert!(ex.extra.contains_key("creatorUserID"));
        assert_eq!(ex.extra.get("pinned"), Some(&Value::Bool(false)));
    }

    #[test]
    fn working_dir_strips_file_scheme() {
        assert_eq!(real().working_dir().as_deref(), Some("/tmp/amp-elicit"));
    }

    #[test]
    fn client_version_is_thread_anchor() {
        assert_eq!(
            real().client_version().as_deref(),
            Some("0.0.1785170481-ga5b614")
        );
    }

    #[test]
    fn created_and_updated_parse() {
        let ex = real();
        let created = ex.created_at().unwrap();
        assert_eq!(created.timestamp_millis(), 1785177254351);
        let updated = ex.updated_at_utc().unwrap();
        assert!(updated >= created);
    }

    #[test]
    fn first_user_text_skips_tool_result_messages() {
        let text = real().first_user_text().unwrap();
        assert!(text.starts_with("You're going to walk through"));
    }

    #[test]
    fn message_ids_and_roles() {
        let ex = real();
        let m = &ex.messages[0];
        assert_eq!(m.role, "user");
        assert_eq!(m.message_id, Some(1));
        assert_eq!(
            m.protocol_message_id.as_deref(),
            Some("M-033wt7sbSDccKHLOPDTqjB")
        );
    }

    #[test]
    fn message_extras_keep_explicit_null_read_at() {
        let ex = real();
        assert_eq!(ex.messages[0].extra.get("readAt"), Some(&Value::Null));
    }

    #[test]
    fn stop_reason_and_sent_at_accessors() {
        let ex = real();
        assert_eq!(ex.messages[1].stop_reason(), Some("tool_use"));
        assert_eq!(ex.messages[23].stop_reason(), Some("end_turn"));
        assert_eq!(ex.messages[0].sent_at_ms(), Some(1785177255723));
    }

    #[test]
    fn tool_result_only_detection() {
        let ex = real();
        assert!(!ex.messages[0].is_tool_result_only()); // the human prompt
        assert!(ex.messages[2].is_tool_result_only()); // skill result
        assert!(!ex.messages[1].is_tool_result_only()); // assistant
    }

    #[test]
    fn usage_parses_all_counters() {
        let ex = real();
        let u = ex.messages[1].usage.as_ref().unwrap();
        assert_eq!(u.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(u.input_tokens, Some(0));
        assert_eq!(u.output_tokens, Some(117));
        assert_eq!(u.cache_read_input_tokens, Some(0));
        assert_eq!(u.cache_creation_input_tokens, Some(16954));
        assert_eq!(u.max_input_tokens, Some(272000));
        assert_eq!(u.total_input_tokens, Some(16954));
    }

    #[test]
    fn usage_zero_guard() {
        let zero = MessageUsage {
            max_input_tokens: Some(272000), // capacity is not spend
            total_input_tokens: Some(0),
            ..Default::default()
        };
        assert!(zero.is_usage_zero());
        let real_usage = MessageUsage {
            input_tokens: Some(0),
            cache_creation_input_tokens: Some(16954),
            ..Default::default()
        };
        assert!(!real_usage.is_usage_zero());
    }

    #[test]
    fn blocks_classify_known_types() {
        let ex = real();
        let kinds: Vec<&str> = ex.messages[1]
            .content
            .iter()
            .map(|b| match b {
                Block::Known(KnownBlock::Text(_)) => "text",
                Block::Known(KnownBlock::Thinking(_)) => "thinking",
                Block::Known(KnownBlock::ToolUse(_)) => "tool_use",
                Block::Known(KnownBlock::ToolResult(_)) => "tool_result",
                Block::Unknown(_) => "unknown",
            })
            .collect();
        assert_eq!(kinds, vec!["thinking", "text", "tool_use"]);
    }

    #[test]
    fn unknown_block_type_preserved_not_dropped() {
        let json = r#"{"type":"hologram","payload":{"x":1}}"#;
        let b: Block = serde_json::from_str(json).unwrap();
        assert!(matches!(b, Block::Unknown(_)));
        let back = serde_json::to_value(&b).unwrap();
        assert_eq!(back, serde_json::from_str::<Value>(json).unwrap());
    }

    #[test]
    fn tool_use_block_fields() {
        let ex = real();
        let Some(Block::Known(KnownBlock::ToolUse(tu))) = ex.messages[3].content.last() else {
            panic!("expected tool_use block");
        };
        assert_eq!(tu.id, "TU-033wt8676K5StumvhRV8kd");
        assert_eq!(tu.name, "shell_command");
        assert_eq!(tu.input["command"], "ls -la");
        assert!(tu.extra.contains_key("providerToolUseId"));
    }

    #[test]
    fn tool_result_block_polymorphic_results() {
        let ex = real();
        // shell_command → {output, exitCode}
        let Some(Block::Known(KnownBlock::ToolResult(tr))) = ex.messages[4].content.first() else {
            panic!("expected tool_result block");
        };
        assert_eq!(tr.tool_use_id, "TU-033wt8676K5StumvhRV8kd");
        assert_eq!(tr.run.status.as_deref(), Some("done"));
        assert_eq!(tr.run.result.as_ref().unwrap()["exitCode"], 0);
        // Task → plain string
        let Some(Block::Known(KnownBlock::ToolResult(task))) = ex.messages[22].content.first()
        else {
            panic!("expected tool_result block");
        };
        assert!(task.run.result.as_ref().unwrap().is_string());
    }

    #[test]
    fn thinking_block_empty_and_nonempty() {
        let ex = real();
        let thinking_of = |i: usize| -> String {
            let Some(Block::Known(KnownBlock::Thinking(t))) = ex.messages[i].content.first() else {
                panic!("expected thinking block first in message {i}");
            };
            t.thinking.clone()
        };
        assert!(thinking_of(1).contains("Preparing for tool usage"));
        assert!(thinking_of(7).is_empty()); // sealed reasoning, empty summary
    }

    #[test]
    fn wire_value_identity_whole_document() {
        let orig: Value = serde_json::from_str(REAL).unwrap();
        let typed = real();
        let back = serde_json::to_value(&typed).unwrap();
        assert_eq!(orig, back, "export document not value-identical");
    }

    #[test]
    fn session_metadata_surface() {
        let s = Session::from_export(real());
        assert_eq!(s.id, "T-019fa4db-29cf-70c9-8d9b-81524df70e52");
        assert_eq!(s.message_count(), 24);
        assert_eq!(s.cwd().as_deref(), Some("/tmp/amp-elicit"));
        assert_eq!(s.version().as_deref(), Some("0.0.1785170481-ga5b614"));
        assert!(s.started_at().is_some());
        assert!(s.last_activity().is_some());
        assert!(s.first_user_text().is_some());
    }

    #[test]
    fn strip_file_scheme_variants() {
        assert_eq!(strip_file_scheme("file:///tmp/x"), "/tmp/x");
        assert_eq!(strip_file_scheme("/tmp/x"), "/tmp/x");
    }
}
