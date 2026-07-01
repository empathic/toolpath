//! Typed model of the Copilot CLI `events.jsonl` stream.
//!
//! ⚠️ **The `events.jsonl` schema is undocumented and reverse-engineered.**
//! See `docs/agents/formats/copilot-cli/events.md`. Everything here is built
//! to be *tolerant*: the line envelope accepts the payload inline, under
//! `data`, or under `payload`; field extraction tries several key spellings;
//! unrecognized event types fall through to [`CopilotEvent::Unknown`] so a
//! schema change degrades gracefully instead of dropping data.
//!
//! When a real session is captured (see
//! `docs/agents/formats/copilot-cli/known-gaps-and-sourcing.md`), tighten the
//! extraction here against the observed shapes and upgrade the confidence
//! tags in the docs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

// ── Event type discriminants (v1.0.54, reverse-engineered) ───────────

pub const EV_SESSION_START: &str = "session.start";
pub const EV_SESSION_TASK_COMPLETE: &str = "session.task_complete";
pub const EV_SESSION_SHUTDOWN: &str = "session.shutdown";
pub const EV_SESSION_MODEL_CHANGE: &str = "session.model_change";
pub const EV_SESSION_MODE_CHANGED: &str = "session.mode_changed";
pub const EV_SESSION_PLAN_CHANGED: &str = "session.plan_changed";
pub const EV_SESSION_COMPACTION_START: &str = "session.compaction_start";
pub const EV_SESSION_COMPACTION_COMPLETE: &str = "session.compaction_complete";
pub const EV_USER_MESSAGE: &str = "user.message";
pub const EV_ASSISTANT_TURN_START: &str = "assistant.turn_start";
pub const EV_ASSISTANT_MESSAGE: &str = "assistant.message";
pub const EV_ASSISTANT_TURN_END: &str = "assistant.turn_end";
pub const EV_TOOL_EXECUTION_START: &str = "tool.execution_start";
pub const EV_TOOL_EXECUTION_COMPLETE: &str = "tool.execution_complete";
pub const EV_SUBAGENT_STARTED: &str = "subagent.started";
pub const EV_SUBAGENT_COMPLETED: &str = "subagent.completed";
pub const EV_SKILL_INVOKED: &str = "skill.invoked";
pub const EV_HOOK_START: &str = "hook.start";
pub const EV_HOOK_END: &str = "hook.end";
pub const EV_ABORT: &str = "abort";

// ── Line envelope ────────────────────────────────────────────────────

/// One line of `events.jsonl`.
///
/// The payload location is uncertain (see module docs), so we keep `data`
/// and `payload` separately and flatten everything else into `extra`. Use
/// [`EventLine::payload`] to get the effective payload regardless of where
/// it landed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLine {
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,

    /// Anything else on the line (e.g. an inline payload whose keys sit
    /// directly on the envelope, or fields we don't model yet).
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

impl EventLine {
    /// The effective payload, regardless of where it was carried.
    ///
    /// Priority: `data`, then `payload`, then an object synthesized from the
    /// inline `extra` keys.
    pub fn payload(&self) -> Value {
        if let Some(d) = &self.data {
            return d.clone();
        }
        if let Some(p) = &self.payload {
            return p.clone();
        }
        if self.extra.is_empty() {
            Value::Null
        } else {
            Value::Object(self.extra.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
    }

    pub fn parsed_timestamp(&self) -> Option<DateTime<Utc>> {
        let ts = self.timestamp.as_ref()?;
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    /// Classify this line into a [`CopilotEvent`].
    pub fn event(&self) -> CopilotEvent {
        let p = self.payload();
        match self.kind.as_str() {
            EV_SESSION_START => CopilotEvent::SessionStart(SessionStart::from_payload(&p)),
            EV_SESSION_SHUTDOWN => CopilotEvent::SessionShutdown(SessionShutdown::from_payload(&p)),
            EV_SESSION_COMPACTION_COMPLETE => CopilotEvent::CompactionComplete(p),
            k if k.starts_with("session.") => CopilotEvent::SessionOther {
                kind: k.to_string(),
                payload: p,
            },
            EV_USER_MESSAGE => CopilotEvent::UserMessage(MessageEvent::from_payload(&p)),
            EV_ASSISTANT_TURN_START => CopilotEvent::AssistantTurnStart,
            EV_ASSISTANT_MESSAGE => CopilotEvent::AssistantMessage(MessageEvent::from_payload(&p)),
            EV_ASSISTANT_TURN_END => CopilotEvent::AssistantTurnEnd,
            EV_TOOL_EXECUTION_START => {
                CopilotEvent::ToolStart(ToolExecution::from_payload(&p))
            }
            EV_TOOL_EXECUTION_COMPLETE => {
                CopilotEvent::ToolComplete(ToolExecution::from_payload(&p))
            }
            EV_SUBAGENT_STARTED => CopilotEvent::SubagentStarted(Subagent::from_payload(&p)),
            EV_SUBAGENT_COMPLETED => CopilotEvent::SubagentCompleted(Subagent::from_payload(&p)),
            EV_SKILL_INVOKED => CopilotEvent::SkillInvoked(p),
            EV_HOOK_START | EV_HOOK_END => CopilotEvent::Hook {
                kind: self.kind.clone(),
                payload: p,
            },
            EV_ABORT => CopilotEvent::Abort(p),
            other => CopilotEvent::Unknown {
                kind: other.to_string(),
                payload: p,
            },
        }
    }
}

// ── Classified events ────────────────────────────────────────────────

/// A semantically classified `events.jsonl` line.
#[derive(Debug, Clone)]
pub enum CopilotEvent {
    SessionStart(SessionStart),
    SessionShutdown(SessionShutdown),
    CompactionComplete(Value),
    /// Any other `session.*` event we don't specifically model.
    SessionOther { kind: String, payload: Value },
    UserMessage(MessageEvent),
    AssistantTurnStart,
    AssistantMessage(MessageEvent),
    AssistantTurnEnd,
    ToolStart(ToolExecution),
    ToolComplete(ToolExecution),
    SubagentStarted(Subagent),
    SubagentCompleted(Subagent),
    SkillInvoked(Value),
    Hook { kind: String, payload: Value },
    Abort(Value),
    Unknown { kind: String, payload: Value },
}

/// `session.start` — the closest thing to a session-meta line.
#[derive(Debug, Clone, Default)]
pub struct SessionStart {
    pub version: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
}

impl SessionStart {
    fn from_payload(p: &Value) -> Self {
        Self {
            version: str_field(p, &["version", "cliVersion", "cli_version"]),
            cwd: str_field(p, &["cwd", "workingDirectory", "working_dir"]),
            model: str_field(p, &["model", "modelId", "model_id"]),
        }
    }
}

/// `session.shutdown` — carries per-session token/model metrics.
#[derive(Debug, Clone, Default)]
pub struct SessionShutdown {
    pub model: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

impl SessionShutdown {
    fn from_payload(p: &Value) -> Self {
        // `usage` may be nested; `modelMetrics` carries a model id.
        let usage = p.get("usage").unwrap_or(p);
        let model = p
            .get("modelMetrics")
            .and_then(|m| str_field(m, &["model", "modelId", "model_id"]))
            .or_else(|| str_field(p, &["model"]));
        Self {
            model,
            input_tokens: u32_field(usage, &["inputTokens", "input_tokens", "promptTokens"]),
            output_tokens: u32_field(usage, &["outputTokens", "output_tokens", "completionTokens"]),
            total_tokens: u32_field(usage, &["totalTokens", "total_tokens"]),
        }
    }
}

/// A `user.message` or `assistant.message`.
#[derive(Debug, Clone, Default)]
pub struct MessageEvent {
    pub text: String,
    pub model: Option<String>,
    pub id: Option<String>,
}

impl MessageEvent {
    fn from_payload(p: &Value) -> Self {
        Self {
            text: payload_text(p).unwrap_or_default(),
            model: str_field(p, &["model", "modelId", "model_id"]),
            id: str_field(p, &["id", "messageId", "message_id"]),
        }
    }
}

/// A `tool.execution_start` / `tool.execution_complete`.
#[derive(Debug, Clone, Default)]
pub struct ToolExecution {
    pub id: Option<String>,
    pub name: String,
    pub args: Value,
    /// Present on `..._complete`; `None` means "not reported".
    pub success: Option<bool>,
    /// Result/output text, when carried inline on `..._complete`.
    pub output: Option<String>,
}

impl ToolExecution {
    fn from_payload(p: &Value) -> Self {
        let args = p
            .get("args")
            .or_else(|| p.get("arguments"))
            .or_else(|| p.get("input"))
            .or_else(|| p.get("parameters"))
            .cloned()
            .unwrap_or(Value::Null);
        Self {
            id: str_field(p, &["id", "callId", "call_id", "toolCallId", "tool_call_id"]),
            name: str_field(p, &["name", "tool", "toolName", "tool_name"]).unwrap_or_default(),
            args,
            success: p
                .get("success")
                .and_then(|v| v.as_bool())
                .or_else(|| {
                    // `status: "success"` / `"error"` is a plausible alternative.
                    str_field(p, &["status"]).map(|s| {
                        let s = s.to_ascii_lowercase();
                        s == "success" || s == "ok" || s == "completed"
                    })
                }),
            output: payload_text_keys(
                p,
                &["output", "result", "stdout", "content", "aggregatedOutput", "aggregated_output"],
            ),
        }
    }
}

/// A `subagent.started` / `subagent.completed`.
#[derive(Debug, Clone, Default)]
pub struct Subagent {
    pub id: Option<String>,
    pub prompt: Option<String>,
    pub result: Option<String>,
}

impl Subagent {
    fn from_payload(p: &Value) -> Self {
        Self {
            id: str_field(p, &["id", "agentId", "agent_id", "subagentId", "subagent_id"]),
            prompt: payload_text_keys(p, &["prompt", "instruction", "task", "input"]),
            result: payload_text_keys(p, &["result", "output", "summary"]),
        }
    }
}

// ── Parsed session ───────────────────────────────────────────────────

/// A parsed Copilot session: the ordered event lines plus the directory id.
#[derive(Debug, Clone)]
pub struct Session {
    /// Session id (the `session-state/<id>/` directory name).
    pub id: String,
    /// Path to the session-state directory.
    pub dir_path: PathBuf,
    /// Ordered, parsed lines of `events.jsonl`.
    pub lines: Vec<EventLine>,
    /// Git/workspace context from the sibling `workspace.yaml`, if present.
    pub workspace: Option<Workspace>,
}

impl Session {
    pub fn events(&self) -> impl Iterator<Item = CopilotEvent> + '_ {
        self.lines.iter().map(|l| l.event())
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.lines.iter().find_map(|l| l.parsed_timestamp())
    }

    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.lines.iter().rev().find_map(|l| l.parsed_timestamp())
    }

    /// Model recorded on `session.start` (best-effort default model).
    pub fn start_model(&self) -> Option<String> {
        self.lines.iter().find_map(|l| match l.event() {
            CopilotEvent::SessionStart(s) => s.model,
            _ => None,
        })
    }

    /// Working directory from `session.start`.
    pub fn cwd(&self) -> Option<String> {
        self.lines.iter().find_map(|l| match l.event() {
            CopilotEvent::SessionStart(s) => s.cwd,
            _ => None,
        })
    }

    /// Producer version from `session.start`.
    pub fn version(&self) -> Option<String> {
        self.lines.iter().find_map(|l| match l.event() {
            CopilotEvent::SessionStart(s) => s.version,
            _ => None,
        })
    }

    /// First non-empty user-message text (for listing UIs).
    pub fn first_user_text(&self) -> Option<String> {
        self.lines.iter().find_map(|l| match l.event() {
            CopilotEvent::UserMessage(m) if !m.text.trim().is_empty() => Some(m.text),
            _ => None,
        })
    }
}

/// Git/workspace context parsed from a session's `workspace.yaml`.
///
/// ⚠️ `[reverse-eng, Medium]` — the exact field names and structure are
/// unverified (see `docs/agents/formats/copilot-cli/session-state.md`). Rather
/// than pull in a YAML dependency for a guessed schema, [`parse_workspace`]
/// does a tolerant line scan that matches known key spellings at any
/// indentation (so both a flat file and a one-level-nested `git:` block
/// resolve). Correct once verified against a real `workspace.yaml`.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    /// Absolute path to the repository root.
    pub git_root: Option<String>,
    /// Repository identifier (owner/name, or a remote URL).
    pub repository: Option<String>,
    /// Active branch at session time.
    pub branch: Option<String>,
    /// Commit hash, when recorded.
    pub revision: Option<String>,
}

impl Workspace {
    /// True when nothing useful was extracted.
    pub fn is_empty(&self) -> bool {
        self.git_root.is_none()
            && self.repository.is_none()
            && self.branch.is_none()
            && self.revision.is_none()
    }
}

/// Parse a (reverse-engineered) `workspace.yaml` body.
///
/// Tolerant by design: scans each line for a `key: value` pair, matching known
/// key spellings after trimming indentation (so nested blocks still resolve),
/// unquoting the value, and taking the first non-empty match per field.
pub fn parse_workspace(content: &str) -> Workspace {
    const ROOT_KEYS: &[&str] = &["git_root", "gitroot", "root", "worktree", "workspace_root"];
    const REPO_KEYS: &[&str] =
        &["repository", "repo", "remote", "origin", "repository_url", "remote_url"];
    const BRANCH_KEYS: &[&str] = &["branch", "git_branch", "current_branch"];
    const REV_KEYS: &[&str] = &["revision", "commit", "commit_hash", "sha", "head"];

    let mut ws = Workspace::default();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
        let value = unquote(value.trim());
        if value.is_empty() {
            continue; // block header like `git:` — no scalar to take
        }
        let slot = if ROOT_KEYS.contains(&key.as_str()) {
            &mut ws.git_root
        } else if REPO_KEYS.contains(&key.as_str()) {
            &mut ws.repository
        } else if BRANCH_KEYS.contains(&key.as_str()) {
            &mut ws.branch
        } else if REV_KEYS.contains(&key.as_str()) {
            &mut ws.revision
        } else {
            continue;
        };
        if slot.is_none() {
            *slot = Some(value.to_string());
        }
    }
    ws
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Lightweight metadata for a session, for listing without a full walk.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub id: String,
    pub dir_path: PathBuf,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    pub version: Option<String>,
    pub first_user_message: Option<String>,
    pub line_count: usize,
}

// ── Field-extraction helpers (tolerant of key-name variance) ─────────

/// First present string-valued key among `keys`.
fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// First present unsigned-int-valued key among `keys`.
fn u32_field(v: &Value, keys: &[&str]) -> Option<u32> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return Some(n as u32);
        }
    }
    None
}

/// Extract human text from a message-shaped payload.
///
/// Handles `text`, a string `content`, an array `content[].text`, and
/// `message`.
fn payload_text(v: &Value) -> Option<String> {
    payload_text_keys(v, &["text", "content", "message"])
}

/// Like [`payload_text`] but over an explicit key list; tolerates string,
/// array-of-parts, and object-with-text shapes.
fn payload_text_keys(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(s)) => {
                if !s.is_empty() {
                    return Some(s.clone());
                }
            }
            Some(Value::Array(parts)) => {
                let joined: String = parts
                    .iter()
                    .filter_map(|part| match part {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(_) => part
                            .get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
            Some(Value::Object(_)) => {
                if let Some(s) = v[*k].get("text").and_then(|t| t.as_str()) {
                    return Some(s.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(kind: &str, data: Value) -> EventLine {
        EventLine {
            kind: kind.to_string(),
            timestamp: Some("2026-06-30T10:00:00.000Z".to_string()),
            data: Some(data),
            payload: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn payload_prefers_data_then_payload_then_inline() {
        let l = EventLine {
            kind: "x".into(),
            timestamp: None,
            data: Some(json!({"a": 1})),
            payload: Some(json!({"b": 2})),
            extra: HashMap::new(),
        };
        assert_eq!(l.payload(), json!({"a": 1}));

        let l2 = EventLine {
            kind: "x".into(),
            timestamp: None,
            data: None,
            payload: Some(json!({"b": 2})),
            extra: HashMap::new(),
        };
        assert_eq!(l2.payload(), json!({"b": 2}));

        let mut extra = HashMap::new();
        extra.insert("c".to_string(), json!(3));
        let l3 = EventLine {
            kind: "x".into(),
            timestamp: None,
            data: None,
            payload: None,
            extra,
        };
        assert_eq!(l3.payload(), json!({"c": 3}));
    }

    #[test]
    fn classifies_known_events() {
        assert!(matches!(
            line(EV_USER_MESSAGE, json!({"text": "hi"})).event(),
            CopilotEvent::UserMessage(_)
        ));
        assert!(matches!(
            line(EV_TOOL_EXECUTION_START, json!({"name": "shell"})).event(),
            CopilotEvent::ToolStart(_)
        ));
        assert!(matches!(
            line(EV_ASSISTANT_TURN_END, json!({})).event(),
            CopilotEvent::AssistantTurnEnd
        ));
    }

    #[test]
    fn unknown_event_falls_through() {
        match line("brand.new_event", json!({"k": 1})).event() {
            CopilotEvent::Unknown { kind, .. } => assert_eq!(kind, "brand.new_event"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn session_other_for_unmodeled_session_events() {
        match line(EV_SESSION_MODEL_CHANGE, json!({"model": "x"})).event() {
            CopilotEvent::SessionOther { kind, .. } => assert_eq!(kind, EV_SESSION_MODEL_CHANGE),
            other => panic!("expected SessionOther, got {other:?}"),
        }
    }

    #[test]
    fn message_text_handles_string_array_and_object() {
        assert_eq!(
            MessageEvent::from_payload(&json!({"text": "plain"})).text,
            "plain"
        );
        assert_eq!(
            MessageEvent::from_payload(&json!({"content": [{"text": "a"}, {"text": "b"}]})).text,
            "ab"
        );
        assert_eq!(
            MessageEvent::from_payload(&json!({"content": "str"})).text,
            "str"
        );
    }

    #[test]
    fn tool_execution_extracts_args_and_success() {
        let t = ToolExecution::from_payload(&json!({
            "call_id": "c1", "name": "shell",
            "args": {"command": "ls"}, "success": true, "output": "a.rs"
        }));
        assert_eq!(t.id.as_deref(), Some("c1"));
        assert_eq!(t.name, "shell");
        assert_eq!(t.args, json!({"command": "ls"}));
        assert_eq!(t.success, Some(true));
        assert_eq!(t.output.as_deref(), Some("a.rs"));
    }

    #[test]
    fn tool_status_string_maps_to_success() {
        let ok = ToolExecution::from_payload(&json!({"name": "x", "status": "success"}));
        assert_eq!(ok.success, Some(true));
        let err = ToolExecution::from_payload(&json!({"name": "x", "status": "error"}));
        assert_eq!(err.success, Some(false));
    }

    #[test]
    fn shutdown_reads_nested_usage_camelcase() {
        let s = SessionShutdown::from_payload(&json!({
            "modelMetrics": {"model": "gpt-5-copilot"},
            "usage": {"inputTokens": 1200, "outputTokens": 340}
        }));
        assert_eq!(s.model.as_deref(), Some("gpt-5-copilot"));
        assert_eq!(s.input_tokens, Some(1200));
        assert_eq!(s.output_tokens, Some(340));
    }

    #[test]
    fn parse_workspace_flat() {
        let ws = parse_workspace(
            "git_root: /home/x/proj\nrepository: git@github.com:o/r.git\nbranch: main\n",
        );
        assert_eq!(ws.git_root.as_deref(), Some("/home/x/proj"));
        assert_eq!(ws.repository.as_deref(), Some("git@github.com:o/r.git"));
        assert_eq!(ws.branch.as_deref(), Some("main"));
        assert!(ws.revision.is_none());
        assert!(!ws.is_empty());
    }

    #[test]
    fn parse_workspace_tolerates_nesting_quotes_and_comments() {
        // A nested `git:` block header (no scalar) plus quoted, indented values.
        let ws = parse_workspace(
            "# session workspace\ngit:\n  root: \"/tmp/p\"\n  branch: 'feature/x'\n  commit: abc123\n",
        );
        assert_eq!(ws.git_root.as_deref(), Some("/tmp/p"));
        assert_eq!(ws.branch.as_deref(), Some("feature/x"));
        assert_eq!(ws.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_workspace_empty_when_no_known_keys() {
        assert!(parse_workspace("unrelated: value\nfoo: bar\n").is_empty());
        assert!(parse_workspace("").is_empty());
    }
}
