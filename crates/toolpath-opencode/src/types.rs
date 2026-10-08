//! On-disk schema for opencode SQLite databases.
//!
//! opencode stores everything in `opencode.db`. Most tables are
//! simple scalar rows, but `message.data` and `part.data` are JSON
//! blobs that discriminate on `role` and `type` respectively. These
//! types mirror the upstream Zod schemas in
//! [`packages/opencode/src/session/message-v2.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts)
//! closely enough for fidelity, but use `Value` fallbacks on enum
//! variants we don't exhaustively enumerate so unknown future
//! payloads survive round-trip.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

// ── Project ─────────────────────────────────────────────────────────

/// A `project` row. `id` is the SHA of the repo's first root commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub worktree: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_initialized: Option<i64>,
    /// JSON `string[]` — sandbox paths.
    #[serde(default)]
    pub sandboxes: Vec<String>,
}

// ── Session ─────────────────────────────────────────────────────────

/// A `session` row — one opencode conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub slug: String,
    pub directory: PathBuf,
    pub title: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_additions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_deletions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_files: Option<i64>,
    pub time_created: i64,
    pub time_updated: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_compacting: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_archived: Option<i64>,

    /// Ordered messages, populated by the reader. Empty until
    /// `read_session` is called.
    #[serde(default)]
    pub messages: Vec<Message>,
}

impl Session {
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        Utc.timestamp_millis_opt(self.time_created).single()
    }
    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        Utc.timestamp_millis_opt(self.time_updated).single()
    }

    /// First user-message text across the conversation. Rides over
    /// any system/summary messages at the head.
    pub fn first_user_text(&self) -> Option<String> {
        for msg in &self.messages {
            if let MessageData::User(_) = &msg.data {
                for part in &msg.parts {
                    if let PartData::Text(t) = &part.data
                        && !t.text.is_empty()
                    {
                        return Some(t.text.clone());
                    }
                }
            }
        }
        None
    }
}

/// Lightweight metadata (no message bodies).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub project_id: String,
    pub directory: PathBuf,
    pub title: String,
    pub version: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub first_user_message: Option<String>,
    pub summary_additions: Option<i64>,
    pub summary_deletions: Option<i64>,
    pub summary_files: Option<i64>,
}

// ── Message ────────────────────────────────────────────────────────

/// A `message` row plus its associated parts, ordered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub time_created: i64,
    pub time_updated: i64,
    pub data: MessageData,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// `message.data` — discriminated on `role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum MessageData {
    User(UserMessage),
    Assistant(AssistantMessage),
    /// Catch-all for future roles.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    pub time: MessageTime,
    #[serde(default)]
    pub agent: String,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<UserSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<HashMap<String, bool>>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub diffs: Vec<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    #[serde(rename = "parentID")]
    pub parent_id: String,
    pub time: MessageTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default)]
    pub agent: String,
    /// Deprecated in upstream, still on old rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(rename = "modelID", default)]
    pub model_id: String,
    #[serde(rename = "providerID", default)]
    pub provider_id: String,
    pub path: MessagePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageTime {
    pub created: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRef {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePath {
    pub cwd: PathBuf,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default)]
    pub cache: TokenCache,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCache {
    #[serde(default)]
    pub read: u64,
    #[serde(default)]
    pub write: u64,
}

// ── Part ───────────────────────────────────────────────────────────

/// A `part` row. The `data` column is the typed variant payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub message_id: String,
    pub session_id: String,
    pub time_created: i64,
    pub time_updated: i64,
    pub data: PartData,
}

/// `part.data` — discriminated on `type`.
///
/// Unknown variants land in [`PartData::Unknown`] so a future
/// opencode release that adds a new part type still round-trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PartData {
    Text(TextPart),
    Reasoning(ReasoningPart),
    Tool(ToolPart),
    File(FilePart),
    Agent(AgentPart),
    Subtask(SubtaskPart),
    Retry(RetryPart),
    Compaction(CompactionPart),
    StepStart(StepStartPart),
    StepFinish(StepFinishPart),
    Snapshot(SnapshotPart),
    Patch(PatchPart),
    /// Catch-all for future variants. The `type` tag and payload are
    /// preserved in the typed layer only when the round-trip deserializer
    /// records them; `serde(other)` discards the tag string itself.
    #[serde(other)]
    Unknown,
}

impl PartData {
    /// The upstream `type` tag value.
    pub fn kind(&self) -> &'static str {
        match self {
            PartData::Text(_) => "text",
            PartData::Reasoning(_) => "reasoning",
            PartData::Tool(_) => "tool",
            PartData::File(_) => "file",
            PartData::Agent(_) => "agent",
            PartData::Subtask(_) => "subtask",
            PartData::Retry(_) => "retry",
            PartData::Compaction(_) => "compaction",
            PartData::StepStart(_) => "step-start",
            PartData::StepFinish(_) => "step-finish",
            PartData::Snapshot(_) => "snapshot",
            PartData::Patch(_) => "patch",
            PartData::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPart {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningPart {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPart {
    pub tool: String,
    #[serde(rename = "callID")]
    pub call_id: String,
    pub state: ToolState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

/// Tool-call execution state. Discriminated on `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolState {
    Pending(ToolStatePending),
    Running(ToolStateRunning),
    Completed(ToolStateCompleted),
    Error(ToolStateError),
    /// Unknown future status. The raw payload is preserved only
    /// at the JSON layer; this variant carries no fields.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatePending {
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStateRunning {
    #[serde(default)]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: ToolStartTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStartTime {
    pub start: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStateCompleted {
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub metadata: Value,
    pub time: ToolRunTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRunTime {
    pub start: i64,
    pub end: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStateError {
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub time: ToolRunTime,
}

impl ToolState {
    /// Extract the common `input` field regardless of status variant.
    pub fn input(&self) -> Option<&Value> {
        match self {
            ToolState::Pending(p) => Some(&p.input),
            ToolState::Running(r) => Some(&r.input),
            ToolState::Completed(c) => Some(&c.input),
            ToolState::Error(e) => Some(&e.input),
            ToolState::Unknown => None,
        }
    }

    pub fn output(&self) -> Option<&str> {
        match self {
            ToolState::Completed(c) => Some(&c.output),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            ToolState::Error(e) => Some(&e.error),
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, ToolState::Error(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePart {
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPart {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskPart {
    pub prompt: String,
    pub description: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPart {
    pub attempt: u32,
    pub error: Value,
    pub time: RetryTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryTime {
    pub created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionPart {
    pub auto: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_start_id: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepStartPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepFinishPart {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub tokens: Tokens,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPart {
    pub snapshot: String,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchPart {
    pub hash: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub extra: HashMap<String, Value>,
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_message() {
        let raw = r#"{"role":"user","time":{"created":1776792838512},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"},"summary":{"diffs":[]}}"#;
        let md: MessageData = serde_json::from_str(raw).unwrap();
        match md {
            MessageData::User(u) => {
                assert_eq!(u.agent, "build");
                assert_eq!(u.model.provider_id, "opencode");
                assert_eq!(u.model.model_id, "big-pickle");
                assert_eq!(u.time.created, 1776792838512);
            }
            _ => panic!("expected User"),
        }
    }

    #[test]
    fn parse_assistant_message() {
        let raw = r#"{"parentID":"msg_x","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/p","root":"/p"},"cost":0.0,"tokens":{"total":10,"input":5,"output":3,"reasoning":2,"cache":{"read":1,"write":0}},"modelID":"m","providerID":"p","time":{"created":1,"completed":2},"finish":"stop"}"#;
        let md: MessageData = serde_json::from_str(raw).unwrap();
        match md {
            MessageData::Assistant(a) => {
                assert_eq!(a.parent_id, "msg_x");
                assert_eq!(a.model_id, "m");
                assert_eq!(a.provider_id, "p");
                assert_eq!(a.finish.as_deref(), Some("stop"));
                assert_eq!(a.tokens.input, 5);
                assert_eq!(a.tokens.cache.read, 1);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn unknown_role_roundtrips_to_other() {
        let raw = r#"{"role":"system","text":"hi"}"#;
        let md: MessageData = serde_json::from_str(raw).unwrap();
        assert!(matches!(md, MessageData::Other));
    }

    #[test]
    fn parse_text_part() {
        let raw = r#"{"type":"text","text":"hello"}"#;
        let pd: PartData = serde_json::from_str(raw).unwrap();
        match pd {
            PartData::Text(t) => assert_eq!(t.text, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn parse_reasoning_part() {
        let raw = r#"{"type":"reasoning","text":"thinking","time":{"start":1,"end":2},"metadata":{"anthropic":{"signature":"abc"}}}"#;
        let pd: PartData = serde_json::from_str(raw).unwrap();
        match pd {
            PartData::Reasoning(r) => {
                assert_eq!(r.text, "thinking");
                assert_eq!(r.time.as_ref().unwrap().start, 1);
                assert_eq!(r.time.as_ref().unwrap().end, Some(2));
            }
            _ => panic!("expected Reasoning"),
        }
    }

    #[test]
    fn parse_tool_part_completed() {
        let raw = r#"{"type":"tool","tool":"bash","callID":"c1","state":{"status":"completed","input":{"command":"ls"},"output":"a\nb\n","title":"List","metadata":{"exit":0},"time":{"start":1,"end":2}}}"#;
        let pd: PartData = serde_json::from_str(raw).unwrap();
        match pd {
            PartData::Tool(t) => {
                assert_eq!(t.tool, "bash");
                assert_eq!(t.call_id, "c1");
                match &t.state {
                    ToolState::Completed(c) => {
                        assert_eq!(c.output, "a\nb\n");
                        assert_eq!(c.title, "List");
                        assert_eq!(c.metadata["exit"], 0);
                    }
                    _ => panic!("expected Completed"),
                }
            }
            _ => panic!("expected Tool"),
        }
    }

    #[test]
    fn parse_tool_part_error() {
        let raw = r#"{"type":"tool","tool":"bash","callID":"c","state":{"status":"error","input":{"command":"false"},"error":"exit 1","time":{"start":1,"end":2}}}"#;
        let pd: PartData = serde_json::from_str(raw).unwrap();
        match pd {
            PartData::Tool(t) => match &t.state {
                ToolState::Error(e) => {
                    assert_eq!(e.error, "exit 1");
                }
                _ => panic!("expected Error"),
            },
            _ => panic!("expected Tool"),
        }
    }

    #[test]
    fn parse_step_parts() {
        let raw_start = r#"{"type":"step-start","snapshot":"abc"}"#;
        let pd: PartData = serde_json::from_str(raw_start).unwrap();
        assert!(matches!(pd, PartData::StepStart(_)));

        let raw_finish = r#"{"type":"step-finish","reason":"stop","snapshot":"abc","tokens":{"input":1,"output":2,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.001}"#;
        let pd: PartData = serde_json::from_str(raw_finish).unwrap();
        match pd {
            PartData::StepFinish(s) => {
                assert_eq!(s.reason, "stop");
                assert_eq!(s.snapshot.as_deref(), Some("abc"));
            }
            _ => panic!("expected StepFinish"),
        }
    }

    #[test]
    fn parse_unknown_part_type() {
        let raw = r#"{"type":"future-thing","foo":"bar"}"#;
        let pd: PartData = serde_json::from_str(raw).unwrap();
        assert!(matches!(pd, PartData::Unknown));
        assert_eq!(pd.kind(), "unknown");
    }

    #[test]
    fn parse_compaction_and_retry() {
        let c: PartData =
            serde_json::from_str(r#"{"type":"compaction","auto":true,"overflow":false}"#).unwrap();
        assert!(matches!(c, PartData::Compaction(_)));

        let r: PartData = serde_json::from_str(
            r#"{"type":"retry","attempt":1,"error":{"message":"nope"},"time":{"created":1}}"#,
        )
        .unwrap();
        assert!(matches!(r, PartData::Retry(_)));
    }

    #[test]
    fn tokens_roundtrip() {
        let raw = r#"{"total":10,"input":5,"output":3,"reasoning":2,"cache":{"read":1,"write":0}}"#;
        let t: Tokens = serde_json::from_str(raw).unwrap();
        let back = serde_json::to_value(&t).unwrap();
        let orig: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(orig, back);
    }

    #[test]
    fn message_extras_survive() {
        let raw = r#"{"role":"user","time":{"created":1},"agent":"build","model":{"providerID":"p","modelID":"m"},"future_field":"kept"}"#;
        let md: MessageData = serde_json::from_str(raw).unwrap();
        match md {
            MessageData::User(u) => {
                assert_eq!(
                    u.extra.get("future_field"),
                    Some(&serde_json::json!("kept"))
                );
            }
            _ => panic!("expected User"),
        }
    }
}
