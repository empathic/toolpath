//! [`ClaudeProjector`] — maps a [`ConversationView`] back to a Claude
//! [`Conversation`].
//!
//! This is the inverse of [`crate::provider::to_view`]: where `to_view`
//! reads a Claude JSONL conversation into a provider-agnostic view,
//! `ClaudeProjector` serializes that view back into the Claude wire format.

use crate::types::{
    ContentPart, Conversation, ConversationEntry, Message, MessageContent, MessageRole,
    ToolResultContent, Usage,
};
use serde_json::json;
use std::collections::HashMap;
use toolpath_convo::{
    Compaction, CompactionTrigger, ConversationProjector, ConversationView, ConvoError, Result,
    Role, ToolInvocation, Turn,
};

// ── ClaudeProjector ───────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Claude [`Conversation`].
///
/// Maps the provider-agnostic view back into Claude's JSONL wire format.
/// Assistant turns with tool uses will produce a separate tool-result user
/// entry after each assistant entry (one entry per assistant turn that has
/// tool uses with results).
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

/// Marker used by Claude's derive to preserve tool-result user entries as
/// events. Their UUID is what the next assistant turn's `parentUuid`
/// points at — synthesizing a new one breaks the chain.
const TOOL_RESULT_USER_EVENT: &str = "tool_result_user";

fn project_view(view: &ConversationView) -> std::result::Result<Conversation, String> {
    let mut convo = Conversation::new(view.id.clone());

    // Headerless lines (the JSONL "preamble": ai-title, last-prompt,
    // queue-operation, permission-mode, file-history-snapshot, and anything
    // unrecognized) ride in `view.events` carrying the original line verbatim
    // under `data["raw"]`. Dump them straight back. A headerless event is
    // identified by that `raw` key — no enumerated type list.
    let mut emitted_preamble = false;
    for event in view.events() {
        if let Some(raw) = event.data.get("raw") {
            convo.preamble.push(raw.clone());
            emitted_preamble = true;
        }
    }
    // Cross-harness views won't carry a Claude preamble; emit a default
    // permission-mode line so Claude Code can resume them.
    if !emitted_preamble {
        convo.preamble.push(json!({
            "type": "permission-mode",
            "permissionMode": "default",
            "sessionId": view.id,
        }));
    }

    // Index tool_result_user events by parent_id so we can re-emit them
    // inline right after the assistant turn they responded to. This keeps
    // every downstream `parentUuid` reference valid.
    let mut tool_result_events_by_parent: HashMap<String, Vec<&toolpath_convo::ConversationEvent>> =
        HashMap::new();
    for event in view.events() {
        if event.event_type != TOOL_RESULT_USER_EVENT {
            continue;
        }
        if let Some(pid) = &event.parent_id {
            tool_result_events_by_parent
                .entry(pid.clone())
                .or_default()
                .push(event);
        }
    }
    let mut consumed_event_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // For cross-harness sources whose IR doesn't model intermediate tool-
    // result turns, the next assistant's `parent_id` points at the prior
    // assistant. Claude expects it to point at the tool_result entry that
    // ran in between. Track those rewrites so we can patch the chain.
    let mut parent_rewrites: HashMap<String, String> = HashMap::new();

    // Message-group accounting. The IR carries a message's total
    // `token_usage` only on the group's final turn; real Claude JSONL stamps
    // usage on every line of a split (streaming snapshots that climb to the
    // total — see `provider::canonicalize_message_usage`). We re-expand the
    // group total onto every line, matching the common after-generation
    // pattern where the lines repeat one value. (We don't reconstruct the
    // intermediate streaming snapshots: they carry no per-step meaning, the
    // IR doesn't retain them, and the final total is what consumers sum.)
    let mut group_total: HashMap<&str, toolpath_convo::TokenUsage> = HashMap::new();
    for turn in view.turns() {
        if let (Some(mid), Some(usage)) = (turn.group_id.as_deref(), &turn.token_usage) {
            group_total
                .entry(mid)
                .and_modify(|acc| *acc = crate::provider::max_usage(acc, usage))
                .or_insert_with(|| usage.clone());
        }
    }

    // Iterate the full item stream (not just turns) so a compaction boundary
    // lands at its true position between the turns it separates — and so do
    // events: real Claude interleaves attachments and system entries
    // (turn_duration, etc.) with the turns, so emitting them from a trailing
    // pass regrouped them at the end of the file and reordered the entry
    // stream on every round-trip. Tool-result events stay with the by-parent
    // mechanism (their position must follow the assistant entry that isn't
    // 1:1 with an item on cross-harness views); preamble lines (`raw`) were
    // already pushed above.
    for item in &view.items {
        let turn = match item {
            toolpath_convo::Item::Turn(t) => t,
            toolpath_convo::Item::Compaction(c) => {
                // Emit the boundary (+ summary) that `to_view` re-folds into
                // this `Item::Compaction`. The preserved set rides in
                // compactMetadata.preservedMessages, which is how re-read
                // recovers it.
                let effective_parent = c
                    .parent_id
                    .as_ref()
                    .and_then(|pid| parent_rewrites.get(pid).cloned())
                    .or_else(|| c.parent_id.clone());
                let entries = compaction_entries(c, &view.id, effective_parent, &view.items);
                let summary_uuid = entries.get(1).map(|e| e.uuid.clone());
                for entry in entries {
                    convo.add_entry(entry);
                }
                // Claude's wire convention chains the first post-boundary
                // entry through the synthetic summary (boundary ← summary ←
                // first-post-entry), so IR parents pointing at the boundary
                // must be redirected to the summary we just emitted.
                if let Some(summary_uuid) = summary_uuid {
                    parent_rewrites.insert(c.id.clone(), summary_uuid);
                }
                continue;
            }
            toolpath_convo::Item::Event(event) => {
                if event.data.contains_key("raw")
                    || event.event_type == TOOL_RESULT_USER_EVENT
                    || consumed_event_ids.contains(&event.id)
                {
                    continue;
                }
                consumed_event_ids.insert(event.id.clone());
                let entry = project_event(event, &view.id);
                convo.add_entry(entry);
                continue;
            }
        };

        // Pre-rewrite this turn's parent_id if a synthesized tool_result
        // was emitted between it and its IR-recorded parent.
        let effective_parent = turn
            .parent_id
            .as_ref()
            .and_then(|pid| parent_rewrites.get(pid).cloned())
            .or_else(|| turn.parent_id.clone());

        match &turn.role {
            Role::User => {
                let mut entry = user_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                convo.add_entry(entry);
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
                convo.add_entry(assistant_entry);

                // Prefer the original tool-result user entries (preserved
                // as events with their source UUIDs) over synthesizing.
                // Synthesizing rewrites the UUID, which breaks the
                // parentUuid chain on every subsequent assistant turn.
                let real = tool_result_events_by_parent.remove(&turn.id);
                if let Some(events) = real {
                    let mut last_uuid = turn.id.clone();
                    for event in events {
                        let entry = tool_result_event_to_entry(event, &view.id);
                        last_uuid = entry.uuid.clone();
                        convo.add_entry(entry);
                        consumed_event_ids.insert(event.id.clone());
                    }
                    // Anything in the IR that pointed at this assistant
                    // turn should now point at the last tool-result entry
                    // we emitted, matching Claude's wire convention.
                    if last_uuid != turn.id {
                        parent_rewrites.insert(turn.id.clone(), last_uuid);
                    }
                } else {
                    // Cross-harness fallback: synthesize per-tool-use
                    // result entries.
                    let mut last_uuid = turn.id.clone();
                    for mut result_entry in tool_result_entries(turn, &view.id) {
                        apply_turn_metadata(&mut result_entry, turn);
                        last_uuid = result_entry.uuid.clone();
                        convo.add_entry(result_entry);
                    }
                    if last_uuid != turn.id {
                        parent_rewrites.insert(turn.id.clone(), last_uuid);
                    }
                }
            }
            Role::System => {
                let mut entry = system_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                convo.add_entry(entry);
            }
            Role::Other(_) => {
                let mut entry = other_turn_to_entry(turn, &view.id);
                apply_turn_metadata(&mut entry, turn);
                entry.parent_uuid = effective_parent;
                convo.add_entry(entry);
            }
        }
    }

    // Emit non-preamble events (attachments, etc.) as entries.
    for event in view.events() {
        if event.data.contains_key("raw") {
            continue; // headerless line — already pushed onto convo.preamble
        }
        if consumed_event_ids.contains(&event.id) {
            continue;
        }
        // Tool-result events without a matching parent turn — emit them
        // anyway (rare; happens when the assistant turn is dropped).
        if event.event_type == TOOL_RESULT_USER_EVENT {
            let entry = tool_result_event_to_entry(event, &view.id);
            convo.add_entry(entry);
            continue;
        }
        let entry = project_event(event, &view.id);
        convo.add_entry(entry);
    }

    Ok(convo)
}

/// Inverse of [`crate::provider::compaction_from_boundary`]: turn an
/// [`Item::Compaction`](toolpath_convo::Item) back into the on-disk entries
/// Claude writes for a compaction, so re-reading re-detects the same
/// compaction with the same preserved set.
///
/// Emits the boundary (`type: "system"`, `subtype: "compact_boundary"`)
/// carrying `logicalParentUuid` and `compactMetadata.{trigger, preTokens,
/// preservedMessages}` in `extra` — where [`crate::provider::is_compact_boundary`]
/// and `compaction_from_boundary` read them. `preservedMessages.uuids` is
/// [`toolpath_convo::expand_kept`] — the contiguous kept tail named by
/// `kept_from` (the compaction contract carries nothing richer). Then the
/// synthetic summary (`type: "user"`, `isCompactSummary: true`,
/// `parentUuid` = boundary), only when `summary` is `Some`.
///
/// Preserved turns are NOT re-logged as a replay block. Claude's resume
/// rebuilds context from the summary plus post-boundary turns only — anything
/// before the boundary's `parentUuid: null` is unreachable — so a replay is
/// dead weight (a resume test confirmed it changes nothing), and the preserved
/// set already round-trips through `preservedMessages`.
///
/// `effective_parent` is the boundary's logical parent after any tool-result
/// parent rewrites — it lands in the entry's top-level `logicalParentUuid`.
fn compaction_entries(
    c: &Compaction,
    session_id: &str,
    effective_parent: Option<String>,
    items: &[toolpath_convo::Item],
) -> Vec<ConversationEntry> {
    let mut entries: Vec<ConversationEntry> = Vec::new();

    let mut compact_metadata = serde_json::Map::new();
    if let Some(trigger) = c.trigger {
        let s = match trigger {
            CompactionTrigger::Auto => "auto",
            CompactionTrigger::Manual => "manual",
        };
        compact_metadata.insert("trigger".into(), json!(s));
    }
    if let Some(pre_tokens) = c.pre_tokens {
        compact_metadata.insert("preTokens".into(), json!(pre_tokens));
    }
    let preserved: Vec<String> = toolpath_convo::expand_kept(items, c);
    if !preserved.is_empty() {
        compact_metadata.insert("preservedMessages".into(), json!({ "uuids": preserved }));
    }

    let mut boundary_extra: HashMap<String, serde_json::Value> = HashMap::new();
    boundary_extra.insert("subtype".into(), json!("compact_boundary"));
    if let Some(parent) = &effective_parent {
        boundary_extra.insert("logicalParentUuid".into(), json!(parent));
    }
    boundary_extra.insert(
        "compactMetadata".into(),
        serde_json::Value::Object(compact_metadata),
    );

    let boundary = ConversationEntry {
        uuid: c.id.clone(),
        // The boundary's own parentUuid is always null on the wire; the
        // logical parent rides in compactMetadata's logicalParentUuid.
        parent_uuid: None,
        is_sidechain: false,
        entry_type: "system".to_string(),
        timestamp: c.timestamp.clone(),
        session_id: Some(session_id.to_string()),
        cwd: None,
        git_branch: None,
        message: None,
        version: None,
        user_type: None,
        request_id: None,
        tool_use_result: None,
        snapshot: None,
        message_id: None,
        extra: boundary_extra,
    };

    entries.push(boundary);

    if let Some(summary) = &c.summary {
        let mut summary_extra: HashMap<String, serde_json::Value> = HashMap::new();
        summary_extra.insert("isCompactSummary".into(), json!(true));
        // Real boundaries also mark the summary transcript-only; without it
        // the TUI renders the entire summary text inline on resume instead
        // of collapsing it behind the "Compacted" indicator (verified
        // against claude 2.1.216 by resuming a projected session).
        summary_extra.insert("isVisibleInTranscriptOnly".into(), json!(true));

        entries.push(ConversationEntry {
            uuid: format!("{}-summary", c.id),
            parent_uuid: Some(c.id.clone()),
            is_sidechain: false,
            entry_type: "user".to_string(),
            timestamp: c.timestamp.clone(),
            session_id: Some(session_id.to_string()),
            cwd: None,
            git_branch: None,
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text(summary.clone())),
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
            extra: summary_extra,
        });
    }

    entries
}

/// Rebuild a Claude tool-result user entry verbatim from a preserved event.
///
/// The event was emitted by [`crate::derive::derive_path`] when reading the
/// source JSONL — it carries the original UUID, parent UUID, the
/// `toolUseResult` blob, and any `entry_extra` fields (promptId, slug, …).
/// Reconstructing those preserves the UUID chain that Claude's UI traverses.
fn tool_result_event_to_entry(
    event: &toolpath_convo::ConversationEvent,
    session_id: &str,
) -> ConversationEntry {
    let mut content_parts: Vec<ContentPart> = Vec::new();
    if let Some(arr) = event.data.get("tool_results").and_then(|v| v.as_array()) {
        for v in arr {
            let tool_use_id = v
                .get("tool_use_id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let content_text = v
                .get("content")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let is_error = v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
            content_parts.push(ContentPart::ToolResult {
                tool_use_id,
                content: ToolResultContent::Text(content_text),
                is_error,
            });
        }
    }

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
        message: Some(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts(content_parts)),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        }),
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
        message_id: None,
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

    // The IR carries no provider-specific extras, so source-format details
    // (`version`, `user_type`, `request_id`, per-entry catch-all) stay `None`
    // here and the harness fills in defaults at write time.
}

/// Build a `ConversationEntry` for a user turn.
fn user_turn_to_entry(turn: &Turn, session_id: &str) -> ConversationEntry {
    let content = MessageContent::Text(turn.text.clone());

    // Claude writes local-command caveat entries with `isMeta: true` — the
    // loader hides them from the transcript sent back to the API. The flag
    // is a deterministic function of the caveat envelope, so re-derive it
    // rather than carry it through the IR.
    let mut extra: std::collections::HashMap<String, serde_json::Value> = Default::default();
    if turn.text.trim_start().starts_with("<local-command-caveat>") {
        extra.insert("isMeta".to_string(), serde_json::json!(true));
    }

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
        extra,
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
        // A fully empty assistant turn (a source whose derivation dropped an
        // empty-thinking block, an aborted response, …) must NOT become
        // `[{"type":"text","text":""}]`: that content shape aborts Claude
        // 2.1.216's transcript renderer — a resumed session shows no ❯
        // prompts and no Compacted indicator. Re-emit it as the empty
        // thinking block real Claude writes for these entries (verified to
        // render, signature-less, in 2.1.216).
        if turn.text.is_empty() {
            return MessageContent::Parts(vec![ContentPart::Thinking {
                thinking: String::new(),
                signature: None,
            }]);
        }
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
fn tool_result_entries(turn: &Turn, session_id: &str) -> Vec<ConversationEntry> {
    turn.tool_uses
        .iter()
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
            items: turns.into_iter().map(toolpath_convo::Item::Turn).collect(),
            total_usage: None,
            provider_id: None,
            files_changed: vec![],
            session_ids: vec![],
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

    #[test]
    fn test_fully_empty_assistant_turn_never_projects_an_empty_text_block() {
        // `[{"type":"text","text":""}]` aborts Claude 2.1.216's transcript
        // renderer — a resumed session shows no ❯ prompts and no Compacted
        // indicator. An empty turn (thinking dropped at derive, aborted
        // response) must project as the empty thinking block real Claude
        // writes instead (verified to render signature-less in 2.1.216).
        let content = build_assistant_content(&assistant_turn("a1", ""));
        let MessageContent::Parts(parts) = content else {
            panic!("assistant content is always Parts");
        };
        assert_eq!(
            serde_json::to_value(&parts).unwrap(),
            serde_json::json!([{"type":"thinking","thinking":""}]),
        );

        // Non-empty text keeps the plain text shape.
        let content = build_assistant_content(&assistant_turn("a2", "hi"));
        let MessageContent::Parts(parts) = content else {
            panic!("assistant content is always Parts");
        };
        assert_eq!(
            serde_json::to_value(&parts).unwrap(),
            serde_json::json!([{"type":"text","text":"hi"}]),
        );
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
        let a: Vec<&Turn> = back.turns().filter(|t| t.role == Role::Assistant).collect();
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
