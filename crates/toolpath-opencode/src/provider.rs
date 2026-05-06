//! Implementation of `toolpath-convo` traits for opencode sessions.
//!
//! Unlike Codex's streaming event model, opencode's parts are
//! self-contained: each [`crate::types::ToolPart`] already carries both
//! the tool input and the tool output/error in its [`ToolState`]. So the
//! mapping is mostly a direct translation per part, with minimal
//! cross-part assembly:
//!
//! 1. For each user [`Message`], emit a [`Turn`] with `role: User`
//!    whose `text` is the concatenation of the message's text parts.
//! 2. For each assistant [`Message`], emit a [`Turn`] with
//!    `role: Assistant`:
//!    - `text` ← concatenation of its `text` parts.
//!    - `thinking` ← concatenation of its `reasoning` parts.
//!    - `tool_uses` ← one [`ToolInvocation`] per `tool` part.
//!    - `token_usage` ← summed across all `step-finish` parts (each
//!      is a per-step delta). Falls back to the message-level
//!      `tokens` field if no step-finish parts exist.
//!    - `extra["opencode"]["snapshots"]` ← ordered list of snapshot
//!      SHAs from `step-start`/`step-finish`/`snapshot` parts, used
//!      by the derive layer to fetch file diffs.
//!    - `extra["opencode"]["patches"]` ← any `patch` parts (their
//!      `{hash, files}` records).
//! 3. Non-turn parts land in `ConversationView.events`:
//!    `compaction`, `retry`, unknown types.
//! 4. `subtask` parts are captured on the turn's `delegations`
//!    (empty-turn list — the sub-agent's own session lives under
//!    its own id, linked by `session.parent_id`).

use chrono::{TimeZone, Utc};
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::error::Result;
use crate::io::ConvoIO;
use crate::paths::PathResolver;
use crate::types::{
    AssistantMessage, Message, MessageData, Part, PartData, Session, SessionMetadata, Tokens,
    ToolState, UserMessage,
};
use toolpath_convo::{
    ConversationEvent, ConversationMeta, ConversationProvider, ConversationView,
    ConvoError as ConvoTraitError, DelegatedWork, EnvironmentSnapshot, Role, TokenUsage,
    ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Provider for opencode sessions.
#[derive(Default)]
pub struct OpencodeConvo {
    io: ConvoIO,
}

impl OpencodeConvo {
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        self.io.read_session(session_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        self.io.list_session_metadata(None)
    }

    pub fn most_recent_session(&self) -> Result<Option<Session>> {
        let metas = self.list_sessions()?;
        match metas.first() {
            Some(m) => Ok(Some(self.read_session(&m.id)?)),
            None => Ok(None),
        }
    }

    /// Read every session. Expensive on large histories.
    pub fn read_all_sessions(&self) -> Result<Vec<Session>> {
        let metas = self.list_sessions()?;
        let mut out = Vec::with_capacity(metas.len());
        for m in metas {
            match self.read_session(&m.id) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("Warning: could not read session {}: {}", m.id, e),
            }
        }
        Ok(out)
    }
}

// ── Tool classification ─────────────────────────────────────────────

/// Map an opencode tool name to toolpath's category ontology.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "read" | "list" | "view" | "ls" => Some(ToolCategory::FileRead),
        "glob" | "grep" | "search" => Some(ToolCategory::FileSearch),
        "write" | "edit" | "multiedit" | "patch" | "delete" => Some(ToolCategory::FileWrite),
        "bash" | "shell" | "exec" | "terminal" => Some(ToolCategory::Shell),
        "webfetch" | "websearch" | "web_fetch" | "web_search" | "fetch" => {
            Some(ToolCategory::Network)
        }
        "task" | "agent" | "subagent" | "spawn_agent" => Some(ToolCategory::Delegation),
        _ => {
            // MCP tools use "mcp__<server>__<tool>" convention. We don't
            // have enough info to categorize those; leave as None.
            None
        }
    }
}

/// Reverse of `tool_category`: pick opencode's native tool name for a
/// given category, disambiguating by `args` shape where needed
/// (e.g. `edit` vs `write`, `glob` vs `grep`).
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("bash"),
        ToolCategory::FileRead => Some("read"),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep"
        } else {
            "glob"
        }),
        ToolCategory::FileWrite => Some(if args.get("old_string").is_some() {
            "edit"
        } else {
            "write"
        }),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "webfetch"
        } else {
            "websearch"
        }),
        ToolCategory::Delegation => Some("task"),
    }
}

// ── Session → ConversationView ─────────────────────────────────────

/// Convert a parsed opencode [`Session`] to the provider-agnostic
/// [`ConversationView`] shape.
pub fn to_view(session: &Session) -> ConversationView {
    Builder::new(session).build()
}

struct Builder<'a> {
    session: &'a Session,
    turns: Vec<Turn>,
    events: Vec<ConversationEvent>,
    files_changed_order: Vec<String>,
    files_changed_seen: std::collections::HashSet<String>,
    total_usage: TokenUsage,
    total_usage_set: bool,
}

impl<'a> Builder<'a> {
    fn new(session: &'a Session) -> Self {
        Self {
            session,
            turns: Vec::new(),
            events: Vec::new(),
            files_changed_order: Vec::new(),
            files_changed_seen: std::collections::HashSet::new(),
            total_usage: TokenUsage::default(),
            total_usage_set: false,
        }
    }

    fn build(mut self) -> ConversationView {
        for msg in &self.session.messages {
            match &msg.data {
                MessageData::User(u) => self.handle_user_message(msg, u),
                MessageData::Assistant(a) => self.handle_assistant_message(msg, a),
                MessageData::Other => {
                    self.events.push(ConversationEvent {
                        id: format!("msg-other-{}", msg.id),
                        timestamp: millis_to_iso(msg.time_created),
                        parent_id: None,
                        event_type: "message.other".into(),
                        data: HashMap::new(),
                    });
                }
            }
        }

        ConversationView {
            id: self.session.id.clone(),
            started_at: Utc.timestamp_millis_opt(self.session.time_created).single(),
            last_activity: Utc.timestamp_millis_opt(self.session.time_updated).single(),
            turns: self.turns,
            total_usage: if self.total_usage_set {
                Some(self.total_usage)
            } else {
                None
            },
            provider_id: Some("opencode".into()),
            files_changed: self.files_changed_order,
            session_ids: vec![self.session.id.clone()],
            events: self.events,
        }
    }

    fn handle_user_message(&mut self, msg: &Message, u: &UserMessage) {
        let text = concat_text_parts(&msg.parts);
        let environment = Some(EnvironmentSnapshot {
            working_dir: Some(self.session.directory.to_string_lossy().to_string()),
            vcs_branch: None,
            vcs_revision: None,
        });
        let mut extra: HashMap<String, Value> = HashMap::new();
        let mut opencode_extra = Map::new();
        opencode_extra.insert("agent".into(), Value::String(u.agent.clone()));
        opencode_extra.insert(
            "model".into(),
            serde_json::to_value(&u.model).unwrap_or(Value::Null),
        );
        if let Some(tools) = &u.tools {
            opencode_extra.insert(
                "tools".into(),
                serde_json::to_value(tools).unwrap_or(Value::Null),
            );
        }
        if let Some(system) = &u.system
            && !system.is_empty()
        {
            opencode_extra.insert("system".into(), Value::String(system.clone()));
        }
        if !opencode_extra.is_empty() {
            extra.insert("opencode".into(), Value::Object(opencode_extra));
        }

        self.turns.push(Turn {
            id: msg.id.clone(),
            parent_id: None,
            role: Role::User,
            timestamp: millis_to_iso(msg.time_created),
            text,
            thinking: None,
            tool_uses: Vec::new(),
            model: None,
            stop_reason: None,
            token_usage: None,
            environment,
            delegations: Vec::new(),
            extra,
        });
    }

    fn handle_assistant_message(&mut self, msg: &Message, a: &AssistantMessage) {
        let mut text_chunks: Vec<String> = Vec::new();
        let mut thinking_chunks: Vec<String> = Vec::new();
        let mut tool_uses: Vec<ToolInvocation> = Vec::new();
        let mut snapshots: Vec<String> = Vec::new();
        let mut patches: Vec<Value> = Vec::new();
        let mut delegations: Vec<DelegatedWork> = Vec::new();
        let mut step_usage = TokenUsage::default();
        let mut step_usage_set = false;
        let mut step_cost_total = 0.0_f64;
        let mut stop_reason: Option<String> = None;

        for p in &msg.parts {
            match &p.data {
                PartData::Text(t) => {
                    if !t.text.is_empty() {
                        text_chunks.push(t.text.clone());
                    }
                }
                PartData::Reasoning(r) => {
                    if !r.text.is_empty() {
                        thinking_chunks.push(r.text.clone());
                    }
                }
                PartData::Tool(tp) => {
                    tool_uses.push(to_invocation(
                        tp,
                        &mut self.files_changed_order,
                        &mut self.files_changed_seen,
                    ));
                }
                PartData::StepStart(s) => {
                    if let Some(sh) = &s.snapshot
                        && snapshots.last().is_none_or(|l| l != sh)
                    {
                        snapshots.push(sh.clone());
                    }
                }
                PartData::StepFinish(sf) => {
                    if let Some(sh) = &sf.snapshot
                        && snapshots.last().is_none_or(|l| l != sh)
                    {
                        snapshots.push(sh.clone());
                    }
                    accumulate_tokens(&mut step_usage, &sf.tokens);
                    step_usage_set = true;
                    step_cost_total += sf.cost;
                    stop_reason = Some(sf.reason.clone());
                }
                PartData::Snapshot(s) => {
                    if snapshots.last().is_none_or(|l| l != &s.snapshot) {
                        snapshots.push(s.snapshot.clone());
                    }
                }
                PartData::Patch(pp) => {
                    patches.push(serde_json::json!({
                        "hash": pp.hash,
                        "files": pp.files,
                    }));
                    for f in &pp.files {
                        if self.files_changed_seen.insert(f.clone()) {
                            self.files_changed_order.push(f.clone());
                        }
                    }
                }
                PartData::Subtask(st) => {
                    delegations.push(DelegatedWork {
                        agent_id: st.agent.clone(),
                        prompt: st.prompt.clone(),
                        turns: Vec::new(),
                        result: None,
                    });
                }
                PartData::File(f) => {
                    self.events.push(ConversationEvent {
                        id: format!("file-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.file".into(),
                        data: to_data_map(&serde_json::to_value(f).unwrap_or(Value::Null)),
                    });
                }
                PartData::Agent(ag) => {
                    self.events.push(ConversationEvent {
                        id: format!("agent-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.agent".into(),
                        data: to_data_map(&serde_json::to_value(ag).unwrap_or(Value::Null)),
                    });
                }
                PartData::Retry(r) => {
                    self.events.push(ConversationEvent {
                        id: format!("retry-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.retry".into(),
                        data: to_data_map(&serde_json::to_value(r).unwrap_or(Value::Null)),
                    });
                }
                PartData::Compaction(c) => {
                    self.events.push(ConversationEvent {
                        id: format!("compaction-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.compaction".into(),
                        data: to_data_map(&serde_json::to_value(c).unwrap_or(Value::Null)),
                    });
                }
                PartData::Unknown => {
                    self.events.push(ConversationEvent {
                        id: format!("unknown-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.unknown".into(),
                        data: HashMap::new(),
                    });
                }
            }
        }

        // Prefer step-summed tokens over the message-level snapshot —
        // the step deltas capture the real per-step work.
        let token_usage = if step_usage_set {
            Some(step_usage.clone())
        } else {
            let u = tokens_to_convo(&a.tokens);
            if is_usage_zero(&u) { None } else { Some(u) }
        };

        if let Some(u) = token_usage.as_ref() {
            accumulate_total(&mut self.total_usage, u);
            self.total_usage_set = true;
        }

        let environment = Some(EnvironmentSnapshot {
            working_dir: Some(a.path.cwd.to_string_lossy().to_string()),
            vcs_branch: None,
            vcs_revision: None,
        });

        let mut extra: HashMap<String, Value> = HashMap::new();
        let mut opencode_extra: Map<String, Value> = Map::new();
        opencode_extra.insert("agent".into(), Value::String(a.agent.clone()));
        opencode_extra.insert("providerID".into(), Value::String(a.provider_id.clone()));
        opencode_extra.insert("modelID".into(), Value::String(a.model_id.clone()));
        opencode_extra.insert("cost_step_total".into(), json_num(step_cost_total));
        opencode_extra.insert("cost_message".into(), json_num(a.cost));
        if !snapshots.is_empty() {
            opencode_extra.insert(
                "snapshots".into(),
                Value::Array(snapshots.into_iter().map(Value::String).collect()),
            );
        }
        if !patches.is_empty() {
            opencode_extra.insert("patches".into(), Value::Array(patches));
        }
        if let Some(v) = &a.variant {
            opencode_extra.insert("variant".into(), Value::String(v.clone()));
        }
        if let Some(err) = &a.error {
            opencode_extra.insert("error".into(), err.clone());
        }
        extra.insert("opencode".into(), Value::Object(opencode_extra));

        self.turns.push(Turn {
            id: msg.id.clone(),
            parent_id: if a.parent_id.is_empty() {
                None
            } else {
                Some(a.parent_id.clone())
            },
            role: Role::Assistant,
            timestamp: millis_to_iso(msg.time_created),
            text: text_chunks.join("\n\n"),
            thinking: if thinking_chunks.is_empty() {
                None
            } else {
                Some(thinking_chunks.join("\n\n"))
            },
            tool_uses,
            model: if a.model_id.is_empty() {
                None
            } else {
                Some(a.model_id.clone())
            },
            stop_reason: stop_reason.or_else(|| a.finish.clone()),
            token_usage,
            environment,
            delegations,
            extra,
        });
    }
}

fn concat_text_parts(parts: &[Part]) -> String {
    let mut chunks = Vec::new();
    for p in parts {
        if let PartData::Text(t) = &p.data
            && !t.text.is_empty()
            && !t.ignored.unwrap_or(false)
        {
            chunks.push(t.text.clone());
        }
    }
    chunks.join("\n\n")
}

fn to_invocation(
    tp: &crate::types::ToolPart,
    files_changed_order: &mut Vec<String>,
    files_changed_seen: &mut std::collections::HashSet<String>,
) -> ToolInvocation {
    let input = tp.state.input().cloned().unwrap_or(Value::Null);
    let result = match &tp.state {
        ToolState::Completed(c) => Some(ToolResult {
            content: c.output.clone(),
            is_error: false,
        }),
        ToolState::Error(e) => Some(ToolResult {
            content: e.error.clone(),
            is_error: true,
        }),
        _ => None,
    };

    // Opportunistically collect files-changed from tool inputs.
    if matches!(tp.tool.as_str(), "edit" | "write" | "multiedit" | "patch")
        && let Some(path) = input
            .get("filePath")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str())
        && files_changed_seen.insert(path.to_string())
    {
        files_changed_order.push(path.to_string());
    }

    ToolInvocation {
        id: tp.call_id.clone(),
        name: tp.tool.clone(),
        input,
        result,
        category: tool_category(&tp.tool),
    }
}

fn accumulate_tokens(total: &mut TokenUsage, step: &Tokens) {
    add_u32(&mut total.input_tokens, step.input as u32);
    add_u32(&mut total.output_tokens, step.output as u32);
    add_u32(&mut total.cache_read_tokens, step.cache.read as u32);
    add_u32(&mut total.cache_write_tokens, step.cache.write as u32);
}

fn add_u32(slot: &mut Option<u32>, delta: u32) {
    if delta == 0 {
        return;
    }
    *slot = Some(slot.unwrap_or(0).saturating_add(delta));
}

fn tokens_to_convo(t: &Tokens) -> TokenUsage {
    TokenUsage {
        input_tokens: if t.input == 0 {
            None
        } else {
            Some(t.input as u32)
        },
        output_tokens: if t.output == 0 {
            None
        } else {
            Some(t.output as u32)
        },
        cache_read_tokens: if t.cache.read == 0 {
            None
        } else {
            Some(t.cache.read as u32)
        },
        cache_write_tokens: if t.cache.write == 0 {
            None
        } else {
            Some(t.cache.write as u32)
        },
    }
}

fn is_usage_zero(u: &TokenUsage) -> bool {
    u.input_tokens.is_none()
        && u.output_tokens.is_none()
        && u.cache_read_tokens.is_none()
        && u.cache_write_tokens.is_none()
}

fn accumulate_total(total: &mut TokenUsage, delta: &TokenUsage) {
    if let Some(v) = delta.input_tokens {
        add_u32(&mut total.input_tokens, v);
    }
    if let Some(v) = delta.output_tokens {
        add_u32(&mut total.output_tokens, v);
    }
    if let Some(v) = delta.cache_read_tokens {
        add_u32(&mut total.cache_read_tokens, v);
    }
    if let Some(v) = delta.cache_write_tokens {
        add_u32(&mut total.cache_write_tokens, v);
    }
}

fn millis_to_iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| ms.to_string())
}

fn to_data_map(v: &Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => {
            let mut m = HashMap::new();
            m.insert("value".into(), v.clone());
            m
        }
    }
}

fn json_num(v: f64) -> Value {
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

// ── ConversationProvider trait impl ─────────────────────────────────

impl ConversationProvider for OpencodeConvo {
    fn list_conversations(&self, _project: &str) -> toolpath_convo::Result<Vec<String>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas.into_iter().map(|m| m.id).collect())
    }

    fn load_conversation(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let s = self
            .read_session(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(to_view(&s))
    }

    fn load_metadata(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let m = self
            .io
            .read_metadata(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: m.id,
            started_at: m.started_at,
            last_activity: m.last_activity,
            message_count: m.message_count,
            file_path: Some(m.directory),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, _project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: Some(m.directory),
                predecessor: None,
                successor: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    fn setup(body_sql: &str) -> (TempDir, OpencodeConvo) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let conn = Connection::open(data.join("opencode.db")).unwrap();
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
              icon_url text, icon_color text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            {body_sql}
        "#
        ))
        .unwrap();
        drop(conn);
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_data_dir(&data);
        (temp, OpencodeConvo::with_resolver(resolver))
    }

    const BASIC_SQL: &str = r#"
        INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
          VALUES ('proj', '/tmp/proj', 1000, 3000, '[]');
        INSERT INTO session (id, project_id, slug, directory, title, version,
                             time_created, time_updated)
          VALUES ('ses_x', 'proj', 'slug', '/tmp/proj', 'T', '1.3.10', 1000, 3000);
        INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
          ('m1','ses_x',1001,1001,
           '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
          ('m2','ses_x',1002,1100,
           '{"parentID":"m1","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"modelID":"claude","providerID":"anthropic","time":{"created":1002,"completed":1100},"finish":"stop"}');
        INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
          ('p1','m1','ses_x',1001,1001,'{"type":"text","text":"make a pickle"}'),
          ('p2','m2','ses_x',1002,1002,'{"type":"step-start","snapshot":"snap_a"}'),
          ('p3','m2','ses_x',1003,1003,'{"type":"reasoning","text":"I should write main.cpp","time":{"start":1003,"end":1004}}'),
          ('p4','m2','ses_x',1005,1005,'{"type":"tool","tool":"bash","callID":"call_1","state":{"status":"completed","input":{"command":"ls"},"output":"files\n","title":"List","metadata":{"exit":0},"time":{"start":1005,"end":1006}}}'),
          ('p5','m2','ses_x',1007,1007,'{"type":"tool","tool":"write","callID":"call_2","state":{"status":"completed","input":{"filePath":"/tmp/proj/main.cpp","content":"int main(){}\n"},"output":"wrote","title":"Write","metadata":{"bytes":13},"time":{"start":1007,"end":1008}}}'),
          ('p6','m2','ses_x',1009,1009,'{"type":"text","text":"done!"}'),
          ('p7','m2','ses_x',1010,1010,'{"type":"step-finish","reason":"stop","snapshot":"snap_b","tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"cost":0.01}');
    "#;

    #[test]
    fn basic_view_shape() {
        let (_t, mgr) = setup(BASIC_SQL);
        let s = mgr.read_session("ses_x").unwrap();
        let view = to_view(&s);

        assert_eq!(view.id, "ses_x");
        assert_eq!(view.provider_id.as_deref(), Some("opencode"));
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "make a pickle");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "done!");
        assert_eq!(
            view.turns[1].thinking.as_deref(),
            Some("I should write main.cpp")
        );
    }

    #[test]
    fn tool_invocations_paired() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        let assistant = &view.turns[1];
        assert_eq!(assistant.tool_uses.len(), 2);
        let bash = &assistant.tool_uses[0];
        assert_eq!(bash.name, "bash");
        assert_eq!(bash.category, Some(ToolCategory::Shell));
        assert_eq!(bash.result.as_ref().unwrap().content, "files\n");
        let write = &assistant.tool_uses[1];
        assert_eq!(write.name, "write");
        assert_eq!(write.category, Some(ToolCategory::FileWrite));
    }

    #[test]
    fn snapshots_surface_on_assistant_extra() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        let assistant = &view.turns[1];
        let snaps = assistant.extra["opencode"]["snapshots"].as_array().unwrap();
        assert_eq!(
            snaps,
            &[
                Value::String("snap_a".into()),
                Value::String("snap_b".into())
            ]
        );
    }

    #[test]
    fn files_changed_from_tool_input() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        assert_eq!(view.files_changed, vec!["/tmp/proj/main.cpp".to_string()]);
    }

    #[test]
    fn step_finish_drives_token_usage() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        let u = view.turns[1].token_usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.cache_read_tokens, Some(10));

        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(100));
        assert_eq!(total.output_tokens, Some(20));
    }

    #[test]
    fn tool_error_becomes_tool_result_error() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p', '/p', 1, 2, '[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"tool","tool":"bash","callID":"c","state":{"status":"error","input":{"command":"false"},"error":"exit 1","time":{"start":1,"end":2}}}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        let tool = &view.turns[0].tool_uses[0];
        let r = tool.result.as_ref().unwrap();
        assert!(r.is_error);
        assert_eq!(r.content, "exit 1");
    }

    #[test]
    fn compaction_becomes_event() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"compaction","auto":true,"overflow":false}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        assert!(
            view.events
                .iter()
                .any(|e| e.event_type == "part.compaction")
        );
    }

    #[test]
    fn unknown_part_type_becomes_event() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes) VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"future-thing","foo":"bar"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        assert!(view.events.iter().any(|e| e.event_type == "part.unknown"));
    }

    #[test]
    fn tool_category_mapping() {
        assert_eq!(tool_category("bash"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("edit"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("write"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("read"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("grep"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("webfetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("task"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("mcp__x__y"), None);
    }

    #[test]
    fn provider_trait_list_and_load() {
        let (_t, mgr) = setup(BASIC_SQL);
        let ids = ConversationProvider::list_conversations(&mgr, "").unwrap();
        assert_eq!(ids, vec!["ses_x".to_string()]);
        let v = ConversationProvider::load_conversation(&mgr, "", "ses_x").unwrap();
        assert_eq!(v.turns.len(), 2);
    }
}
