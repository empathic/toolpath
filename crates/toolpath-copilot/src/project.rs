//! Project a provider-agnostic [`ConversationView`] into a Copilot CLI
//! [`Session`] (an `events.jsonl` line stream + workspace metadata).
//!
//! This is the reverse of [`crate::provider::to_view`] and the basis for
//! `path p export copilot` and `path resume`. ⚠️ **Preview**: the emitted
//! `events.jsonl` matches the observed 1.0.67 shape, but whether the real
//! `copilot --resume` loads a *synthesized* session is unverified — see
//! `docs/agents/formats/copilot-cli/known-gaps-and-sourcing.md`.

use crate::provider::native_name;
use crate::types::{EventLine, Session, Workspace};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use toolpath_convo::{
    ConversationProjector, ConversationView, Result, Role, TokenUsage, ToolInvocation, Turn,
};

/// The default `copilotVersion` stamped on projected `session.start` events.
pub const DEFAULT_COPILOT_VERSION: &str = "1.0.67";

/// Projects a [`ConversationView`] into a Copilot [`Session`].
#[derive(Debug, Clone)]
pub struct CopilotProjector {
    pub copilot_version: String,
}

impl Default for CopilotProjector {
    fn default() -> Self {
        Self {
            copilot_version: DEFAULT_COPILOT_VERSION.to_string(),
        }
    }
}

impl CopilotProjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ConversationProjector for CopilotProjector {
    type Output = Session;

    fn project(&self, view: &ConversationView) -> Result<Session> {
        Ok(self.build(view))
    }
}

/// Accumulates `events.jsonl` lines, assigning each a UUID-ish `id` and
/// chaining `parentId` off the previous line.
struct LineBuilder {
    lines: Vec<EventLine>,
    seq: usize,
    last_id: Option<String>,
}

impl LineBuilder {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            seq: 0,
            last_id: None,
        }
    }

    fn push(&mut self, kind: &str, ts: &str, data: Value) {
        self.seq += 1;
        // Copilot's loader requires the envelope `id` (and `parentId`) to be a
        // UUID *string* — synthetic `e1`/`e2` ids are rejected as "invalid
        // session event envelope". Emit syntactically-valid, per-session-unique
        // v4-shaped UUIDs (deterministic; no rng/dep needed).
        let id = event_uuid(self.seq);
        let mut extra: HashMap<String, Value> = HashMap::new();
        extra.insert("id".to_string(), Value::String(id.clone()));
        if let Some(parent) = &self.last_id {
            extra.insert("parentId".to_string(), Value::String(parent.clone()));
        }
        self.lines.push(EventLine {
            kind: kind.to_string(),
            timestamp: (!ts.is_empty()).then(|| ts.to_string()),
            data: Some(data),
            payload: None,
            extra,
        });
        self.last_id = Some(id);
    }
}

impl CopilotProjector {
    fn build(&self, view: &ConversationView) -> Session {
        let mut b = LineBuilder::new();

        // Copilot's loader requires every event `timestamp` to be an ISO 8601
        // date-time WITH a timezone offset. Pick a base (first valid turn ts, or
        // the view's start) and normalize each event's timestamp against it.
        let base_ts = view
            .turns
            .iter()
            .map(|t| t.timestamp.as_str())
            .find(|s| is_iso_offset(s))
            .map(str::to_string)
            .or_else(|| view.started_at.map(|dt| dt.to_rfc3339()))
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_string());

        // session.start with git context from the view's base.
        b.push("session.start", &base_ts, self.session_start_data(view));

        for turn in &view.turns {
            let ts = iso_or(&turn.timestamp, &base_ts);
            match &turn.role {
                Role::User => b.push("user.message", &ts, json!({ "content": turn.text })),
                Role::System => b.push(
                    "system.message",
                    &ts,
                    json!({ "role": "system", "content": turn.text }),
                ),
                Role::Assistant => self.push_assistant(&mut b, turn, &ts),
                // Unknown/other roles (e.g. pi's `tool` role) fold into a user
                // message so the forward path reproduces them stably.
                Role::Other(_) => b.push("user.message", &ts, json!({ "content": turn.text })),
            }
        }

        Session {
            id: view.id.clone(),
            dir_path: std::path::PathBuf::from(&view.id),
            lines: b.lines,
            workspace: self.workspace(view),
        }
    }

    fn session_start_data(&self, view: &ConversationView) -> Value {
        let mut ctx = Map::new();
        if let Some(base) = &view.base {
            if let Some(wd) = &base.working_dir {
                let wd = strip_file_uri(wd);
                ctx.insert("cwd".into(), json!(wd));
                ctx.insert("gitRoot".into(), json!(wd));
            }
            if let Some(r) = &base.vcs_remote {
                ctx.insert("repository".into(), json!(r));
            }
            if let Some(br) = &base.vcs_branch {
                ctx.insert("branch".into(), json!(br));
            }
            if let Some(rev) = &base.vcs_revision {
                ctx.insert("headCommit".into(), json!(rev));
            }
        }
        let producer = view
            .producer
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "copilot-agent".to_string());
        json!({
            "sessionId": view.id,
            "version": 1,
            "producer": producer,
            "copilotVersion": self.copilot_version,
            "context": Value::Object(ctx),
        })
    }

    fn push_assistant(&self, b: &mut LineBuilder, turn: &Turn, ts: &str) {
        b.push("assistant.turn_start", ts, json!({}));

        // assistant.message carries text, model, reasoning, tokens, and the
        // tool-request mirror.
        let mut data = Map::new();
        data.insert("content".into(), json!(turn.text));
        if let Some(m) = &turn.model {
            data.insert("model".into(), json!(m));
        }
        if let Some(th) = &turn.thinking {
            data.insert("reasoningText".into(), json!(th));
        }
        if let Some(u) = &turn.token_usage {
            insert_token_fields(&mut data, u);
        }
        if !turn.tool_uses.is_empty() {
            let reqs: Vec<Value> = turn
                .tool_uses
                .iter()
                .map(|tu| {
                    json!({
                        "toolCallId": tu.id,
                        "name": tool_name(tu),
                        "arguments": tu.input,
                    })
                })
                .collect();
            data.insert("toolRequests".into(), Value::Array(reqs));
        }
        b.push("assistant.message", ts, Value::Object(data));

        // Tool execution lifecycle.
        for tu in &turn.tool_uses {
            b.push(
                "tool.execution_start",
                ts,
                json!({
                    "toolCallId": tu.id,
                    "toolName": tool_name(tu),
                    "arguments": tu.input,
                }),
            );
            if let Some(res) = &tu.result {
                b.push(
                    "tool.execution_complete",
                    ts,
                    json!({
                        "toolCallId": tu.id,
                        "success": !res.is_error,
                        "result": { "content": res.content },
                    }),
                );
            }
        }

        // Sub-agent delegations.
        for d in &turn.delegations {
            b.push(
                "subagent.started",
                ts,
                json!({ "id": d.agent_id, "prompt": d.prompt }),
            );
            if let Some(result) = &d.result {
                b.push(
                    "subagent.completed",
                    ts,
                    json!({ "id": d.agent_id, "result": result }),
                );
            }
        }

        b.push("assistant.turn_end", ts, json!({}));
    }

    fn workspace(&self, view: &ConversationView) -> Option<Workspace> {
        let base = view.base.as_ref()?;
        let ws = Workspace {
            git_root: base.working_dir.as_deref().map(strip_file_uri),
            repository: base.vcs_remote.clone(),
            branch: base.vcs_branch.clone(),
            revision: base.vcs_revision.clone(),
        };
        (!ws.is_empty()).then_some(ws)
    }
}

fn tool_name(tu: &ToolInvocation) -> String {
    match tu.category {
        Some(cat) => native_name(cat, &tu.input).to_string(),
        None => tu.name.clone(),
    }
}

fn insert_token_fields(data: &mut Map<String, Value>, u: &TokenUsage) {
    if let Some(o) = u.output_tokens {
        data.insert("outputTokens".into(), json!(o));
    }
    if let Some(i) = u.input_tokens {
        data.insert("inputTokens".into(), json!(i));
    }
    if let Some(c) = u.cache_read_tokens {
        data.insert("cacheReadTokens".into(), json!(c));
    }
    if let Some(c) = u.cache_write_tokens {
        data.insert("cacheWriteTokens".into(), json!(c));
    }
}

fn strip_file_uri(s: &str) -> String {
    s.strip_prefix("file://").unwrap_or(s).to_string()
}

/// A syntactically-valid, per-session-unique, v4-shaped UUID for event `n`.
/// Copilot's loader validates the envelope `id`/`parentId` shape (UUID string),
/// not the version or randomness, so a deterministic value is fine and keeps the
/// projector output reproducible.
fn event_uuid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{:012x}", n)
}

/// True when `s` is an ISO 8601 / RFC 3339 date-time WITH a timezone offset
/// (what Copilot's loader requires on every event `timestamp`).
fn is_iso_offset(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// `s` if it's a valid offset-bearing ISO timestamp, else `fallback`.
fn iso_or(s: &str, fallback: &str) -> String {
    if is_iso_offset(s) {
        s.to_string()
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::to_view;

    #[test]
    fn round_trips_a_view() {
        // Build a view via the forward path, project it back, forward again,
        // and assert the round-trip is a fixed point on the salient fields.
        let body = [
            r#"{"type":"session.start","timestamp":"2026-07-01T00:00:00Z","data":{"copilotVersion":"1.0.67","context":{"cwd":"/tmp/proj","gitRoot":"/tmp/proj","repository":"o/r","branch":"main","headCommit":"abc"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-07-01T00:00:01Z","data":{"content":"build it"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-07-01T00:00:02Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-07-01T00:00:03Z","data":{"content":"listing","model":"claude-haiku-4.5","reasoningText":"think","outputTokens":42}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-07-01T00:00:04Z","data":{"toolCallId":"c1","toolName":"bash","arguments":{"command":"ls"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-07-01T00:00:05Z","data":{"toolCallId":"c1","success":true,"result":{"content":"a.rs"}}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-07-01T00:00:06Z","data":{}}"#,
        ]
        .join("\n");
        let session = crate::Session {
            id: "s1".into(),
            dir_path: "/tmp/s1".into(),
            lines: body
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            workspace: None,
        };
        let view1 = to_view(&session);

        let projected = CopilotProjector::new().project(&view1).unwrap();
        let view2 = to_view(&projected);

        // Turns, roles, text.
        assert_eq!(view1.turns.len(), view2.turns.len());
        assert_eq!(view2.turns[0].role, Role::User);
        assert_eq!(view2.turns[0].text, "build it");
        assert_eq!(view2.turns[1].role, Role::Assistant);
        assert_eq!(view2.turns[1].text, "listing");
        // Thinking + model + per-turn tokens survive.
        assert_eq!(view2.turns[1].thinking.as_deref(), Some("think"));
        assert_eq!(view2.turns[1].model.as_deref(), Some("claude-haiku-4.5"));
        assert_eq!(
            view2.turns[1].token_usage.as_ref().unwrap().output_tokens,
            Some(42)
        );
        // Tool call + result.
        let tu = &view2.turns[1].tool_uses[0];
        assert_eq!(tu.id, "c1");
        assert_eq!(tu.name, "bash");
        assert_eq!(tu.result.as_ref().unwrap().content, "a.rs");
        // Base git context survives via session.start context.
        let base = view2.base.as_ref().unwrap();
        assert_eq!(base.working_dir.as_deref(), Some("/tmp/proj"));
        assert_eq!(base.vcs_branch.as_deref(), Some("main"));
        assert_eq!(base.vcs_revision.as_deref(), Some("abc"));
        // total_usage survives.
        assert_eq!(view2.total_usage.as_ref().unwrap().output_tokens, Some(42));
    }

    #[test]
    fn event_ids_are_uuid_shaped() {
        // Regression: Copilot's loader rejects non-UUID envelope ids
        // ("invalid session event envelope: `id` must be a UUID string").
        fn uuid_shaped(s: &str) -> bool {
            let b = s.as_bytes();
            s.len() == 36
                && b[8] == b'-'
                && b[13] == b'-'
                && b[18] == b'-'
                && b[23] == b'-'
                && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        }
        let session = crate::Session {
            id: "s".into(),
            dir_path: "/tmp/s".into(),
            lines: [
                r#"{"type":"user.message","data":{"content":"hi"}}"#,
                r#"{"type":"assistant.turn_start","data":{}}"#,
                r#"{"type":"assistant.message","data":{"content":"ok"}}"#,
                r#"{"type":"assistant.turn_end","data":{}}"#,
            ]
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
            workspace: None,
        };
        let view = to_view(&session);
        let projected = CopilotProjector::new().project(&view).unwrap();
        assert!(projected.lines.len() >= 2);
        for line in &projected.lines {
            let id = line.extra.get("id").and_then(|v| v.as_str()).unwrap();
            assert!(uuid_shaped(id), "event id not UUID-shaped: {id:?}");
            if let Some(p) = line.extra.get("parentId").and_then(|v| v.as_str()) {
                assert!(uuid_shaped(p), "parentId not UUID-shaped: {p:?}");
            }
        }
    }

    #[test]
    fn remaps_foreign_tool_names() {
        use serde_json::json;
        use toolpath_convo::{ToolCategory, ToolInvocation, ToolResult};
        // A codex-style `shell` call should project to copilot's `bash`.
        let mut view = ConversationView {
            id: "x".into(),
            provider_id: Some("codex".into()),
            ..Default::default()
        };
        view.turns.push(Turn {
            id: "a1".into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-07-01T00:00:00Z".into(),
            text: String::new(),
            thinking: None,
            tool_uses: vec![ToolInvocation {
                id: "c1".into(),
                name: "shell".into(),
                input: json!({"command": "ls"}),
                result: Some(ToolResult {
                    content: "out".into(),
                    is_error: false,
                }),
                category: Some(ToolCategory::Shell),
            }],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        });
        let projected = CopilotProjector::new().project(&view).unwrap();
        let back = to_view(&projected);
        assert_eq!(back.turns[0].tool_uses[0].name, "bash");
    }
}
