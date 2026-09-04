//! [`ClaudeProjector`] — maps a [`ConversationView`] back to a Claude
//! [`Conversation`].
//!
//! This is the inverse of [`crate::provider::to_view`]: where `to_view`
//! reads a Claude JSONL conversation into a provider-agnostic view,
//! `ClaudeProjector` serializes that view back into the Claude wire format.

use crate::provider::{NEXT_TURN_KEY, SOURCE_PARENT_KEY, SourceRoot};
use crate::types::{
    ContentPart, Conversation, ConversationEntry, Message, MessageContent, MessageRole,
    ToolResultContent, Usage,
};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Result, Role, ToolInvocation, Turn,
};

// ── ClaudeProjector ───────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Claude [`Conversation`].
///
/// Maps the provider-agnostic view back into Claude's JSONL wire format.
/// A source tool-result line kept as a `tool_result_user` event is written
/// in its place; a tool use without one gets a synthesized tool-result
/// user entry after its assistant entry.
///
/// # Example
///
/// ```rust
/// use toolpath_claude::project::ClaudeProjector;
/// use toolpath_convo::{ConversationView, ConversationProjector};
///
/// let view = ConversationView {
///     id: "my-session".to_string(),
///     ..Default::default()
/// };
///
/// let projector = ClaudeProjector;
/// let convo = projector.project(&view).unwrap();
/// assert_eq!(convo.session_id, "my-session");
/// ```
pub struct ClaudeProjector;

impl ConversationProjector for ClaudeProjector {
    type Output = Conversation;

    fn project(&self, view: &ConversationView) -> Result<Conversation> {
        project_view(view).map_err(|e| ConvoError::Provider(e.to_string()))
    }
}

// ── Projection logic ─────────────────────────────────────────────────

use crate::provider::TOOL_RESULT_USER_EVENT;

fn project_view(view: &ConversationView) -> std::result::Result<Conversation, String> {
    let mut out = Output::new(view.id.clone());

    // Headerless lines (the JSONL "preamble": ai-title, last-prompt,
    // queue-operation, permission-mode, file-history-snapshot, and anything
    // unrecognized) ride in `view.events` carrying the original line verbatim
    // under `data["raw"]`. Dump them straight back. A headerless event is
    // identified by that `raw` key — no enumerated type list.
    let mut emitted_preamble = false;
    for event in &view.events {
        if let Some(raw) = event.data.get("raw") {
            out.convo.preamble.push(raw.clone());
            emitted_preamble = true;
        }
    }
    // Cross-harness views won't carry a Claude preamble; emit a default
    // permission-mode line so Claude Code can resume them.
    if !emitted_preamble {
        out.convo.preamble.push(json!({
            "type": "permission-mode",
            "permissionMode": "default",
            "sessionId": view.id,
        }));
    }

    // Claude Code writes one chain: every line's `parentUuid` names the
    // line written before it, whatever its type. The IR keeps turn-to-turn
    // parents only, so the last entry written for a turn (its tool
    // results, then the passthrough run that followed them) is recorded
    // here and the next turn hangs off that.
    let mut parent_rewrites: HashMap<String, String> = HashMap::new();
    let with_source_line = tool_uses_with_source_line(view);
    let mut runs = PassthroughRuns::new(view);
    let root_tail = emit_run(
        &mut out,
        runs.before_first_turn(),
        None,
        ParentPolicy::Source,
    );

    // Message-group accounting. The IR carries a message's total
    // `token_usage` only on the group's final turn; real Claude JSONL stamps
    // usage on every line of a split (streaming snapshots that climb to the
    // total — see `provider::canonicalize_message_usage`). We re-expand the
    // group total onto every line, matching the common after-generation
    // pattern where the lines repeat one value. (We don't reconstruct the
    // intermediate streaming snapshots: they carry no per-step meaning, the
    // IR doesn't retain them, and the final total is what consumers sum.)
    let mut group_total: HashMap<&str, toolpath_convo::TokenUsage> = HashMap::new();
    for turn in &view.turns {
        if let (Some(mid), Some(usage)) = (turn.group_id.as_deref(), &turn.token_usage) {
            group_total
                .entry(mid)
                .and_modify(|acc| *acc = crate::provider::max_usage(acc, usage))
                .or_insert_with(|| usage.clone());
        }
    }

    // The line a turn hangs off when it is not the last line written
    // before the turn.
    let next_turn_parent: HashMap<&str, &str> = view
        .events
        .iter()
        .filter_map(|e| Some((e.data.get(NEXT_TURN_KEY)?.as_str()?, e.id.as_str())))
        .collect();

    for (idx, turn) in view.turns.iter().enumerate() {
        let effective_parent = next_turn_parent
            .get(turn.id.as_str())
            .filter(|p| out.has(p))
            .map(|p| p.to_string())
            .or_else(|| match (idx, turn.parent_id.as_ref()) {
                (0, None) => root_tail.clone(),
                (_, Some(pid)) => parent_rewrites
                    .get(pid)
                    .cloned()
                    .or_else(|| Some(pid.clone())),
                (_, None) => None,
            });
        let mut synthesized = false;

        // The last line written for this turn: its own line, then its
        // tool results, then the passthrough run that follows them.
        let mut tail = turn.id.clone();
        match &turn.role {
            Role::User => {
                let mut entry = user_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                out.push(entry);
            }
            Role::Assistant => {
                // Grouped: the message total on every line of the split.
                // Ungrouped: the turn's own usage.
                let wire_usage: Option<toolpath_convo::TokenUsage> = match turn.group_id.as_deref()
                {
                    Some(mid) => group_total.get(mid).cloned(),
                    None => turn.token_usage.clone(),
                };
                let mut assistant_entry =
                    assistant_turn_to_entry_with_usage(turn, &view.id, wire_usage.as_ref());
                apply_turn_metadata(&mut assistant_entry, turn);
                assistant_entry.parent_uuid = effective_parent;
                out.push(assistant_entry);

                // A source tool-result line is written in its place by the
                // run it belongs to. A tool use without one gets a
                // synthesized result line here.
                for mut result_entry in tool_result_entries(turn, &view.id, &with_source_line) {
                    apply_turn_metadata(&mut result_entry, turn);
                    tail = result_entry.uuid.clone();
                    out.push(result_entry);
                    synthesized = true;
                }
            }
            Role::System => {
                let mut entry = system_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                out.push(entry);
            }
            Role::Other(_) => {
                let mut entry = other_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                out.push(entry);
            }
        }
        let policy = if synthesized {
            ParentPolicy::Chain
        } else {
            ParentPolicy::Source
        };
        if let Some(last) = emit_run(&mut out, runs.after(&turn.id), Some(&tail), policy) {
            tail = last;
        }
        if tail != turn.id {
            parent_rewrites.insert(turn.id.clone(), tail);
        }
    }

    Ok(out.convo)
}

/// The projected conversation and the UUIDs written into it so far. A
/// passthrough line keeps its source parent only when that parent is
/// already written.
struct Output {
    convo: Conversation,
    written: HashSet<String>,
}

impl Output {
    fn new(session_id: String) -> Self {
        Self {
            convo: Conversation::new(session_id),
            written: HashSet::new(),
        }
    }

    fn push(&mut self, entry: ConversationEntry) {
        self.written.insert(entry.uuid.clone());
        self.convo.add_entry(entry);
    }

    fn has(&self, uuid: &str) -> bool {
        self.written.contains(uuid)
    }
}

/// Passthrough events (attachments, message-less system lines, source
/// tool-result lines) grouped by the turn they hang off, in view order.
/// `None` holds the run before the first turn.
struct PassthroughRuns<'a> {
    by_anchor: HashMap<Option<&'a str>, Vec<&'a toolpath_convo::ConversationEvent>>,
}

impl<'a> PassthroughRuns<'a> {
    fn new(view: &'a ConversationView) -> Self {
        let turn_ids: HashSet<&str> = view.turns.iter().map(|t| t.id.as_str()).collect();
        let mut root_ids: HashSet<&str> = HashSet::new();
        let mut event_parent: HashMap<&str, Option<&str>> = HashMap::new();
        for event in &view.events {
            let preamble = event.data.contains_key("raw");
            if preamble || SourceRoot::of(event) == Some(SourceRoot::Session) {
                root_ids.insert(event.id.as_str());
            }
            if !preamble {
                event_parent.insert(event.id.as_str(), source_parent(event));
            }
        }
        let mut by_anchor: HashMap<Option<&'a str>, Vec<&'a toolpath_convo::ConversationEvent>> =
            HashMap::new();
        for event in &view.events {
            if event.data.contains_key("raw") {
                continue;
            }
            let anchor = anchor_turn(&view.turns, event, &turn_ids, &root_ids, &event_parent);
            by_anchor.entry(anchor).or_default().push(event);
        }
        Self { by_anchor }
    }

    /// The run before the first turn.
    fn before_first_turn(&mut self) -> Vec<&'a toolpath_convo::ConversationEvent> {
        self.by_anchor.remove(&None).unwrap_or_default()
    }

    /// The run that follows the turn and its tool results.
    fn after(&mut self, turn_id: &'a str) -> Vec<&'a toolpath_convo::ConversationEvent> {
        self.by_anchor.remove(&Some(turn_id)).unwrap_or_default()
    }
}

/// Tool-use IDs whose result line the view kept as a `tool_result_user`
/// event. Every other tool use gets a synthesized result line.
fn tool_uses_with_source_line(view: &ConversationView) -> HashSet<String> {
    view.events
        .iter()
        .filter(|e| e.event_type == TOOL_RESULT_USER_EVENT)
        .filter_map(source_message)
        .flat_map(|msg| {
            msg.tool_results()
                .into_iter()
                .map(|tr| tr.tool_use_id.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The source line's message, when the event carries it.
fn source_message(event: &toolpath_convo::ConversationEvent) -> Option<Message> {
    event
        .data
        .get("message")
        .and_then(|v| serde_json::from_value::<Message>(v.clone()).ok())
}

/// The event's source parent: the one `to_view` recorded when the
/// document round-trip would lose it, else `parent_id`.
fn source_parent(event: &toolpath_convo::ConversationEvent) -> Option<&str> {
    event
        .data
        .get(SOURCE_PARENT_KEY)
        .and_then(|v| v.as_str())
        .or(event.parent_id.as_deref())
}

/// The turn an event hangs off: follow `parent_id` through other events
/// until a turn. A source root (a line that opens the session, or one
/// whose parent is a headerless preamble line) belongs to the run before
/// the first turn. An event whose parent chain ends without a turn (no
/// parent, or a parent that names nothing in the view) falls back to
/// the last turn at or before the event's timestamp.
fn anchor_turn<'a>(
    turns: &'a [toolpath_convo::Turn],
    event: &'a toolpath_convo::ConversationEvent,
    turn_ids: &HashSet<&'a str>,
    root_ids: &HashSet<&'a str>,
    event_parent: &HashMap<&'a str, Option<&'a str>>,
) -> Option<&'a str> {
    if root_ids.contains(event.id.as_str()) {
        return None;
    }
    let mut pid = source_parent(event);
    for _ in 0..=event_parent.len() {
        let Some(p) = pid else {
            break;
        };
        if turn_ids.contains(p) {
            return Some(p);
        }
        if root_ids.contains(p) {
            return None;
        }
        match event_parent.get(p) {
            Some(next) => pid = *next,
            None => break,
        }
    }
    turns
        .iter()
        .rev()
        .find(|t| at_or_before(&t.timestamp, &event.timestamp))
        .map(|t| t.id.as_str())
}

/// `turn_ts <= event_ts` on RFC 3339 timestamps. An event timestamp that
/// does not parse matches every turn, so the last turn wins; a turn
/// timestamp that does not parse compares as text.
fn at_or_before(turn_ts: &str, event_ts: &str) -> bool {
    use chrono::DateTime;
    let Ok(event) = DateTime::parse_from_rfc3339(event_ts) else {
        return true;
    };
    match DateTime::parse_from_rfc3339(turn_ts) {
        Ok(turn) => turn <= event,
        Err(_) => turn_ts <= event_ts,
    }
}

/// How a passthrough entry picks its `parentUuid`.
#[derive(Clone, Copy)]
enum ParentPolicy {
    /// The source parent when that line is already written, which
    /// reproduces the harness's own shape (a hook line as a side leaf off
    /// a tool call). Otherwise the line written before it.
    Source,
    /// The line written before it, always. Used after a synthesized
    /// tool-result line: a hook line that kept the tool-use line as its
    /// parent would leave the synthesized line a leaf, and the loader can
    /// start from any leaf.
    Chain,
}

/// Write a passthrough run after `prev`: the first entry hangs off `prev`
/// and each later entry off the one before it, unless `policy` keeps a
/// source parent. A source root has no parent and the chain continues
/// from it. Returns the last entry written, if the run wrote any.
fn emit_run(
    out: &mut Output,
    run: Vec<&toolpath_convo::ConversationEvent>,
    prev: Option<&str>,
    policy: ParentPolicy,
) -> Option<String> {
    let mut last: Option<String> = None;
    for event in run {
        let mut entry = if event.event_type == TOOL_RESULT_USER_EVENT {
            tool_result_event_to_entry(event, &out.convo.session_id)
        } else {
            project_event(event, &out.convo.session_id)
        };
        let kept = match policy {
            ParentPolicy::Source => source_parent(event).filter(|p| out.has(p)),
            ParentPolicy::Chain => None,
        };
        entry.parent_uuid = match SourceRoot::of(event) {
            Some(_) => None,
            None => kept.or(last.as_deref()).or(prev).map(str::to_string),
        };
        last = Some(entry.uuid.clone());
        out.push(entry);
    }
    last
}

/// Rebuild a Claude tool-result user entry from a preserved event: the
/// source UUID, parent UUID, `toolUseResult` blob, `entry_extra` fields,
/// and the message.
fn tool_result_event_to_entry(
    event: &toolpath_convo::ConversationEvent,
    session_id: &str,
) -> ConversationEntry {
    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(map) = event.data.get("entry_extra").and_then(|v| v.as_object()) {
        for (k, v) in map {
            extra.insert(k.clone(), v.clone());
        }
    }

    ConversationEntry {
        uuid: event.id.clone(),
        parent_uuid: event.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: event.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: event
            .data
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        git_branch: event
            .data
            .get("git_branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        message: source_message(event),
        version: event
            .data
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        user_type: event
            .data
            .get("user_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        request_id: None,
        tool_use_result: event.data.get("tool_use_result").cloned(),
        snapshot: None,
        message_id: event
            .data
            .get("message_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        extra,
    }
}

/// Apply Claude-specific metadata from a [`Turn`] onto a [`ConversationEntry`].
///
/// Populates `cwd` and `git_branch` from [`Turn::environment`], and
/// `version`, `user_type`, `request_id` from `Turn::extra["claude"]`.
/// Remaining keys from the `"claude"` extras are merged into the entry's
/// own `extra` map so they serialize as top-level fields (via `#[serde(flatten)]`).
fn apply_turn_metadata(entry: &mut ConversationEntry, turn: &Turn) {
    // From Turn.environment
    if let Some(env) = &turn.environment {
        if entry.cwd.is_none() {
            entry.cwd = env.working_dir.clone();
        }
        if entry.git_branch.is_none() {
            entry.git_branch = env.vcs_branch.clone();
        }
    }

    // Source-format details (`version`, `user_type`, `request_id`,
    // per-entry catch-all) used to ride through `Turn.extra["claude"]` for
    // claude → IR → claude round-trip. The IR no longer carries
    // provider-specific extras; the projected entry's fields stay `None`
    // and the harness fills in defaults at write time.
}

/// Build a `ConversationEntry` for a user turn.
fn user_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    let content = MessageContent::Text(turn.text.clone());

    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: turn
            .environment
            .as_ref()
            .and_then(|e| e.working_dir.clone()),
        git_branch: turn.environment.as_ref().and_then(|e| e.vcs_branch.clone()),
        message: Some(Message {
            role: MessageRole::User,
            content: Some(content),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build a `ConversationEntry` for an assistant turn. `wire_usage` is the
/// usage to write on the JSONL line: the IR carries a message's total only
/// on the group's final turn, but real Claude Code repeats `message.usage`
/// on every line of a split message, so `project_view` passes the group
/// total for every member turn.
fn assistant_turn_to_entry_with_usage(
    turn: &Turn,
    session_id: &str,
    wire_usage: Option<&toolpath_convo::TokenUsage>,
) -> ConversationEntry {
    let content = build_assistant_content(turn);

    let usage = wire_usage.map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        // TokenUsage uses cache_write_tokens; Usage uses cache_creation_input_tokens
        cache_creation_input_tokens: u.cache_write_tokens,
        cache_read_input_tokens: u.cache_read_tokens,
        cache_creation: None,
        service_tier: None,
    });

    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "assistant".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::Assistant,
            content: Some(content),
            model: turn.model.clone(),
            id: turn.group_id.clone(),
            message_type: None,
            stop_reason: turn.stop_reason.clone(),
            stop_sequence: None,
            usage,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build the `MessageContent` for an assistant turn.
///
/// If the turn has ONLY text (no thinking, no tool_uses): returns
/// `MessageContent::Text`. Otherwise builds `MessageContent::Parts`.
fn build_assistant_content(turn: &Turn) -> MessageContent {
    let has_thinking = turn.thinking.is_some();
    let has_tool_uses = !turn.tool_uses.is_empty();

    if !has_thinking && !has_tool_uses {
        // Claude Code expects assistant content to always be an array,
        // even for simple text-only responses.
        return MessageContent::Parts(vec![ContentPart::Text {
            text: turn.text.clone(),
        }]);
    }

    let mut parts: Vec<ContentPart> = Vec::new();

    if let Some(thinking) = &turn.thinking {
        parts.push(ContentPart::Thinking {
            thinking: thinking.clone(),
            signature: None,
        });
    }

    if !turn.text.is_empty() {
        parts.push(ContentPart::Text {
            text: turn.text.clone(),
        });
    }

    for tu in &turn.tool_uses {
        // Rename non-Claude tools to Claude's canonical names so the UI's
        // tool-specific renderers (Bash output panel, Edit diff view, Read
        // file viewer, Glob/Grep result lists) actually fire. Without
        // this, names like Codex's `exec_command` / `read_file` /
        // `write_file` come through as opaque blocks even when their
        // toolUseResult is well-formed.
        let name = canonical_claude_tool_name(tu);
        let input = canonical_claude_tool_input(tu, &name);
        parts.push(ContentPart::ToolUse {
            id: tu.id.clone(),
            name,
            input,
        });
    }

    MessageContent::Parts(parts)
}

/// Pick Claude's native tool name. Same shape as `tool_native_name` on
/// codex / opencode / gemini / pi: keep the source name when it's
/// already Claude-canonical, otherwise route through the IR's
/// `category` plus [`crate::provider::native_name`] to land on a Claude
/// tool name; otherwise pass through verbatim. The rename makes
/// Claude's UI fire its rich result panes (diff view, shell output
/// box, file viewer) for cross-harness sources.
fn canonical_claude_tool_name(tu: &ToolInvocation) -> String {
    if crate::provider::tool_category(&tu.name).is_some() {
        return tu.name.clone();
    }
    if let Some(cat) = tu.category
        && let Some(remap) = crate::provider::native_name(cat, &tu.input)
    {
        return remap.to_string();
    }
    tu.name.clone()
}

/// Translate a non-Claude tool input map to Claude's expected input keys.
///
/// Claude's UI renders rich panes by reading specific keys from the input
/// (`Bash` reads `command`, `Edit` reads `file_path`/`old_string`/
/// `new_string`, `Read` reads `file_path`). Cross-harness inputs use
/// different keys (`path` vs `file_path`, etc.); without renaming, the
/// pane has nothing to display.
fn canonical_claude_tool_input(tu: &ToolInvocation, claude_name: &str) -> serde_json::Value {
    let get_str = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(v) = tu.input.get(*k).and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
        None
    };
    let path_alts = ["file_path", "filePath", "path", "absolute_path", "filename"];
    match claude_name {
        "Bash" => {
            let mut obj = serde_json::Map::new();
            if let Some(cmd) = get_str(&["command", "cmd"]) {
                obj.insert("command".into(), serde_json::Value::String(cmd));
            }
            if let Some(desc) = get_str(&["description", "summary"]) {
                obj.insert("description".into(), serde_json::Value::String(desc));
            }
            if !obj.is_empty() {
                serde_json::Value::Object(obj)
            } else {
                tu.input.clone()
            }
        }
        "Read" => {
            let mut obj = serde_json::Map::new();
            if let Some(p) = get_str(&path_alts) {
                obj.insert("file_path".into(), serde_json::Value::String(p));
            }
            if let Some(off) = tu.input.get("offset").or_else(|| tu.input.get("startLine")) {
                obj.insert("offset".into(), off.clone());
            }
            if let Some(lim) = tu.input.get("limit").or_else(|| tu.input.get("numLines")) {
                obj.insert("limit".into(), lim.clone());
            }
            if !obj.is_empty() {
                serde_json::Value::Object(obj)
            } else {
                tu.input.clone()
            }
        }
        "Write" => {
            let mut obj = serde_json::Map::new();
            if let Some(p) = get_str(&path_alts) {
                obj.insert("file_path".into(), serde_json::Value::String(p));
            }
            if let Some(c) = get_str(&["content", "text"]) {
                obj.insert("content".into(), serde_json::Value::String(c));
            }
            if !obj.is_empty() {
                serde_json::Value::Object(obj)
            } else {
                tu.input.clone()
            }
        }
        "Edit" | "MultiEdit" => {
            let mut obj = serde_json::Map::new();
            if let Some(p) = get_str(&path_alts) {
                obj.insert("file_path".into(), serde_json::Value::String(p));
            }
            if let Some(o) = get_str(&["old_string", "oldString"]) {
                obj.insert("old_string".into(), serde_json::Value::String(o));
            }
            if let Some(n) = get_str(&["new_string", "newString"]) {
                obj.insert("new_string".into(), serde_json::Value::String(n));
            }
            if let Some(r) = tu
                .input
                .get("replace_all")
                .or_else(|| tu.input.get("replaceAll"))
            {
                obj.insert("replace_all".into(), r.clone());
            }
            if !obj.is_empty() {
                serde_json::Value::Object(obj)
            } else {
                tu.input.clone()
            }
        }
        "Glob" | "Grep" => {
            let mut obj = serde_json::Map::new();
            if let Some(p) = get_str(&["pattern", "query", "regex"]) {
                obj.insert("pattern".into(), serde_json::Value::String(p));
            }
            if let Some(p) = get_str(&path_alts) {
                obj.insert("path".into(), serde_json::Value::String(p));
            }
            if !obj.is_empty() {
                serde_json::Value::Object(obj)
            } else {
                tu.input.clone()
            }
        }
        _ => tu.input.clone(),
    }
}

/// Build one tool-result user entry per `ToolInvocation` that has a result.
///
/// Claude's wire format uses a separate user entry per tool result so that
/// the top-level `toolUseResult` field (which carries the rich UI display
/// blob — Bash stdout/stderr, Edit's structuredPatch, etc.) is unambiguous.
fn tool_result_entries(
    turn: &Turn,
    session_id: &str,
    with_source_line: &HashSet<String>,
) -> Vec<ConversationEntry> {
    turn.tool_uses
        .iter()
        .filter(|tu| !with_source_line.contains(&tu.id))
        .filter_map(|tu| {
            let result = tu.result.as_ref()?;
            let part = ContentPart::ToolResult {
                tool_use_id: tu.id.clone(),
                content: ToolResultContent::Text(result.content.clone()),
                is_error: result.is_error,
            };

            let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
            extra.insert("sourceToolAssistantUUID".to_string(), json!(turn.id));

            Some(ConversationEntry {
                uuid: format!("{}-result-{}", turn.id, tu.id),
                parent_uuid: Some(turn.id.clone()),
                is_sidechain: false,
                entry_type: "user".to_string(),
                timestamp: turn.timestamp.clone(),
                session_id: Some(session_id.to_string()),
                cwd: None,
                git_branch: None,
                message: Some(Message {
                    role: MessageRole::User,
                    content: Some(MessageContent::Parts(vec![part])),
                    model: None,
                    id: None,
                    message_type: None,
                    stop_reason: None,
                    stop_sequence: None,
                    usage: None,
                }),
                version: None,
                user_type: None,
                request_id: None,
                tool_use_result: tool_use_result_from_invocation(tu),
                snapshot: None,
                message_id: None,
                extra,
            })
        })
        .collect()
}

/// Reconstruct Claude's `toolUseResult` JSON from the tool's name, input,
/// and result content.
///
/// This is what drives Claude Code's diff view, shell output box, and
/// agent stat panel. The data was already in the IR — `name` says what
/// kind of tool it was, `input` carries the args (file path, command,
/// pattern, …), and `result.content` is the model-visible text. We just
/// switch on the name and fan the fields out to Claude's expected shape.
///
/// For unrecognized tools we fall back to the content as a plain string
/// so the UI at least shows *something*.
fn tool_use_result_from_invocation(tu: &ToolInvocation) -> Option<serde_json::Value> {
    use toolpath_convo::ToolCategory;

    let str_field = |k: &str| -> Option<String> {
        tu.input
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    // Cross-harness tool inputs name the same concept differently —
    // Claude has `file_path`, Codex has `path`, Gemini has `absolute_path`,
    // opencode uses `filePath` / `path` depending on tool. Try each.
    let path_field = || -> Option<String> {
        ["file_path", "filePath", "path", "absolute_path", "filename"]
            .iter()
            .find_map(|k| str_field(k))
    };
    let result_text = || {
        tu.result
            .as_ref()
            .map(|r| r.content.clone())
            .unwrap_or_default()
    };

    // Pick the dispatch key: name takes precedence (lets us recognize
    // Claude-specific quirks like Read's offset/limit), category is the
    // cross-harness fallback so Codex's `exec_command` and opencode's
    // `bash` and Pi's `bash` all land on the shell shape.
    enum Kind {
        Shell,
        Write,
        Edit,
        Read,
        Search,
        Other,
    }
    let kind = match tu.name.as_str() {
        "Bash" => Kind::Shell,
        "Write" => Kind::Write,
        "Edit" | "MultiEdit" => Kind::Edit,
        "Read" => Kind::Read,
        "Glob" | "Grep" => Kind::Search,
        _ => match tu.category {
            Some(ToolCategory::Shell) => Kind::Shell,
            Some(ToolCategory::FileWrite) => {
                // Disambiguate write vs edit by input shape: presence of
                // `old_string` signals an in-place edit.
                if tu.input.get("old_string").is_some() || tu.input.get("oldString").is_some() {
                    Kind::Edit
                } else {
                    Kind::Write
                }
            }
            Some(ToolCategory::FileRead) => Kind::Read,
            Some(ToolCategory::FileSearch) => Kind::Search,
            _ => Kind::Other,
        },
    };

    match kind {
        Kind::Shell => Some(json!({
            "stdout": result_text(),
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        })),
        Kind::Write => {
            let path = path_field()?;
            let content = str_field("content").unwrap_or_default();
            Some(json!({
                "type": "update",
                "filePath": path,
                "content": content,
            }))
        }
        Kind::Edit => {
            let path = path_field()?;
            let old = str_field("old_string")
                .or_else(|| str_field("oldString"))
                .unwrap_or_default();
            let new_ = str_field("new_string")
                .or_else(|| str_field("newString"))
                .unwrap_or_default();
            let replace_all = tu
                .input
                .get("replace_all")
                .or_else(|| tu.input.get("replaceAll"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(json!({
                "filePath": path,
                "oldString": old,
                "newString": new_,
                "originalFile": "",
                "replaceAll": replace_all,
                "userModified": false,
                "structuredPatch": structured_patch_hunks(&old, &new_),
            }))
        }
        Kind::Read => {
            let Some(path) = path_field() else {
                return Some(json!(result_text()));
            };
            let content = result_text();
            let stripped = strip_line_numbers(&content);
            let total_lines = stripped.lines().count();
            Some(json!({
                "type": "text",
                "file": {
                    "filePath": path,
                    "content": stripped,
                    "numLines": total_lines,
                    "startLine": tu.input.get("offset").and_then(|v| v.as_u64()).unwrap_or(1),
                    "totalLines": total_lines,
                }
            }))
        }
        Kind::Search => {
            let pattern = str_field("pattern")
                .or_else(|| str_field("query"))
                .unwrap_or_default();
            let filenames: Vec<String> = tu
                .result
                .as_ref()
                .map(|r| {
                    r.content
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let num_files = filenames.len();
            Some(json!({
                "filenames": filenames,
                "numFiles": num_files,
                "pattern": pattern,
            }))
        }
        Kind::Other => tu.result.as_ref().map(|r| json!(r.content)),
    }
}

/// Build Claude's `structuredPatch` array — a list of hunks each containing
/// `{oldStart, oldLines, newStart, newLines, lines: ["-…", "+…", " …"]}` —
/// from `old_string` and `new_string`.
///
/// Drives the side-by-side diff view in Claude Code's UI. Without this the
/// diff panel renders empty even though the change went through.
fn structured_patch_hunks(old: &str, new_: &str) -> serde_json::Value {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new_);
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(3) {
        if group.is_empty() {
            continue;
        }
        let first = group.first().unwrap();
        let last = group.last().unwrap();
        let old_start = first.old_range().start + 1;
        let old_lines = last.old_range().end - first.old_range().start;
        let new_start = first.new_range().start + 1;
        let new_lines = last.new_range().end - first.new_range().start;
        let mut lines: Vec<String> = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                let text: &str = change.value();
                let trimmed = text.trim_end_matches('\n');
                lines.push(format!("{prefix}{trimmed}"));
            }
        }
        hunks.push(json!({
            "oldStart": old_start,
            "oldLines": old_lines,
            "newStart": new_start,
            "newLines": new_lines,
            "lines": lines,
        }));
    }
    serde_json::Value::Array(hunks)
}

/// Strip Claude's `cat -n`-style line numbering ("    1\tfoo") from a Read
/// result so the round-tripped `file.content` is the raw file text.
/// Leaves content alone when no leading-number pattern is detected.
fn strip_line_numbers(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut all_match = !lines.is_empty();
    for line in &lines {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars();
        let has_digit = chars.next().map(|c| c.is_ascii_digit()).unwrap_or(false);
        let has_tab = trimmed.contains('\t');
        if !has_digit || !has_tab {
            all_match = false;
            break;
        }
    }
    if !all_match {
        return s.to_string();
    }
    lines
        .iter()
        .map(|l| {
            let trimmed = l.trim_start();
            match trimmed.find('\t') {
                Some(idx) => trimmed[idx + 1..].to_string(),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build a user entry for a System turn.
fn system_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build a user entry for an Other-role turn.
fn other_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    ConversationEntry {
        uuid: turn.id.clone(),
        parent_uuid: turn.parent_id.clone(),
        is_sidechain: false,
        entry_type: "user".to_string(),
        timestamp: turn.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text(turn.text.clone())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: Default::default(),
    }
}

/// Build a `ConversationEntry` from a [`ConversationEvent`].
///
/// Reconstructs the original JSONL entry from the event's data map.
/// For system events with text, a message is created.
fn project_event(event: &toolpath_convo::ConversationEvent, session_id: &str) -> ConversationEntry {
    let mut extra = HashMap::new();

    // Extract entry_extra and merge into top-level extras
    if let Some(entry_extra) = event.data.get("entry_extra").and_then(|v| v.as_object()) {
        for (k, v) in entry_extra {
            extra.insert(k.clone(), v.clone());
        }
    }

    // If the event has text (system messages), create a message
    let message = event
        .data
        .get("text")
        .and_then(|v| v.as_str())
        .map(|text| Message {
            role: if event.event_type == "system" {
                MessageRole::System
            } else {
                MessageRole::User
            },
            content: Some(MessageContent::Text(text.to_string())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        });

    ConversationEntry {
        uuid: event.id.clone(),
        entry_type: event.event_type.clone(),
        timestamp: event.timestamp.clone(),
        session_id: Some(session_id.into()),
        parent_uuid: event.parent_id.clone(),
        is_sidechain: false,
        message,
        cwd: event
            .data
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        git_branch: event
            .data
            .get("git_branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version: event
            .data
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        user_type: event
            .data
            .get("user_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        request_id: None,
        tool_use_result: event.data.get("tool_use_result").cloned(),
        snapshot: event.data.get("snapshot").cloned(),
        message_id: event
            .data
            .get("message_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        extra,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{EnvironmentSnapshot, TokenUsage, ToolResult};

    fn make_view(id: &str, turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: id.to_string(),
            started_at: None,
            last_activity: None,
            turns,
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
            ..Default::default()
        }
    }

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn assistant_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    /// Helper: return all conversation entries (preamble is separate).
    fn content_entries(convo: &Conversation) -> &[ConversationEntry] {
        &convo.entries
    }

    // ── Message-group usage re-expansion ─────────────────────────────

    #[test]
    fn test_projector_reexpands_group_usage_onto_every_line() {
        // The IR carries a message's total only on the group's final turn;
        // real Claude Code JSONL repeats `message.usage` (and `message.id`)
        // on every line of the split. The projector must restore that.
        let usage = toolpath_convo::TokenUsage {
            input_tokens: Some(6),
            output_tokens: Some(997),
            cache_read_tokens: Some(14_842),
            cache_write_tokens: Some(429_831),
            ..Default::default()
        };
        let mut a1 = assistant_turn("a1", "Working on it.");
        a1.group_id = Some("msg_A".into());
        let mut a2 = assistant_turn("a2", "");
        a2.group_id = Some("msg_A".into());
        a2.token_usage = Some(usage);

        let view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let assistants: Vec<&ConversationEntry> = content_entries(&convo)
            .iter()
            .filter(|e| e.entry_type == "assistant")
            .collect();
        assert_eq!(assistants.len(), 2);
        for entry in &assistants {
            let msg = entry.message.as_ref().unwrap();
            assert_eq!(msg.id.as_deref(), Some("msg_A"));
            let u = msg.usage.as_ref().expect("every line carries usage");
            assert_eq!(u.output_tokens, Some(997));
            assert_eq!(u.cache_creation_input_tokens, Some(429_831));
        }
    }

    #[test]
    fn test_group_total_survives_projector_roundtrip() {
        // The IR carries a message's total on the group's final turn only.
        // The projector re-expands it onto every line of the split (with the
        // shared message.id); re-reading collapses it back to the same total
        // on the final turn. Summing per step yields the session total
        // either way.
        let mut a1 = assistant_turn("a1", "first");
        a1.group_id = Some("msg_A".into());
        let mut a2 = assistant_turn("a2", "second");
        a2.group_id = Some("msg_A".into());
        a2.token_usage = Some(toolpath_convo::TokenUsage {
            input_tokens: Some(6),
            output_tokens: Some(164),
            cache_read_tokens: Some(100),
            cache_write_tokens: Some(200),
            ..Default::default()
        });

        let view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2]);
        let convo = ClaudeProjector.project(&view).unwrap();

        // Wire: the total is stamped on every line of the split, each tagged
        // with the shared message.id.
        for entry in content_entries(&convo)
            .iter()
            .filter(|e| e.entry_type == "assistant")
        {
            let msg = entry.message.as_ref().unwrap();
            assert_eq!(msg.id.as_deref(), Some("msg_A"));
            assert_eq!(msg.usage.as_ref().unwrap().output_tokens, Some(164));
        }

        // Re-read: total back on the final turn only; no fabricated attribution.
        let back = crate::provider::to_view(&convo);
        let a: Vec<&Turn> = back
            .turns
            .iter()
            .filter(|t| t.role == Role::Assistant)
            .collect();
        assert!(a[0].token_usage.is_none());
        assert_eq!(a[1].token_usage.as_ref().unwrap().output_tokens, Some(164));
        assert!(a.iter().all(|t| t.attributed_token_usage.is_none()));
    }

    // ── Permission-mode preamble ─────────────────────────────────────

    #[test]
    fn test_permission_mode_in_preamble() {
        let view = make_view("sess-1", vec![user_turn("u1", "Hello")]);
        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(convo.preamble.len(), 1);
        let perm = &convo.preamble[0];
        assert_eq!(perm["type"], "permission-mode");
        assert_eq!(perm["permissionMode"], "default");
        assert_eq!(perm["sessionId"], "sess-1");
        // Should NOT have uuid, timestamp, isSidechain etc.
        assert!(perm.get("uuid").is_none());
        assert!(perm.get("timestamp").is_none());
    }

    // ── Test 1: Basic conversation (user + assistant, no tools) ───────

    #[test]
    fn test_basic_conversation_entry_count_and_content() {
        let view = make_view(
            "sess-1",
            vec![user_turn("u1", "Hello"), assistant_turn("a1", "Hi there!")],
        );
        let projector = ClaudeProjector;
        let convo = projector.project(&view).unwrap();

        assert_eq!(convo.session_id, "sess-1");
        let entries = content_entries(&convo);
        assert_eq!(entries.len(), 2);

        let user_entry = &entries[0];
        assert_eq!(user_entry.entry_type, "user");
        assert_eq!(user_entry.uuid, "u1");
        let msg = user_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.text(), "Hello");

        let asst_entry = &entries[1];
        assert_eq!(asst_entry.entry_type, "assistant");
        assert_eq!(asst_entry.uuid, "a1");
        let msg = asst_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.text(), "Hi there!");
        // Claude Code requires assistant content to always be an array
        assert!(matches!(msg.content, Some(MessageContent::Parts(_))));
    }

    // ── Test 2: User turn with environment → cwd and git_branch ──────

    #[test]
    fn test_user_turn_with_environment() {
        let mut turn = user_turn("u1", "Hello");
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/my/project".to_string()),
            vcs_branch: Some("feat/auth".to_string()),
            vcs_revision: None,
        });

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        assert_eq!(entry.cwd.as_deref(), Some("/my/project"));
        assert_eq!(entry.git_branch.as_deref(), Some("feat/auth"));
    }

    // ── Test 3: Assistant with thinking + text + tool_use → Parts ────

    #[test]
    fn test_assistant_thinking_text_tool_use_produces_parts() {
        let mut turn = assistant_turn("a1", "I'll read the file.");
        turn.thinking = Some("Hmm, need to read the file first.".to_string());
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "src/main.rs"}),
            result: None,
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        // One assistant entry (no results → no tool-result entry)
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let msg = entry.message.as_ref().unwrap();

        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 3);
                // Order: Thinking, Text, ToolUse
                assert!(matches!(parts[0], ContentPart::Thinking { .. }));
                assert!(matches!(parts[1], ContentPart::Text { .. }));
                assert!(matches!(parts[2], ContentPart::ToolUse { .. }));

                if let ContentPart::Thinking { thinking, .. } = &parts[0] {
                    assert_eq!(thinking, "Hmm, need to read the file first.");
                }
                if let ContentPart::Text { text } = &parts[1] {
                    assert_eq!(text, "I'll read the file.");
                }
                if let ContentPart::ToolUse { id, name, .. } = &parts[2] {
                    assert_eq!(id, "t1");
                    assert_eq!(name, "Read");
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Test 4: Simple text-only assistant → always Parts (Claude Code requires arrays) ─

    #[test]
    fn test_simple_text_only_assistant_produces_parts_array() {
        let turn = assistant_turn("a1", "Just a plain answer.");

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        let msg = entry.message.as_ref().unwrap();
        // Claude Code expects assistant content to always be an array
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert!(
                    matches!(&parts[0], ContentPart::Text { text } if text == "Just a plain answer.")
                );
            }
            other => panic!("Expected Parts([Text]), got {:?}", other),
        }
    }

    // ── Test 5: Tool results emitted as separate user entries ─────────

    #[test]
    fn test_tool_results_emitted_as_separate_user_entries() {
        let mut turn = assistant_turn("a1", "Reading file.");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "src/main.rs"}),
            result: Some(ToolResult {
                content: "fn main() {}".to_string(),
                is_error: false,
            }),
            category: None,
        }];

        let view = make_view("sess-1", vec![user_turn("u1", "Go"), turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        // user + assistant + tool-result user
        assert_eq!(entries.len(), 3);

        let result_entry = &entries[2];
        assert_eq!(result_entry.entry_type, "user");
        assert_eq!(result_entry.uuid, "a1-result-t1");
        assert_eq!(result_entry.parent_uuid.as_deref(), Some("a1"));

        let msg = result_entry.message.as_ref().unwrap();
        assert_eq!(msg.role, MessageRole::User);

        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t1");
                        assert_eq!(content.text(), "fn main() {}");
                        assert!(!is_error);
                    }
                    other => panic!("Expected ToolResult, got {:?}", other),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Test 6: No tool result entry when tool uses have no results ───

    #[test]
    fn test_no_tool_result_entry_when_no_results() {
        let mut turn = assistant_turn("a1", "Reading...");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
            result: None, // no result
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        // Only the assistant entry, no tool-result entry
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "assistant");
    }

    // ── Test 7: Token usage mapped correctly (cache field name swap) ──

    #[test]
    fn test_token_usage_mapped_correctly_with_cache_swap() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: Some(500),  // → cache_read_input_tokens
            cache_write_tokens: Some(200), // → cache_creation_input_tokens
            ..Default::default()
        });

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
        let usage = msg.usage.as_ref().unwrap();

        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.cache_read_input_tokens, Some(500));
        assert_eq!(usage.cache_creation_input_tokens, Some(200));
    }

    // ── Test 8: Session ID and parent chain preserved ─────────────────

    #[test]
    fn test_session_id_and_parent_chain_preserved() {
        let mut t2 = assistant_turn("a1", "Reply");
        t2.parent_id = Some("u1".to_string());
        let mut t3 = user_turn("u2", "Second");
        t3.parent_id = Some("a1".to_string());

        let view = make_view("my-session", vec![user_turn("u1", "First"), t2, t3]);
        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(convo.session_id, "my-session");
        for entry in &convo.entries {
            assert_eq!(entry.session_id.as_deref(), Some("my-session"));
        }

        let entries = content_entries(&convo);
        assert_eq!(entries[0].parent_uuid, None);
        assert_eq!(entries[1].parent_uuid.as_deref(), Some("u1"));
        assert_eq!(entries[2].parent_uuid.as_deref(), Some("a1"));
    }

    // ── Passthrough runs rejoin the chain ─────────────────────────────

    fn attachment(id: &str, parent: Option<&str>, ts: &str) -> toolpath_convo::ConversationEvent {
        toolpath_convo::ConversationEvent {
            id: id.to_string(),
            timestamp: ts.to_string(),
            parent_id: parent.map(str::to_string),
            event_type: "attachment".to_string(),
            data: HashMap::new(),
        }
    }

    fn tool_use_with_result(id: &str) -> ToolInvocation {
        ToolInvocation {
            id: id.to_string(),
            name: "Bash".to_string(),
            input: json!({"command": "ls"}),
            result: Some(toolpath_convo::ToolResult {
                content: "a.rs".to_string(),
                is_error: false,
            }),
            category: None,
        }
    }

    fn ids(convo: &Conversation) -> Vec<&str> {
        convo.entries.iter().map(|e| e.uuid.as_str()).collect()
    }

    fn parent_of<'a>(convo: &'a Conversation, id: &str) -> Option<&'a str> {
        convo
            .entries
            .iter()
            .find(|e| e.uuid == id)
            .and_then(|e| e.parent_uuid.as_deref())
    }

    /// One chain: the first entry has no parent, every other entry names
    /// the entry before it, so no entry is a leaf except the last.
    fn assert_one_chain(convo: &Conversation) {
        let entries = &convo.entries;
        assert_eq!(entries[0].parent_uuid, None);
        for pair in entries.windows(2) {
            assert_eq!(
                pair[1].parent_uuid.as_deref(),
                Some(pair[0].uuid.as_str()),
                "{} must hang off {}",
                pair[1].uuid,
                pair[0].uuid
            );
        }
    }

    #[test]
    fn test_passthrough_runs_rejoin_the_chain_in_source_order() {
        // Source order, `<-` reads "is the parent of":
        // r1 <- u1 <- n1 <- n2 <- a1 <- (tool result) <- h1 <- n3 <- a2 <- s1 <- u2
        let mut a1 = assistant_turn("a1", "");
        a1.parent_id = Some("u1".into());
        a1.tool_uses = vec![tool_use_with_result("t1")];
        let mut a2 = assistant_turn("a2", "Done.");
        a2.parent_id = Some("a1".into());
        a2.timestamp = "2024-01-01T00:00:02Z".into();
        let mut u2 = user_turn("u2", "Next");
        u2.parent_id = Some("a2".into());
        u2.timestamp = "2024-01-01T00:00:03Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2, u2]);
        let mut r1 = attachment("r1", None, "2024-01-01T00:00:00Z");
        SourceRoot::Session.mark(&mut r1);
        view.events = vec![
            r1,
            attachment("n1", Some("u1"), "2024-01-01T00:00:00Z"),
            attachment("n2", Some("n1"), "2024-01-01T00:00:00Z"),
            attachment("h1", Some("a1"), "2024-01-01T00:00:01Z"),
            attachment("n3", Some("h1"), "2024-01-01T00:00:01Z"),
            attachment("s1", Some("a2"), "2024-01-01T00:00:02Z"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(
            ids(&convo),
            [
                "r1",
                "u1",
                "n1",
                "n2",
                "a1",
                "a1-result-t1",
                "h1",
                "n3",
                "a2",
                "s1",
                "u2"
            ]
        );
        assert_one_chain(&convo);
    }

    #[test]
    fn test_parallel_tool_calls_get_no_synthesized_result_line() {
        // Claude Code writes each tool_use of one message as its own
        // assistant line and every result after the last one:
        // u1 <- a1 <- a2 <- t1 <- t2 <- a3
        let mut a1 = assistant_turn("a1", "");
        a1.parent_id = Some("u1".into());
        a1.tool_uses = vec![tool_use_with_result("toolu_1")];
        let mut a2 = assistant_turn("a2", "");
        a2.parent_id = Some("a1".into());
        a2.tool_uses = vec![tool_use_with_result("toolu_2")];
        let mut a3 = assistant_turn("a3", "Done.");
        a3.parent_id = Some("a2".into());
        a3.timestamp = "2024-01-01T00:00:02Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2, a3]);
        view.events = vec![
            tool_result_event("t1", "a2", "toolu_1"),
            tool_result_event("t2", "t1", "toolu_2"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "a1", "a2", "t1", "t2", "a3"]);
        assert_one_chain(&convo);
    }

    /// A `tool_result_user` event as the derive writes it: the source
    /// line's message.
    fn tool_result_event(
        id: &str,
        parent: &str,
        tool_use_id: &str,
    ) -> toolpath_convo::ConversationEvent {
        let mut tr = attachment(id, Some(parent), "2024-01-01T00:00:01Z");
        tr.event_type = TOOL_RESULT_USER_EVENT.to_string();
        tr.data.insert(
            "message".into(),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use_id, "content": "a.rs"}]}),
        );
        tr
    }

    #[test]
    fn test_turn_hangs_off_the_line_the_source_named_not_the_run_tail() {
        // Source: u1 <- a1 <- t1 (result), a1 <- hp (side leaf, written
        // after t1), t1 <- a2. The run for a1 ends in the side leaf.
        let mut a1 = assistant_turn("a1", "");
        a1.parent_id = Some("u1".into());
        a1.tool_uses = vec![tool_use_with_result("t")];
        let mut a2 = assistant_turn("a2", "Done.");
        a2.parent_id = Some("a1".into());
        a2.timestamp = "2024-01-01T00:00:02Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2]);
        let mut t1 = tool_result_event("t1", "a1", "t");
        t1.data.insert(NEXT_TURN_KEY.into(), json!("a2"));
        view.events = vec![t1, attachment("hp", Some("a1"), "2024-01-01T00:00:01Z")];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "a1", "t1", "hp", "a2"]);
        assert_eq!(parent_of(&convo, "t1"), Some("a1"));
        assert_eq!(parent_of(&convo, "hp"), Some("a1"));
        assert_eq!(parent_of(&convo, "a2"), Some("t1"));
    }

    #[test]
    fn test_source_tool_result_line_is_written_in_place() {
        // Source: u1 <- a1 <- h1 (side leaf), a1 <- tr1 <- n3 <- a2
        let mut a1 = assistant_turn("a1", "");
        a1.parent_id = Some("u1".into());
        a1.tool_uses = vec![tool_use_with_result("t")];
        let mut a2 = assistant_turn("a2", "Done.");
        a2.parent_id = Some("a1".into());
        a2.timestamp = "2024-01-01T00:00:02Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a2]);
        view.events = vec![
            attachment("h1", Some("a1"), "2024-01-01T00:00:01Z"),
            tool_result_event("tr1", "a1", "t"),
            attachment("n3", Some("tr1"), "2024-01-01T00:00:01Z"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "a1", "h1", "tr1", "n3", "a2"]);
        assert_eq!(parent_of(&convo, "h1"), Some("a1"));
        assert_eq!(parent_of(&convo, "tr1"), Some("a1"));
        assert_eq!(parent_of(&convo, "n3"), Some("tr1"));
        assert_eq!(parent_of(&convo, "a2"), Some("n3"));
        let result = convo
            .entries
            .iter()
            .find(|e| e.uuid == "tr1")
            .and_then(|e| e.message.as_ref())
            .map(|m| m.tool_results())
            .expect("tool-result line is a user message");
        assert_eq!(result[0].tool_use_id, "t");
    }

    #[test]
    fn test_forked_turns_both_hang_off_the_run_tail() {
        let mut a1 = assistant_turn("a1", "First try");
        a1.parent_id = Some("u1".into());
        let mut a1b = assistant_turn("a1b", "Second try");
        a1b.parent_id = Some("u1".into());
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, a1b]);
        view.events = vec![
            attachment("n1", Some("u1"), "2024-01-01T00:00:00Z"),
            attachment("n2", Some("n1"), "2024-01-01T00:00:00Z"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "n1", "n2", "a1", "a1b"]);
        assert_eq!(parent_of(&convo, "a1"), Some("n2"));
        assert_eq!(parent_of(&convo, "a1b"), Some("n2"));
    }

    #[test]
    fn test_event_with_unknown_parent_hangs_off_the_last_turn_before_it() {
        let mut a1 = assistant_turn("a1", "Reply");
        a1.parent_id = Some("u1".into());
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1]);
        view.events = vec![attachment("n1", Some("gone"), "2024-01-01T00:00:00.500Z")];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "n1", "a1"]);
        assert_one_chain(&convo);
    }

    /// Cross-harness sources give events no parent and no mark.
    #[test]
    fn test_event_with_no_parent_and_no_mark_hangs_off_the_last_turn_before_it() {
        let mut a1 = assistant_turn("a1", "Reply");
        a1.parent_id = Some("u1".into());
        let mut u2 = user_turn("u2", "Next");
        u2.parent_id = Some("a1".into());
        u2.timestamp = "2024-01-01T00:00:02Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, u2]);
        view.events = vec![
            attachment("e0", None, "2023-12-31T00:00:00Z"),
            attachment("e1", None, "2024-01-01T00:00:01.500Z"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["e0", "u1", "a1", "e1", "u2"]);
        assert_one_chain(&convo);
    }

    #[test]
    fn test_source_root_event_opens_the_chain_after_the_round_trip() {
        // derive_path hands a parentless event the last step as parent.
        let mut a1 = assistant_turn("a1", "Reply");
        a1.parent_id = Some("u1".into());
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1]);
        let mut r1 = attachment("r1", Some("a1"), "2024-01-01T00:00:00Z");
        SourceRoot::Session.mark(&mut r1);
        view.events = vec![r1, attachment("r2", Some("r1"), "2024-01-01T00:00:00Z")];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["r1", "r2", "u1", "a1"]);
        assert_one_chain(&convo);
    }

    #[test]
    fn test_compaction_boundary_is_written_in_place_with_no_parent() {
        // Source: u1 <- a1, then cb1 with parentUuid null and
        // logicalParentUuid a1, then the summary u2 <- cb1.
        let mut a1 = assistant_turn("a1", "Reply");
        a1.parent_id = Some("u1".into());
        let mut u2 = user_turn("u2", "[summary]");
        u2.parent_id = Some("a1".into());
        u2.timestamp = "2024-01-01T00:00:02Z".into();
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1, u2]);
        let mut cb1 = attachment("cb1", Some("a1"), "2024-01-01T00:00:01Z");
        cb1.event_type = "compact_boundary".to_string();
        SourceRoot::Reset.mark(&mut cb1);
        view.events = vec![cb1];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "a1", "cb1", "u2"]);
        assert_eq!(parent_of(&convo, "cb1"), None);
        assert_eq!(parent_of(&convo, "u2"), Some("cb1"));
    }

    #[test]
    fn test_event_with_unknown_parent_and_no_timestamp_hangs_off_the_last_turn() {
        let mut a1 = assistant_turn("a1", "Reply");
        a1.parent_id = Some("u1".into());
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go"), a1]);
        view.events = vec![attachment("n1", Some("gone"), "")];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["u1", "a1", "n1"]);
        assert_one_chain(&convo);
    }

    #[test]
    fn test_event_parented_on_a_preamble_line_opens_the_chain() {
        let mut view = make_view("sess-1", vec![user_turn("u1", "Go")]);
        let mut preamble = attachment("claude-preamble-0", None, "");
        preamble.event_type = "permission-mode".to_string();
        preamble.data.insert(
            "raw".to_string(),
            json!({"type": "permission-mode", "permissionMode": "default", "sessionId": "sess-1"}),
        );
        view.events = vec![
            preamble,
            attachment("r1", Some("claude-preamble-0"), "2024-01-01T00:00:00Z"),
        ];

        let convo = ClaudeProjector.project(&view).unwrap();

        assert_eq!(ids(&convo), ["r1", "u1"]);
        assert_one_chain(&convo);
    }

    // ── Test 9: Stop reason and model preserved ───────────────────────

    #[test]
    fn test_stop_reason_and_model_preserved() {
        let mut turn = assistant_turn("a1", "Done.");
        turn.model = Some("claude-opus-4-6".to_string());
        turn.stop_reason = Some("end_turn".to_string());

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
        assert_eq!(msg.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(msg.stop_reason.as_deref(), Some("end_turn"));
    }

    // ── Additional edge case: is_sidechain always false ───────────────

    #[test]
    fn test_is_sidechain_always_false() {
        let view = make_view(
            "sess-1",
            vec![user_turn("u1", "Hi"), assistant_turn("a1", "Hello")],
        );
        let convo = ClaudeProjector.project(&view).unwrap();

        for entry in &convo.entries {
            assert!(!entry.is_sidechain);
        }
    }

    // ── Additional edge case: empty text assistant with tool use ──────

    #[test]
    fn test_assistant_no_text_only_tool_use_produces_parts() {
        let mut turn = assistant_turn("a1", "");
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            result: None,
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let msg = content_entries(&convo)[0].message.as_ref().unwrap();
        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                // Empty text not included, just the ToolUse
                assert_eq!(parts.len(), 1);
                assert!(matches!(parts[0], ContentPart::ToolUse { .. }));
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Additional: multiple tool uses, all with results ─────────────

    #[test]
    fn test_multiple_tool_uses_all_with_results() {
        let mut turn = assistant_turn("a1", "Reading two files.");
        turn.tool_uses = vec![
            ToolInvocation {
                id: "t1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file a".to_string(),
                    is_error: false,
                }),
                category: None,
            },
            ToolInvocation {
                id: "t2".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file b".to_string(),
                    is_error: true,
                }),
                category: None,
            },
        ];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        // assistant + one tool-result entry per tool_use
        assert_eq!(entries.len(), 3);

        let r1 = &entries[1];
        match r1.message.as_ref().unwrap().content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t1");
                        assert_eq!(content.text(), "file a");
                        assert!(!is_error);
                    }
                    _ => panic!("Expected ToolResult at index 0"),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }

        let r2 = &entries[2];
        match r2.message.as_ref().unwrap().content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        assert_eq!(tool_use_id, "t2");
                        assert_eq!(content.text(), "file b");
                        assert!(is_error);
                    }
                    _ => panic!("Expected ToolResult at index 0"),
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Additional: mixed results (some with, some without) ──────────

    #[test]
    fn test_partial_tool_results_only_emits_those_with_results() {
        let mut turn = assistant_turn("a1", "Using tools.");
        turn.tool_uses = vec![
            ToolInvocation {
                id: "t1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({}),
                result: Some(ToolResult {
                    content: "file content".to_string(),
                    is_error: false,
                }),
                category: None,
            },
            ToolInvocation {
                id: "t2".to_string(),
                name: "Write".to_string(),
                input: serde_json::json!({}),
                result: None, // no result for this one
                category: None,
            },
        ];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        // assistant + tool-result entry (only t1 has a result)
        assert_eq!(entries.len(), 2);
        let result_entry = &entries[1];
        let msg = result_entry.message.as_ref().unwrap();
        match msg.content.as_ref().unwrap() {
            MessageContent::Parts(parts) => {
                // Only one result (t1), not two
                assert_eq!(parts.len(), 1);
                if let ContentPart::ToolResult { tool_use_id, .. } = &parts[0] {
                    assert_eq!(tool_use_id, "t1");
                } else {
                    panic!("Expected ToolResult");
                }
            }
            other => panic!("Expected Parts, got {:?}", other),
        }
    }

    // ── Tool result entries inherit metadata from parent turn ─────────

    #[test]
    fn test_tool_result_entry_inherits_metadata() {
        let mut turn = assistant_turn("a1", "Reading.");
        turn.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/project".to_string()),
            vcs_branch: Some("dev".to_string()),
            vcs_revision: None,
        });
        turn.tool_uses = vec![ToolInvocation {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
            result: Some(ToolResult {
                content: "contents".to_string(),
                is_error: false,
            }),
            category: None,
        }];

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entries = content_entries(&convo);
        assert_eq!(entries.len(), 2);

        let result_entry = &entries[1];
        assert_eq!(result_entry.cwd.as_deref(), Some("/project"));
        assert_eq!(result_entry.git_branch.as_deref(), Some("dev"));
        // sourceToolAssistantUUID should be the parent turn's ID
        assert_eq!(
            result_entry.extra.get("sourceToolAssistantUUID"),
            Some(&json!("a1"))
        );
    }

    // ── Missing metadata fields don't appear (no nulls) ──────────────

    #[test]
    fn test_missing_metadata_no_nulls_in_json() {
        let turn = user_turn("u1", "Hello");
        // No environment, no extra — metadata fields should be absent

        let view = make_view("sess-1", vec![turn]);
        let convo = ClaudeProjector.project(&view).unwrap();

        let entry = &content_entries(&convo)[0];
        let json_str = serde_json::to_string(entry).unwrap();
        // None fields with skip_serializing_if should not appear
        assert!(!json_str.contains("\"version\""));
        assert!(!json_str.contains("\"userType\""));
        assert!(!json_str.contains("\"requestId\""));
        assert!(!json_str.contains("\"gitBranch\""));
    }
}
