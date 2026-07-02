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
        // Copilot requires `parentId` to be *present* — a UUID string, or
        // explicitly `null` for the root event. Omitting it is rejected.
        extra.insert(
            "parentId".to_string(),
            match &self.last_id {
                Some(parent) => Value::String(parent.clone()),
                None => Value::Null,
            },
        );
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
        b.push(
            "session.start",
            &base_ts,
            self.session_start_data(view, &base_ts),
        );

        let mut assistant_turn: usize = 0;
        for turn in &view.turns {
            let ts = iso_or(&turn.timestamp, &base_ts);
            match &turn.role {
                Role::User => b.push("user.message", &ts, json!({ "content": turn.text })),
                Role::System => b.push(
                    "system.message",
                    &ts,
                    json!({ "role": "system", "content": turn.text }),
                ),
                Role::Assistant => {
                    let turn_id = assistant_turn.to_string();
                    let message_id = message_uuid(assistant_turn);
                    assistant_turn += 1;
                    self.push_assistant(&mut b, turn, &ts, &turn_id, &message_id);
                }
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

    fn session_start_data(&self, view: &ConversationView, start_time: &str) -> Value {
        let mut ctx = Map::new();
        if let Some(base) = &view.base {
            if let Some(wd) = &base.working_dir {
                let wd = strip_file_uri(wd);
                ctx.insert("cwd".into(), json!(wd));
                ctx.insert("gitRoot".into(), json!(wd));
            }
            if let Some(r) = &base.vcs_remote {
                ctx.insert("repository".into(), json!(r));
                ctx.insert("hostType".into(), json!("github"));
                ctx.insert("repositoryHost".into(), json!("github.com"));
            }
            if let Some(br) = &base.vcs_branch {
                ctx.insert("branch".into(), json!(br));
            }
            if let Some(rev) = &base.vcs_revision {
                ctx.insert("headCommit".into(), json!(rev));
                ctx.insert("baseCommit".into(), json!(rev));
            }
        }
        let producer = view
            .producer
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "copilot-agent".to_string());
        // Mirror the observed 1.0.67 session.start top-level shape. The loader
        // validates required fields one at a time (`startTime` was the first
        // caught); emitting the full observed set avoids repeat rejections.
        json!({
            "sessionId": view.id,
            "version": 1,
            "producer": producer,
            "copilotVersion": self.copilot_version,
            "startTime": start_time,
            "contextTier": Value::Null,
            "context": Value::Object(ctx),
            "alreadyInUse": false,
            "remoteSteerable": false,
        })
    }

    fn push_assistant(
        &self,
        b: &mut LineBuilder,
        turn: &Turn,
        ts: &str,
        turn_id: &str,
        message_id: &str,
    ) {
        // Copilot requires `turnId` on turn-scoped events (assistant messages
        // and tool executions); stamp it on every event this turn emits.
        b.push("assistant.turn_start", ts, json!({ "turnId": turn_id }));

        // assistant.message carries text, model, reasoning, tokens, and the
        // tool-request mirror.
        let mut data = Map::new();
        data.insert("content".into(), json!(turn.text));
        data.insert("turnId".into(), json!(turn_id));
        data.insert("messageId".into(), json!(message_id));
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
            // The timeline UI builds its tool row from THIS mirror (name +
            // arguments), not from tool.execution_start — the editor row
            // (title "Edit <path>", +N/−M counts, colorized diff body) only
            // engages when `arguments.path` is present. So the mirror must
            // carry the same remapped arguments as the execution events.
            let reqs: Vec<Value> = turn
                .tool_uses
                .iter()
                .enumerate()
                .map(|(i, tu)| {
                    let (name, args) = projected_tool(tu);
                    json!({
                        "toolCallId": call_id(tu, turn_id, i),
                        "name": name,
                        "arguments": args,
                    })
                })
                .collect();
            data.insert("toolRequests".into(), Value::Array(reqs));
        }
        b.push("assistant.message", ts, Value::Object(data));

        // Tool execution lifecycle. `call_id` guarantees a non-empty toolCallId
        // (Copilot rejects an empty one as missing) and is stable across the
        // request mirror, start, and complete for the same call.
        for (i, tu) in turn.tool_uses.iter().enumerate() {
            let cid = call_id(tu, turn_id, i);
            if let Some(fw) = file_write_projection(tu) {
                // Copilot-native edit/create with a git-diff result so the
                // change renders. Always emit the complete (the diff is the
                // point), even when the IR carried no result.
                let success = tu.result.as_ref().map(|r| !r.is_error).unwrap_or(true);
                b.push(
                    "tool.execution_start",
                    ts,
                    json!({
                        "toolCallId": cid,
                        "toolName": fw.tool_name,
                        "arguments": fw.arguments,
                        "turnId": turn_id,
                    }),
                );
                b.push(
                    "tool.execution_complete",
                    ts,
                    json!({
                        "toolCallId": cid,
                        "success": success,
                        "result": match &fw.detailed {
                            Some(d) => json!({ "content": fw.content, "detailedContent": d }),
                            None => json!({ "content": fw.content }),
                        },
                        "toolTelemetry": fw.telemetry,
                        "turnId": turn_id,
                    }),
                );
                continue;
            }
            let (g_name, g_args) = projected_tool(tu);
            b.push(
                "tool.execution_start",
                ts,
                json!({
                    "toolCallId": cid,
                    "toolName": g_name,
                    "arguments": g_args,
                    "turnId": turn_id,
                }),
            );
            if let Some(res) = &tu.result {
                b.push(
                    "tool.execution_complete",
                    ts,
                    json!({
                        "toolCallId": cid,
                        "success": !res.is_error,
                        "result": { "content": res.content },
                        "turnId": turn_id,
                    }),
                );
            }
        }

        // Sub-agent delegations. A sub-agent is dispatched via a tool call, so
        // Copilot requires a (non-empty) `toolCallId` on `subagent.*` too;
        // synthesize one, stable across started/completed for the delegation.
        for (i, d) in turn.delegations.iter().enumerate() {
            let sub_call = format!("toolcall-sub-{turn_id}-{i}");
            let agent_name = if d.agent_id.trim().is_empty() {
                "subagent"
            } else {
                d.agent_id.as_str()
            };
            b.push(
                "subagent.started",
                ts,
                json!({
                    "id": d.agent_id,
                    "agentName": agent_name,
                    "agentDisplayName": agent_name,
                    "agentDescription": "Delegated sub-agent task",
                    "prompt": d.prompt,
                    "turnId": turn_id,
                    "toolCallId": sub_call,
                }),
            );
            if let Some(result) = &d.result {
                b.push(
                    "subagent.completed",
                    ts,
                    json!({
                        "id": d.agent_id,
                        "agentName": agent_name,
                    "agentDisplayName": agent_name,
                    "agentDescription": "Delegated sub-agent task",
                        "result": result,
                        "turnId": turn_id,
                        "toolCallId": sub_call,
                    }),
                );
            }
        }

        b.push(
            "assistant.turn_end",
            ts,
            json!({ "turnId": turn_id, "messageId": message_id }),
        );
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

/// A non-empty, stable toolCallId for the `i`-th tool call of a turn. Uses the
/// invocation's own id when set; synthesizes one otherwise (Copilot rejects an
/// empty `toolCallId` as a missing field).
fn call_id(tu: &ToolInvocation, turn_id: &str, i: usize) -> String {
    if tu.id.trim().is_empty() {
        format!("toolcall-{turn_id}-{i}")
    } else {
        tu.id.clone()
    }
}

fn tool_name(tu: &ToolInvocation) -> String {
    match tu.category {
        Some(cat) => native_name(cat, &tu.input).to_string(),
        None => tu.name.clone(),
    }
}

/// The (name, arguments) pair to emit for a tool call — Copilot's timeline UI
/// keys its per-tool row rendering off these (e.g. `view`/`edit`/`create` rows
/// read `arguments.path`), so foreign arg names must be remapped alongside the
/// tool name. Used by BOTH the `assistant.message.toolRequests` mirror and
/// `tool.execution_start`, which must agree.
fn projected_tool(tu: &ToolInvocation) -> (String, Value) {
    if let Some(fw) = file_write_projection(tu) {
        return (fw.tool_name.to_string(), fw.arguments);
    }
    if tu.category == Some(toolpath_convo::ToolCategory::FileRead)
        && let Some(path) = str_in(&tu.input, &["path", "file_path", "filePath", "file"])
    {
        let mut args = Map::new();
        args.insert("path".into(), json!(path));
        // Claude's Read offset/limit ≈ Copilot view's view_range.
        let off = tu.input.get("offset").and_then(|v| v.as_i64());
        let lim = tu.input.get("limit").and_then(|v| v.as_i64());
        if let (Some(o), Some(l)) = (off, lim) {
            args.insert("view_range".into(), json!([o, o + l - 1]));
        }
        return ("view".to_string(), Value::Object(args));
    }
    (tool_name(tu), tu.input.clone())
}

/// Copilot-native shape for a file-write tool call, so its diff renders. The
/// diff lives in `result.detailedContent` (a git-style unified diff) — that's
/// what Copilot's UI displays. `edit` = partial replace (`old_str`/`new_str`);
/// `create` = new file (`file_text`).
struct FileWrite {
    tool_name: &'static str,
    arguments: Value,
    content: String,
    /// `None` when the diff has no `@@` hunk — a headerless/hunkless
    /// `detailedContent` renders as raw text in Copilot's diff view, so we
    /// omit it entirely rather than leak headers.
    detailed: Option<String>,
    /// `toolTelemetry` — declares the diff's file extension / language / line
    /// counts; Copilot's UI uses it to render a *colorized* diff view (without
    /// it the diff shows as flat text).
    telemetry: Value,
}

fn str_in(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn file_write_projection(tu: &ToolInvocation) -> Option<FileWrite> {
    if tu.category != Some(toolpath_convo::ToolCategory::FileWrite) {
        return None;
    }
    let input = &tu.input;
    let path = str_in(input, &["path", "file_path", "filePath", "filename", "file", "target_file"])?;
    if input.get("old_string").is_some() || input.get("old_str").is_some() {
        let old = str_in(input, &["old_str", "old_string", "oldString"]).unwrap_or_default();
        let new = str_in(input, &["new_str", "new_string", "newString"]).unwrap_or_default();
        let detailed = hunked(git_diff(&path, &old, &new, false));
        let telemetry = file_telemetry("edit", &path, detailed.as_deref().unwrap_or(""));
        return Some(FileWrite {
            tool_name: "edit",
            arguments: json!({ "path": path, "old_str": old, "new_str": new }),
            content: format!("File {path} updated with changes."),
            detailed,
            telemetry,
        });
    }
    let content = str_in(input, &["file_text", "content", "contents", "text", "new_str", "new_string"])?;
    let detailed = hunked(git_diff(&path, "", &content, true));
    let telemetry = file_telemetry("create", &path, detailed.as_deref().unwrap_or(""));
    Some(FileWrite {
        tool_name: "create",
        arguments: json!({ "path": path, "file_text": content }),
        content: format!("Created file {path} with {} characters", content.chars().count()),
        detailed,
        telemetry,
    })
}

/// Build the `toolTelemetry` object Copilot's UI reads to render a colorized
/// diff. Matches the observed 1.0.67 shape: `properties` values are
/// *stringified* JSON, plus `metrics` and `restrictedProperties`.
fn file_telemetry(command: &str, path: &str, diff: &str) -> Value {
    let ext = path
        .rsplit_once('.')
        .filter(|(_, e)| !e.contains('/'))
        .map(|(_, e)| format!(".{e}"))
        .unwrap_or_default();
    let lang = language_id(&ext);
    let (added, removed) = diff_line_counts(diff);
    let s = |v: Value| serde_json::to_string(&v).unwrap_or_default();
    json!({
        "properties": {
            "command": command,
            "resolvedPathAgainstCwd": "false",
            "fileExtension": s(json!([ext])),
            "codeBlocks": s(json!([{
                "fileExt": ext, "languageId": lang,
                "linesAdded": added, "linesRemoved": removed
            }])),
            "languageId": s(json!([lang])),
        },
        "metrics": { "linesAdded": added, "linesRemoved": removed },
        "restrictedProperties": { "filePaths": s(json!([path])) },
    })
}

/// Count added/removed lines in a unified diff (excluding the `+++`/`---` header).
fn diff_line_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            added += 1;
        } else if l.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

fn language_id(ext: &str) -> &'static str {
    match ext {
        ".rs" => "rust",
        ".md" | ".markdown" => "markdown",
        ".txt" => "text",
        ".py" => "python",
        ".js" | ".mjs" | ".cjs" => "javascript",
        ".ts" => "typescript",
        ".tsx" => "typescriptreact",
        ".jsx" => "javascriptreact",
        ".json" => "json",
        ".jsonl" => "jsonl",
        ".toml" => "toml",
        ".yaml" | ".yml" => "yaml",
        ".sh" | ".bash" => "shellscript",
        ".go" => "go",
        ".c" => "c",
        ".cpp" | ".cc" | ".cxx" | ".h" | ".hpp" => "cpp",
        ".java" => "java",
        ".rb" => "ruby",
        ".php" => "php",
        ".html" | ".htm" => "html",
        ".css" => "css",
        ".scss" => "scss",
        ".sql" => "sql",
        ".xml" => "xml",
        _ => "plaintext",
    }
}

/// A git-style unified diff matching Copilot's `result.detailedContent`.
///
/// Built with `similar` directly so the header appears exactly once — going
/// through `toolpath_convo::unified_diff` double-headers it (its own `a/<path>`
/// header plus `similar`'s empty-filename one), which Copilot can't parse into a
/// colorized diff. Tool args carry absolute paths; git drops the leading slash.
fn git_diff(path: &str, before: &str, after: &str, create: bool) -> String {
    let p = path.strip_prefix('/').unwrap_or(path);
    let from = if create {
        "a/dev/null".to_string()
    } else {
        format!("a/{p}")
    };
    let to = format!("b/{p}");
    // A diff with headers but no `@@` hunk makes Copilot's diff view fall back
    // to rendering the header lines as raw text (observed with an empty-file
    // create: similar emits nothing for ""→""). Match the native tool, which
    // renders an empty created file as one added empty line.
    let after_eff = if create && after.is_empty() && before.is_empty() {
        "\n"
    } else {
        after
    };
    let diff = similar::TextDiff::from_lines(before, after_eff);
    let body = diff
        .unified_diff()
        .context_radius(3)
        .header(&from, &to)
        .to_string();
    if create {
        format!("\ndiff --git a/{p} b/{p}\ncreate file mode 100644\nindex 0000000..0000000\n{body}")
    } else {
        format!("\ndiff --git a/{p} b/{p}\nindex 0000000..0000000 100644\n{body}")
    }
}

/// Keep a diff only if it actually has a hunk to render.
fn hunked(diff: String) -> Option<String> {
    diff.lines().any(|l| l.starts_with("@@")).then_some(diff)
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

/// A stable UUID for assistant message `n` — a distinct namespace from
/// [`event_uuid`] so message ids and event-envelope ids never collide.
fn message_uuid(n: usize) -> String {
    format!("00000000-0000-4000-8001-{:012x}", n)
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
    fn empty_tool_id_gets_a_synthesized_call_id() {
        // Copilot rejects an empty `toolCallId` as a missing field; the
        // projector must synthesize a non-empty, consistent id.
        use serde_json::json;
        use toolpath_convo::{ToolCategory, ToolInvocation, ToolResult};
        let mut view = ConversationView {
            id: "x".into(),
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
                id: String::new(), // empty!
                name: "bash".into(),
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
        let session = CopilotProjector::new().project(&view).unwrap();
        // Collect toolCallId from the start + complete + the message's request.
        let mut ids: Vec<String> = Vec::new();
        for line in &session.lines {
            let d = line.data.as_ref().unwrap();
            if line.kind.starts_with("tool.execution") {
                ids.push(d["toolCallId"].as_str().unwrap().to_string());
            }
            if line.kind == "assistant.message"
                && let Some(reqs) = d.get("toolRequests").and_then(|v| v.as_array())
            {
                for r in reqs {
                    ids.push(r["toolCallId"].as_str().unwrap().to_string());
                }
            }
        }
        assert!(!ids.is_empty());
        assert!(ids.iter().all(|s| !s.is_empty()), "no empty toolCallId");
        // start, complete, and the request mirror all share one id.
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "call id must be stable: {ids:?}");
    }

    #[test]
    fn file_edits_project_to_copilot_edit_shape_with_diff() {
        // Copilot renders a file change from `result.detailedContent` (a
        // git-style diff) on an `edit`/`create` tool; a Claude Edit/Write must
        // map to that shape.
        use serde_json::json;
        use toolpath_convo::{ToolCategory, ToolInvocation, ToolResult};
        fn assistant_with(tool: ToolInvocation) -> ConversationView {
            let mut v = ConversationView {
                id: "x".into(),
                ..Default::default()
            };
            v.turns.push(Turn {
                id: "a1".into(),
                parent_id: None,
                group_id: None,
                role: Role::Assistant,
                timestamp: "2026-07-01T00:00:00Z".into(),
                text: String::new(),
                thinking: None,
                tool_uses: vec![tool],
                model: None,
                stop_reason: None,
                token_usage: None,
                attributed_token_usage: None,
                environment: None,
                delegations: Vec::new(),
                file_mutations: Vec::new(),
            });
            v
        }
        let base = |id: &str, name: &str, input| ToolInvocation {
            id: id.into(),
            name: name.into(),
            input,
            result: Some(ToolResult {
                content: "done".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileWrite),
        };
        let find = |lines: &[EventLine], kind: &str, kv: &str| -> serde_json::Value {
            lines
                .iter()
                .find(|l| l.kind == kind && l.data.as_ref().unwrap().get("toolName").and_then(|v| v.as_str()) == Some(kv))
                .map(|l| l.data.clone().unwrap())
                .unwrap_or(json!(null))
        };

        // Claude Edit -> copilot `edit` with {path, old_str, new_str} + diff.
        let edit = base(
            "c1",
            "Edit",
            json!({"file_path": "/p/a.rs", "old_string": "old", "new_string": "new"}),
        );
        let s = CopilotProjector::new().project(&assistant_with(edit)).unwrap();
        let start = find(&s.lines, "tool.execution_start", "edit");
        assert_eq!(start["arguments"]["path"], "/p/a.rs");
        assert_eq!(start["arguments"]["old_str"], "old");
        assert_eq!(start["arguments"]["new_str"], "new");
        let done = s.lines.iter().find(|l| l.kind == "tool.execution_complete").unwrap();
        let detailed = done.data.as_ref().unwrap()["result"]["detailedContent"].as_str().unwrap();
        assert!(detailed.contains("diff --git a/p/a.rs b/p/a.rs"), "got: {detailed}");
        assert!(detailed.contains("-old") && detailed.contains("+new"));
        // Exactly one file header — no stray empty `--- `/`+++ ` lines (which
        // break Copilot's diff parser and lose colorization).
        assert!(
            !detailed.contains("\n--- \n") && !detailed.contains("\n+++ \n"),
            "duplicate/empty diff header: {detailed:?}"
        );
        assert_eq!(detailed.matches("\n--- a/").count(), 1, "one --- header: {detailed:?}");
        // toolTelemetry drives the colorized diff view.
        let tele = &done.data.as_ref().unwrap()["toolTelemetry"];
        assert_eq!(tele["metrics"]["linesAdded"], 1);
        assert_eq!(tele["metrics"]["linesRemoved"], 1);
        assert!(
            tele["properties"]["codeBlocks"].as_str().unwrap().contains("\"languageId\":\"rust\""),
            "codeBlocks: {tele:?}"
        );

        // Claude Write -> copilot `create` with {path, file_text} + create diff.
        let create = base("c2", "Write", json!({"file_path": "/p/b.rs", "content": "hello"}));
        let s = CopilotProjector::new().project(&assistant_with(create)).unwrap();
        let start = find(&s.lines, "tool.execution_start", "create");
        assert_eq!(start["arguments"]["file_text"], "hello");
        let done = s.lines.iter().find(|l| l.kind == "tool.execution_complete").unwrap();
        let detailed = done.data.as_ref().unwrap()["result"]["detailedContent"].as_str().unwrap();
        assert!(detailed.contains("create file mode"), "got: {detailed}");
        assert!(detailed.contains("--- a/dev/null") && detailed.contains("+hello"));
    }

    #[test]
    fn tool_requests_mirror_carries_remapped_args() {
        // Copilot's timeline UI builds tool rows from the assistant.message
        // toolRequests mirror — the editor row (title, +N/−M, colorized diff)
        // only engages when `arguments.path` is present. A mirror still
        // carrying Claude's `file_path`/`old_string` renders the generic row
        // with a flat markdown diff instead.
        use serde_json::json;
        use toolpath_convo::{ToolCategory, ToolInvocation, ToolResult};
        let mk = |name: &str, cat, input| ToolInvocation {
            id: format!("c-{name}"),
            name: name.into(),
            input,
            result: Some(ToolResult {
                content: "ok".into(),
                is_error: false,
            }),
            category: Some(cat),
        };
        let mut view = ConversationView {
            id: "x".into(),
            ..Default::default()
        };
        view.turns.push(Turn {
            id: "a1".into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-07-01T00:00:00Z".into(),
            text: "t".into(),
            thinking: None,
            tool_uses: vec![
                mk(
                    "Edit",
                    ToolCategory::FileWrite,
                    json!({"file_path": "/p/a.md", "old_string": "x", "new_string": "y"}),
                ),
                mk(
                    "Read",
                    ToolCategory::FileRead,
                    json!({"file_path": "/p/b.rs", "offset": 10, "limit": 5}),
                ),
            ],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        });
        let session = CopilotProjector::new().project(&view).unwrap();
        let msg = session
            .lines
            .iter()
            .find(|l| l.kind == "assistant.message")
            .unwrap();
        let reqs = msg.data.as_ref().unwrap()["toolRequests"].as_array().unwrap();
        // Edit mirror: copilot arg names, no Claude leftovers.
        assert_eq!(reqs[0]["name"], "edit");
        assert_eq!(reqs[0]["arguments"]["path"], "/p/a.md");
        assert!(reqs[0]["arguments"].get("file_path").is_none());
        assert_eq!(reqs[0]["arguments"]["old_str"], "x");
        // Read mirror: view with path + view_range from offset/limit.
        assert_eq!(reqs[1]["name"], "view");
        assert_eq!(reqs[1]["arguments"]["path"], "/p/b.rs");
        assert_eq!(reqs[1]["arguments"]["view_range"], json!([10, 14]));
        // execution_start agrees with the mirror.
        let start = session
            .lines
            .iter()
            .find(|l| {
                l.kind == "tool.execution_start"
                    && l.data.as_ref().unwrap()["toolName"] == "view"
            })
            .unwrap();
        assert_eq!(start.data.as_ref().unwrap()["arguments"]["path"], "/p/b.rs");
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
