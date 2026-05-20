//! Implementation of `toolpath-convo` traits for Codex sessions.
//!
//! The hard part is mapping Codex's **streaming** event model onto
//! `toolpath_convo::Turn`, which is message-shaped. The approach:
//!
//! 1. Walk the rollout lines in order.
//! 2. `response_item.message` creates a new `Turn`. Role/content are
//!    mapped straightforwardly; `developer` becomes `Role::System`.
//! 3. `response_item.reasoning` is buffered and attached to the next
//!    assistant turn's `thinking` field.
//! 4. `response_item.function_call` / `custom_tool_call` attach to the
//!    **current turn** (or a synthetic carrier if no message preceded
//!    them) as `ToolInvocation` entries. Output is back-filled when we
//!    see the matching `*_output` by `call_id`.
//! 5. `event_msg.exec_command_end` enriches the already-emitted tool
//!    invocation with the exit code / stdout / stderr.
//! 6. `event_msg.patch_apply_end` is captured on the current turn's
//!    `extra["codex"]["patch_changes"]` — the derive layer consumes it
//!    for file-artifact sibling changes.
//! 7. `event_msg.token_count` populates `Turn.token_usage` on the next
//!    assistant turn emitted.
//! 8. Everything else (`task_started`, `task_complete`, `turn_context`,
//!    `user_message`/`agent_message` duplicates, unknown events) lands
//!    in `ConversationView.events` as a typed [`ConversationEvent`].

use std::collections::HashMap;

use crate::io::ConvoIO;
use crate::types::{
    EventMsg, ExecCommandEnd, Message, PatchApplyEnd, PatchChange, ResponseItem, RolloutItem,
    Session, TokenCountInfo, TokenUsage as CodexTokenUsage,
};
use serde_json::Value;
use toolpath_convo::{
    ConversationEvent, ConversationMeta, ConversationProvider, ConversationView, ConvoError,
    EnvironmentSnapshot, FileMutation, ProducerInfo, Role, SessionBase, TokenUsage, ToolCategory,
    ToolInvocation, ToolResult, Turn,
};

/// Provider for Codex sessions.
#[derive(Debug, Clone, Default)]
pub struct CodexConvo {
    io: ConvoIO,
}

impl CodexConvo {
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: crate::paths::PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &crate::paths::PathResolver {
        self.io.resolver()
    }

    /// Read one session into a [`Session`] struct (raw lines).
    pub fn read_session(&self, session_id: &str) -> crate::Result<Session> {
        self.io.read_session(session_id)
    }

    /// List all sessions, newest first.
    pub fn list_sessions(&self) -> crate::Result<Vec<crate::types::SessionMetadata>> {
        self.io.list_sessions()
    }

    /// Most recent session (by last activity), if any.
    pub fn most_recent_session(&self) -> crate::Result<Option<Session>> {
        let metas = self.list_sessions()?;
        match metas.first() {
            Some(m) => Ok(Some(self.read_session(&m.id)?)),
            None => Ok(None),
        }
    }

    /// Read all sessions into memory (expensive on large histories).
    pub fn read_all_sessions(&self) -> crate::Result<Vec<Session>> {
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

/// Classify a Codex tool name into toolpath's category ontology.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "read_file" | "read_many_files" | "list_dir" | "view_image" | "mcp_resource" => {
            Some(ToolCategory::FileRead)
        }
        "glob" | "grep_search" | "search_file_content" | "tool_search" | "tool_suggest" => {
            Some(ToolCategory::FileSearch)
        }
        "write_file" | "apply_patch" | "replace" | "edit" => Some(ToolCategory::FileWrite),
        "shell" | "exec_command" | "unified_exec" | "write_stdin" | "js_repl" => {
            Some(ToolCategory::Shell)
        }
        "web_fetch" | "web_search" | "google_web_search" => Some(ToolCategory::Network),
        "spawn_agent" | "close_agent" | "wait_agent" | "resume_agent" | "send_message"
        | "followup_task" | "list_agents" | "agent_jobs" | "task" | "activate_skill" => {
            Some(ToolCategory::Delegation)
        }
        _ => None,
    }
}

/// Reverse of [`tool_category`]: pick Codex's preferred native tool name
/// for a generic [`ToolCategory`], using call args to disambiguate.
///
/// Used by [`crate::project::CodexProjector`] when projecting tool calls
/// from foreign harnesses. Notably, FileWrite always maps to `write_file`
/// (not `apply_patch`) — `apply_patch` takes a free-form V4A patch
/// string rather than JSON args, so projecting JSON-shape edits as
/// `apply_patch` would emit a malformed CustomToolCall. Same-harness
/// round-trips preserve the source name verbatim before reaching this
/// fallback.
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("exec_command"),
        ToolCategory::FileRead => Some(if args.get("file_paths").is_some() {
            "read_many_files"
        } else if args.get("path").is_some() && args.get("file_path").is_none() {
            "list_dir"
        } else {
            "read_file"
        }),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep_search"
        } else {
            "glob"
        }),
        ToolCategory::FileWrite => Some("write_file"),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "web_fetch"
        } else {
            "web_search"
        }),
        ToolCategory::Delegation => Some("spawn_agent"),
    }
}

// ── Session → ConversationView ─────────────────────────────────────

/// Convert a parsed Codex [`Session`] to the provider-agnostic
/// [`ConversationView`] shape.
pub fn to_view(session: &Session) -> ConversationView {
    Builder::new(session).build()
}

/// Convert one rollout line to a "best-effort" `Turn`, if it carries
/// one. Used by consumers who want per-line processing without the
/// cross-line assembly that [`to_view`] does.
pub fn to_turn(line_payload: &ResponseItem) -> Option<Turn> {
    if let ResponseItem::Message(m) = line_payload {
        Some(message_to_turn(m, "", None, None))
    } else {
        None
    }
}

struct Builder<'a> {
    session: &'a Session,
    turns: Vec<Turn>,
    events: Vec<ConversationEvent>,
    /// Plaintext reasoning summaries (rare — only in configurations where
    /// OpenAI exposes public reasoning). These land on `Turn.thinking`.
    pending_reasoning_plaintext: Vec<String>,
    pending_token_usage: Option<TokenUsage>,
    working_dir: Option<String>,
    current_model: Option<String>,
    call_index: HashMap<String, (usize, usize)>,
    total_usage: TokenUsage,
    total_usage_set: bool,
    files_changed_order: Vec<String>,
    files_changed_seen: std::collections::HashSet<String>,
}

impl<'a> Builder<'a> {
    fn new(session: &'a Session) -> Self {
        Self {
            session,
            turns: Vec::new(),
            events: Vec::new(),
            pending_reasoning_plaintext: Vec::new(),
            pending_token_usage: None,
            working_dir: None,
            current_model: None,
            call_index: HashMap::new(),
            total_usage: TokenUsage::default(),
            total_usage_set: false,
            files_changed_order: Vec::new(),
            files_changed_seen: std::collections::HashSet::new(),
        }
    }

    fn build(mut self) -> ConversationView {
        for line in &self.session.lines {
            match line.item() {
                RolloutItem::SessionMeta(m) => {
                    self.working_dir = Some(m.cwd.to_string_lossy().to_string());
                    self.events.push(event_from_raw(
                        &line.timestamp,
                        "session_meta",
                        &line.payload,
                    ));
                }
                RolloutItem::TurnContext(tc) => {
                    if let Some(m) = &tc.model {
                        self.current_model = Some(m.clone());
                    }
                    let wd = tc.cwd.to_string_lossy().to_string();
                    if !wd.is_empty() {
                        self.working_dir = Some(wd);
                    }
                    self.events.push(event_from_raw(
                        &line.timestamp,
                        "turn_context",
                        &line.payload,
                    ));
                }
                RolloutItem::ResponseItem(ri) => self.handle_response_item(&line.timestamp, ri),
                RolloutItem::EventMsg(ev) => {
                    self.handle_event_msg(&line.timestamp, ev, &line.payload)
                }
                RolloutItem::SessionState(payload) => {
                    self.events
                        .push(event_from_raw(&line.timestamp, "session_state", &payload));
                }
                RolloutItem::Compacted(payload) => {
                    self.events
                        .push(event_from_raw(&line.timestamp, "compacted", &payload));
                }
                RolloutItem::Unknown { kind, payload } => {
                    self.events
                        .push(event_from_raw(&line.timestamp, &kind, &payload));
                }
            }
        }

        // Path-level base context from session_meta (cwd + git).
        let meta = self.session.meta();
        let base = {
            let wd = meta
                .as_ref()
                .map(|m| m.cwd.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| self.working_dir.clone());
            let git = meta.as_ref().and_then(|m| m.git.as_ref());
            let revision = git.and_then(|g| g.commit_hash.clone());
            let branch = git.and_then(|g| g.branch.clone());
            let remote = git.and_then(|g| g.repository_url.clone());
            if wd.is_some() || revision.is_some() || branch.is_some() || remote.is_some() {
                Some(SessionBase {
                    working_dir: wd,
                    vcs_revision: revision,
                    vcs_branch: branch,
                    vcs_remote: remote,
                })
            } else {
                None
            }
        };

        // Producer (originator + cli_version) lifts onto the typed view
        // field. `model_provider` already lives on each assistant
        // `ActorDefinition.provider`. Codex's `source` and `forked_from_id`
        // are wire-level fields with no cross-harness analog — the codex
        // projector hard-codes defaults on the return path, so we let them
        // drop on this side.
        let producer = meta.as_ref().map(|m| ProducerInfo {
            name: m.originator.clone(),
            version: Some(m.cli_version.clone()),
        });

        // Filter empty carrier turns (no text, no thinking, no tool calls).
        // Previously done inside `derive_path_from_view`; moved here so the
        // canonical `derive_path` sees only meaningful turns.
        self.turns
            .retain(|t| !(t.text.is_empty() && t.thinking.is_none() && t.tool_uses.is_empty()));

        // Assign synthetic ids to turns whose source message didn't carry
        // one, then link sequentially via `parent_id` so the shared
        // `derive_path` can walk a connected DAG. Codex turns don't carry
        // explicit parent ids on the wire; this preserves the linear
        // ordering the old `derive_path_from_view` produced.
        for (idx, t) in self.turns.iter_mut().enumerate() {
            if t.id.is_empty() {
                t.id = format!("codex-turn-{:04}", idx + 1);
            }
        }
        let mut prev: Option<String> = None;
        for t in self.turns.iter_mut() {
            if t.parent_id.is_none() {
                t.parent_id = prev.clone();
            }
            prev = Some(t.id.clone());
        }

        // Disambiguate event ids. `event_from_raw` synthesizes
        // `<event_type>-<timestamp>`, which collides when codex emits
        // multiple events of the same type at the same timestamp (rare
        // but real). Suffix duplicates with their position so each step
        // gets a unique id.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in &self.turns {
            seen.insert(t.id.clone());
        }
        for (i, e) in self.events.iter_mut().enumerate() {
            if !seen.insert(e.id.clone()) {
                e.id = format!("{}-{:04}", e.id, i);
                seen.insert(e.id.clone());
            }
        }

        ConversationView {
            id: self.session.id.clone(),
            started_at: self.session.started_at(),
            last_activity: self.session.last_activity(),
            turns: self.turns,
            total_usage: if self.total_usage_set {
                Some(self.total_usage)
            } else {
                None
            },
            provider_id: Some("codex".into()),
            files_changed: self.files_changed_order,
            session_ids: vec![],
            events: self.events,
            base,
            producer,
        }
    }

    fn handle_response_item(&mut self, timestamp: &str, ri: ResponseItem) {
        match ri {
            ResponseItem::Message(msg) => {
                let turn = message_to_turn(
                    &msg,
                    timestamp,
                    self.working_dir.as_deref(),
                    self.current_model.as_deref(),
                );
                self.push_turn(turn);
            }
            ResponseItem::Reasoning(r) => {
                // Plaintext content (rare) → Turn.thinking.
                if let Some(Value::Array(arr)) = r.content.as_ref() {
                    for v in arr {
                        if let Some(s) = v.get("text").and_then(|t| t.as_str()) {
                            self.pending_reasoning_plaintext.push(s.to_string());
                        }
                    }
                }
                // Plaintext summary items — same treatment.
                for v in &r.summary {
                    if let Some(s) = v.get("text").and_then(|t| t.as_str()) {
                        self.pending_reasoning_plaintext.push(s.to_string());
                    }
                }
            }
            ResponseItem::FunctionCall(fc) => {
                let name = fc.name.clone();
                let input = fc.arguments_as_json();
                let input = if input.is_null() {
                    Value::String(fc.arguments.clone())
                } else {
                    input
                };
                self.attach_tool_call(timestamp, fc.call_id, name, input);
            }
            ResponseItem::FunctionCallOutput(out) => {
                let is_error = out
                    .extra
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.attach_tool_output(&out.call_id, &out.output, is_error);
            }
            ResponseItem::CustomToolCall(ct) => {
                let input = Value::String(ct.input.clone());
                self.attach_tool_call(timestamp, ct.call_id, ct.name, input);
            }
            ResponseItem::CustomToolCallOutput(out) => {
                let is_error = out
                    .extra
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.attach_tool_output(&out.call_id, &out.output, is_error);
            }
            ResponseItem::Other { kind, payload } => {
                self.events.push(ConversationEvent {
                    id: synthetic_event_id(timestamp, &kind),
                    timestamp: timestamp.to_string(),
                    parent_id: None,
                    event_type: format!("response_item.{}", kind),
                    data: data_from_value(&payload),
                });
            }
        }
    }

    fn handle_event_msg(&mut self, timestamp: &str, ev: EventMsg, raw_payload: &Value) {
        match ev {
            EventMsg::TokenCount(tc) => {
                if let Some(info) = tc.info.as_ref() {
                    apply_token_count(&mut self.total_usage, info);
                    self.total_usage_set = true;
                    if let Some(total) = info.total_token_usage.as_ref() {
                        self.pending_token_usage = Some(codex_usage_to_convo(total));
                    }
                }
                self.events
                    .push(event_from_raw(timestamp, "token_count", raw_payload));
            }
            EventMsg::ExecCommandEnd(exec) => {
                self.apply_exec_command_end(&exec);
                self.events
                    .push(event_from_raw(timestamp, "exec_command_end", raw_payload));
            }
            EventMsg::PatchApplyEnd(patch) => {
                self.apply_patch_apply_end(&patch);
                self.events
                    .push(event_from_raw(timestamp, "patch_apply_end", raw_payload));
            }
            EventMsg::AgentMessage(_)
            | EventMsg::UserMessage(_)
            | EventMsg::TaskStarted(_)
            | EventMsg::TaskComplete(_) => {
                self.events
                    .push(event_from_raw(timestamp, ev.kind(), raw_payload));
            }
            EventMsg::Other { kind, payload } => {
                self.events.push(event_from_raw(timestamp, &kind, &payload));
            }
        }
    }

    fn attach_tool_call(&mut self, timestamp: &str, call_id: String, name: String, input: Value) {
        let category = tool_category(&name);
        let invocation = ToolInvocation {
            id: call_id.clone(),
            name,
            input,
            result: None,
            category,
        };

        let turn_idx = match self.last_assistant_turn_index() {
            Some(idx) => idx,
            None => {
                let mut t = synthetic_assistant_turn(
                    timestamp,
                    self.working_dir.as_deref(),
                    self.current_model.as_deref(),
                );
                self.drain_pending_onto(&mut t);
                self.turns.push(t);
                self.turns.len() - 1
            }
        };
        let tool_idx = self.turns[turn_idx].tool_uses.len();
        self.turns[turn_idx].tool_uses.push(invocation);
        self.call_index.insert(call_id, (turn_idx, tool_idx));
    }

    fn attach_tool_output(&mut self, call_id: &str, output: &str, is_error: bool) {
        if let Some((turn_idx, tool_idx)) = self.call_index.get(call_id).copied() {
            let turn = &mut self.turns[turn_idx];
            if let Some(inv) = turn.tool_uses.get_mut(tool_idx) {
                let prior_error = inv.result.as_ref().map(|r| r.is_error).unwrap_or(false);
                let merged = match inv.result.as_ref() {
                    Some(existing) => format!("{}\n{}", existing.content, output),
                    None => output.to_string(),
                };
                inv.result = Some(ToolResult {
                    content: merged,
                    is_error: is_error || prior_error,
                });
            }
        }
    }

    fn apply_exec_command_end(&mut self, exec: &ExecCommandEnd) {
        if let Some((turn_idx, tool_idx)) = self.call_index.get(&exec.call_id).copied() {
            let turn = &mut self.turns[turn_idx];
            if let Some(inv) = turn.tool_uses.get_mut(tool_idx) {
                let is_error = exec.exit_code.map(|c| c != 0).unwrap_or(false);
                if inv.result.is_none() {
                    let body = if !exec.aggregated_output.is_empty() {
                        exec.aggregated_output.clone()
                    } else if !exec.stdout.is_empty() || !exec.stderr.is_empty() {
                        let mut s = String::new();
                        if !exec.stdout.is_empty() {
                            s.push_str(&exec.stdout);
                        }
                        if !exec.stderr.is_empty() {
                            if !s.is_empty() {
                                s.push('\n');
                            }
                            s.push_str(&exec.stderr);
                        }
                        s
                    } else {
                        format!("(exit {})", exec.exit_code.unwrap_or_default())
                    };
                    inv.result = Some(ToolResult {
                        content: body,
                        is_error,
                    });
                } else if is_error && let Some(r) = inv.result.as_mut() {
                    r.is_error = true;
                }
            }
        }
    }

    fn apply_patch_apply_end(&mut self, patch: &PatchApplyEnd) {
        let loc = self.call_index.get(&patch.call_id).copied();

        // `patch.changes` is a HashMap — iterate in sorted order so the
        // derived order is deterministic across runs.
        let mut paths: Vec<&String> = patch.changes.keys().collect();
        paths.sort();

        // Populate `turn.file_mutations` on the matching turn, with
        // `tool_id` set to the `call_id` so `derive_path` can link the
        // sibling `file.write` change back to this specific tool call.
        if let Some((turn_idx, _tool_idx)) = loc {
            let turn = &mut self.turns[turn_idx];
            for path in &paths {
                if let Some(change) = patch.changes.get(*path) {
                    let mut fm = patch_change_to_file_mutation(path, change);
                    fm.tool_id = Some(patch.call_id.clone());
                    turn.file_mutations.push(fm);
                }
            }
        }

        for path in paths {
            if self.files_changed_seen.insert(path.clone()) {
                self.files_changed_order.push(path.clone());
            }
        }
    }

    fn push_turn(&mut self, mut turn: Turn) {
        self.drain_pending_onto(&mut turn);
        self.turns.push(turn);
    }

    fn drain_pending_onto(&mut self, turn: &mut Turn) {
        if turn.role != Role::Assistant {
            return;
        }
        // Plaintext reasoning summaries are safe to render as thinking.
        if !self.pending_reasoning_plaintext.is_empty() {
            turn.thinking = Some(self.pending_reasoning_plaintext.join("\n\n"));
            self.pending_reasoning_plaintext.clear();
        }
        if let Some(tu) = self.pending_token_usage.take() {
            turn.token_usage = Some(tu);
        }
    }

    fn last_assistant_turn_index(&self) -> Option<usize> {
        self.turns
            .iter()
            .rposition(|t| t.role == Role::Assistant)
            .or_else(|| self.turns.len().checked_sub(1))
    }
}

// ── Patch → FileMutation conversion ─────────────────────────────────

fn patch_change_to_file_mutation(path: &str, change: &PatchChange) -> FileMutation {
    let mut fm = FileMutation {
        path: path.to_string(),
        ..Default::default()
    };
    match change {
        PatchChange::Add { content, .. } => {
            fm.operation = Some("add".into());
            fm.after = Some(content.clone());
            fm.raw_diff = Some(synth_add_diff(content));
        }
        PatchChange::Update {
            unified_diff,
            move_path,
            ..
        } => {
            fm.operation = Some("update".into());
            fm.raw_diff = Some(unified_diff.clone());
            fm.rename_to = move_path.clone();
        }
        PatchChange::Delete {
            original_content, ..
        } => {
            fm.operation = Some("delete".into());
            fm.before = original_content.clone();
            fm.raw_diff = original_content.as_deref().map(synth_delete_diff);
        }
        PatchChange::Unknown => {
            fm.operation = Some("unknown".into());
        }
    }
    fm
}

fn synth_add_diff(content: &str) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let effective: &[&str] = if lines.last() == Some(&"") {
        &lines[..lines.len().saturating_sub(1)]
    } else {
        &lines[..]
    };
    let mut buf = format!("@@ -0,0 +1,{} @@\n", effective.len());
    for l in effective {
        buf.push('+');
        buf.push_str(l);
        buf.push('\n');
    }
    buf
}

fn synth_delete_diff(original: &str) -> String {
    let lines: Vec<&str> = original.split('\n').collect();
    let effective: &[&str] = if lines.last() == Some(&"") {
        &lines[..lines.len().saturating_sub(1)]
    } else {
        &lines[..]
    };
    let mut buf = format!("@@ -1,{} +0,0 @@\n", effective.len());
    for l in effective {
        buf.push('-');
        buf.push_str(l);
        buf.push('\n');
    }
    buf
}

fn message_to_turn(
    msg: &Message,
    timestamp: &str,
    working_dir: Option<&str>,
    model: Option<&str>,
) -> Turn {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "developer" | "system" => Role::System,
        other => Role::Other(other.to_string()),
    };

    let text = msg.text();

    let environment = working_dir.map(|wd| EnvironmentSnapshot {
        working_dir: Some(wd.to_string()),
        vcs_branch: None,
        vcs_revision: None,
    });

    Turn {
        id: msg.id.clone().unwrap_or_default(),
        parent_id: None,
        role: role.clone(),
        timestamp: timestamp.to_string(),
        text,
        thinking: None,
        tool_uses: Vec::new(),
        model: if role == Role::Assistant {
            model.map(str::to_string)
        } else {
            None
        },
        stop_reason: None,
        token_usage: None,
        environment,
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    }
}

fn synthetic_assistant_turn(
    timestamp: &str,
    working_dir: Option<&str>,
    model: Option<&str>,
) -> Turn {
    Turn {
        id: format!("synth-{}", timestamp),
        parent_id: None,
        role: Role::Assistant,
        timestamp: timestamp.to_string(),
        text: String::new(),
        thinking: None,
        tool_uses: Vec::new(),
        model: model.map(str::to_string),
        stop_reason: None,
        token_usage: None,
        environment: working_dir.map(|wd| EnvironmentSnapshot {
            working_dir: Some(wd.to_string()),
            vcs_branch: None,
            vcs_revision: None,
        }),
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    }
}

fn codex_usage_to_convo(u: &CodexTokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cached_input_tokens,
        cache_write_tokens: None,
    }
}

fn apply_token_count(total: &mut TokenUsage, info: &TokenCountInfo) {
    if let Some(t) = info.total_token_usage.as_ref() {
        total.input_tokens = t.input_tokens.or(total.input_tokens);
        total.output_tokens = t.output_tokens.or(total.output_tokens);
        total.cache_read_tokens = t.cached_input_tokens.or(total.cache_read_tokens);
    }
}

fn event_from_raw(timestamp: &str, event_type: &str, payload: &Value) -> ConversationEvent {
    ConversationEvent {
        id: synthetic_event_id(timestamp, event_type),
        timestamp: timestamp.to_string(),
        parent_id: None,
        event_type: event_type.to_string(),
        data: data_from_value(payload),
    }
}

fn synthetic_event_id(timestamp: &str, kind: &str) -> String {
    format!("{}-{}", kind, timestamp)
}

fn data_from_value(v: &Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => {
            let mut m = HashMap::new();
            m.insert("value".into(), v.clone());
            m
        }
    }
}

// ── ConversationProvider trait impl ────────────────────────────────

impl ConversationProvider for CodexConvo {
    fn list_conversations(&self, _project: &str) -> toolpath_convo::Result<Vec<String>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(metas.into_iter().map(|m| m.id).collect())
    }

    fn load_conversation(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let session = self
            .read_session(conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(to_view(&session))
    }

    fn load_metadata(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let path = self
            .io
            .resolver()
            .find_rollout_file(conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        let meta = self
            .io
            .read_metadata(path)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: meta.id,
            started_at: meta.started_at,
            last_activity: meta.last_activity,
            message_count: meta.line_count,
            file_path: Some(meta.file_path),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, _project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.line_count,
                file_path: Some(m.file_path),
                predecessor: None,
                successor: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_session_fixture(body: &str) -> (TempDir, CodexConvo, String) {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        let day = codex.join("sessions/2026/04/20");
        fs::create_dir_all(&day).unwrap();
        let name = "rollout-2026-04-20T10-00-00-019dabc6-8fef-7681-a054-b5bb75fcb97d";
        fs::write(day.join(format!("{}.jsonl", name)), body).unwrap();
        let resolver = crate::paths::PathResolver::new().with_codex_dir(&codex);
        (temp, CodexConvo::with_resolver(resolver), name.to_string())
    }

    fn minimal_session() -> String {
        [
            r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{"id":"019dabc6-8fef-7681-a054-b5bb75fcb97d","timestamp":"2026-04-20T16:43:30.171Z","cwd":"/tmp/proj","originator":"codex-tui","cli_version":"0.118.0","source":"cli","git":{"commit_hash":"abc","branch":"main"}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:37.773Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:37.775Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:37.800Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"please do a thing"}]}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.000Z","type":"response_item","payload":{"type":"reasoning","summary":[],"content":null,"encrypted_content":"encrypted-blob-1"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.100Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"working on it"}],"phase":"commentary"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.200Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}","call_id":"call_1"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.300Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"Command: pwd\nOutput:\n/tmp/proj\n"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.400Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_1","command":["/bin/bash","-lc","pwd"],"stdout":"/tmp/proj\n","exit_code":0,"aggregated_output":"/tmp/proj\n"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.500Z","type":"response_item","payload":{"type":"custom_tool_call","status":"completed","call_id":"call_2","name":"apply_patch","input":"*** Begin Patch\n*** Add File: /tmp/proj/a.rs\n+fn main() {}\n*** End Patch"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.600Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_2","output":"{\"output\":\"ok\"}"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.700Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_2","success":true,"changes":{"/tmp/proj/a.rs":{"type":"add","content":"fn main() {}\n"}}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.800Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":20,"cached_input_tokens":10,"total_tokens":130}}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.900Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}],"phase":"final","end_turn":true}}"#,
            r#"{"timestamp":"2026-04-20T16:44:39.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","last_agent_message":"done"}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn build_view_basic() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let session = mgr.read_session(&id).unwrap();
        let view = to_view(&session);

        assert_eq!(view.id, "019dabc6-8fef-7681-a054-b5bb75fcb97d");
        assert_eq!(view.provider_id.as_deref(), Some("codex"));
        assert_eq!(view.turns.len(), 3);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "please do a thing");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "working on it");
        assert_eq!(view.turns[1].model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn encrypted_reasoning_does_not_land_on_thinking() {
        // The fixture only has encrypted_content. That must NOT be rendered
        // as `Turn.thinking` (which would be opaque ciphertext). Since
        // Turn.extra was removed, encrypted ciphertext is simply dropped.
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        let assistant = &view.turns[1];
        assert!(
            assistant.thinking.is_none(),
            "encrypted ciphertext must not appear as thinking"
        );
    }

    #[test]
    fn plaintext_reasoning_lands_on_thinking() {
        // Craft a session with a `content[*].text` reasoning item — this
        // is the rare public-reasoning case.
        let body = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"s","timestamp":"t","cwd":"/p","originator":"x","cli_version":"1","source":"cli"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"reasoning","summary":[],"content":[{"type":"text","text":"I should check the file"}]}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]}}"#,
        ]
        .join("\n");
        let (_t, mgr, id) = setup_session_fixture(&body);
        let view = to_view(&mgr.read_session(&id).unwrap());
        assert_eq!(
            view.turns[0].thinking.as_deref(),
            Some("I should check the file")
        );
    }

    #[test]
    fn function_call_pairs_with_output() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        let assistant = &view.turns[1];
        assert_eq!(assistant.tool_uses.len(), 2);
        let exec = &assistant.tool_uses[0];
        assert_eq!(exec.name, "exec_command");
        assert_eq!(exec.category, Some(ToolCategory::Shell));
        assert!(exec.result.is_some());
        assert!(exec.result.as_ref().unwrap().content.contains("/tmp/proj"));
    }

    #[test]
    fn custom_tool_call_preserves_raw_input() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        let assistant = &view.turns[1];
        let apply = &assistant.tool_uses[1];
        assert_eq!(apply.name, "apply_patch");
        assert_eq!(apply.category, Some(ToolCategory::FileWrite));
        let input_str = apply.input.as_str().unwrap();
        assert!(input_str.contains("*** Begin Patch"));
    }

    #[test]
    fn patch_apply_end_aggregates_files_changed() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        assert_eq!(view.files_changed, vec!["/tmp/proj/a.rs".to_string()]);
    }

    #[test]
    fn files_changed_order_is_deterministic() {
        // patch.changes is a HashMap; iteration order in Rust is
        // randomized. Derive must still produce a stable ordering.
        let body = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"s","timestamp":"t","cwd":"/p","originator":"x","cli_version":"1","source":"cli"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"go"}]}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c","name":"apply_patch","input":""}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"c","success":true,"changes":{"/p/z.rs":{"type":"add","content":"z"},"/p/a.rs":{"type":"add","content":"a"},"/p/m.rs":{"type":"add","content":"m"}}}}"#,
        ]
        .join("\n");
        let (_t, mgr, id) = setup_session_fixture(&body);
        let view = to_view(&mgr.read_session(&id).unwrap());
        assert_eq!(
            view.files_changed,
            vec![
                "/p/a.rs".to_string(),
                "/p/m.rs".to_string(),
                "/p/z.rs".to_string(),
            ]
        );
    }

    #[test]
    fn patch_apply_end_populates_turn_file_mutations() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        // Find the turn that hosts the `apply_patch` file mutation. The
        // mutation's `tool_id` should link back to the apply_patch tool.
        let apply_patch_id = view
            .turns
            .iter()
            .flat_map(|t| t.tool_uses.iter())
            .find(|tu| tu.name == "apply_patch")
            .map(|tu| tu.id.clone())
            .expect("apply_patch tool invocation present");
        let fm = view
            .turns
            .iter()
            .flat_map(|t| t.file_mutations.iter())
            .find(|fm| fm.path == "/tmp/proj/a.rs")
            .expect("file mutation present");
        assert_eq!(fm.tool_id.as_ref(), Some(&apply_patch_id));
        assert_eq!(fm.operation.as_deref(), Some("add"));
        assert!(fm.raw_diff.is_some());
    }

    #[test]
    fn total_usage_populated() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        let u = view.total_usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.cache_read_tokens, Some(10));
    }

    #[test]
    fn events_preserve_non_turn_content() {
        let (_t, mgr, id) = setup_session_fixture(&minimal_session());
        let view = to_view(&mgr.read_session(&id).unwrap());
        let kinds: Vec<&str> = view.events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(kinds.contains(&"session_meta"));
        assert!(kinds.contains(&"turn_context"));
        assert!(kinds.contains(&"task_started"));
        assert!(kinds.contains(&"task_complete"));
        assert!(kinds.contains(&"exec_command_end"));
        assert!(kinds.contains(&"patch_apply_end"));
        assert!(kinds.contains(&"token_count"));
    }

    #[test]
    fn tool_category_mapping() {
        assert_eq!(tool_category("exec_command"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("apply_patch"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("read_file"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("grep_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("web_fetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("spawn_agent"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("unknown_xyz"), None);
    }

    #[test]
    fn provider_trait_list_load() {
        let (_t, mgr, _name) = setup_session_fixture(&minimal_session());
        let ids = ConversationProvider::list_conversations(&mgr, "").unwrap();
        // `list_conversations` returns inner session_meta.id, not filename stems.
        assert_eq!(
            ids,
            vec!["019dabc6-8fef-7681-a054-b5bb75fcb97d".to_string()]
        );
        let view = ConversationProvider::load_conversation(
            &mgr,
            "",
            "019dabc6-8fef-7681-a054-b5bb75fcb97d",
        )
        .unwrap();
        assert_eq!(view.turns.len(), 3);
    }

    #[test]
    fn developer_role_becomes_system() {
        let body = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"s","timestamp":"t","cwd":"/","originator":"x","cli_version":"1","source":"cli"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"system instructions"}]}}"#,
        ]
        .join("\n");
        let (_t, mgr, id) = setup_session_fixture(&body);
        let view = to_view(&mgr.read_session(&id).unwrap());
        assert_eq!(view.turns[0].role, Role::System);
    }
}
