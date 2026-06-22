//! [`CodexProjector`] — maps a [`ConversationView`] back to a Codex
//! [`crate::Session`].
//!
//! This is the inverse of [`crate::provider::to_view`]: where `to_view`
//! reads a Codex rollout JSONL into a provider-agnostic view,
//! `CodexProjector` serializes that view back into the on-disk shape
//! (a sequence of [`RolloutLine`]s starting with a `session_meta`
//! line, followed by a `turn_context`, then per-turn `response_item`
//! and `event_msg` lines).
//!
//! The projector consumes provider-specific data the forward path
//! stashed under `Turn.extra["codex"]`:
//!
//! - `reasoning_encrypted`: opaque ciphertext blobs that round-trip
//!   verbatim as `Reasoning::encrypted_content`
//! - `tool_extras`: per-`call_id` extras (exec exit_codes, custom-call
//!   statuses, patch-change manifests, etc.) preserved on the matching
//!   `function_call_output` / `custom_tool_call_output` line
//!
//! For non-Codex sources, the projector synthesizes sensible defaults
//! (originator: `"codex-toolpath"`, source: `"cli"`, model_provider:
//! `"openai"`).
//!
//! Foreign-namespace extras (`Turn.extra["claude"]`,
//! `Turn.extra["gemini"]`, …) are dropped — they have no meaning in
//! Codex's protocol and would pollute the JSONL.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};
use toolpath_convo::{
    Compaction, ConversationProjector, ConversationView, ConvoError, Item, Result, Role,
    ToolInvocation, Turn,
};

use crate::types::{
    CompactedItem, ContentPart, CustomToolCall, CustomToolCallOutput, FunctionCall,
    FunctionCallOutput, Message, Reasoning, RolloutLine, SessionMeta, TurnContext,
};

// ── CodexProjector ───────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Codex [`crate::types::Session`].
///
/// Config fields are optional. Defaults match what Codex itself would
/// write for a fresh CLI invocation.
///
/// # Example
///
/// ```rust
/// use toolpath_codex::project::CodexProjector;
/// use toolpath_convo::{ConversationProjector, ConversationView};
///
/// let view = ConversationView {
///     id: "session-uuid".into(),
///     provider_id: Some("codex".into()),
///     ..Default::default()
/// };
///
/// let session = CodexProjector::default().project(&view).unwrap();
/// assert_eq!(session.id, "session-uuid");
/// // Always at least the session_meta line.
/// assert!(!session.lines.is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CodexProjector {
    /// Session `cwd`. Falls back to the first turn's environment, then `/`.
    pub cwd: Option<String>,
    /// Default model for the `turn_context` line. Falls back to the
    /// first assistant turn's `Turn.model`, then `"unknown"`.
    pub model: Option<String>,
    /// `originator` field on the session_meta line. Defaults to
    /// `"codex-toolpath"` so Codex-side tooling can tell projected
    /// sessions from real ones at a glance.
    pub originator: Option<String>,
    /// CLI version string for `session_meta.cli_version`.
    pub cli_version: Option<String>,
}

impl CodexProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_originator(mut self, originator: impl Into<String>) -> Self {
        self.originator = Some(originator.into());
        self
    }
}

impl ConversationProjector for CodexProjector {
    type Output = crate::types::Session;

    fn project(&self, view: &ConversationView) -> Result<crate::types::Session> {
        project_view(self, view).map_err(ConvoError::Provider)
    }
}

// ── Projection logic ─────────────────────────────────────────────────

fn project_view(
    cfg: &CodexProjector,
    view: &ConversationView,
) -> std::result::Result<crate::types::Session, String> {
    let cwd = cfg
        .cwd
        .clone()
        .or_else(|| {
            view.turns()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
        })
        .unwrap_or_else(|| "/".to_string());

    let model = cfg
        .model
        .clone()
        .or_else(|| view.turns().find_map(|t| t.model.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    let session_timestamp = view
        .started_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| view.turns().next().map(|t| t.timestamp.clone()))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());

    let mut lines: Vec<RolloutLine> = Vec::new();

    // Line 1: session_meta. Codex always writes this first.
    lines.push(make_session_meta_line(cfg, view, &session_timestamp, &cwd));

    // Find the last assistant turn so we can mark it `phase: "final"`.
    // Codex annotates every other assistant turn with `phase: "commentary"`,
    // matching what real rollouts look like. Indexed over the turn stream
    // (events/compactions don't count), matching `turn_idx` below.
    let last_assistant_idx = view
        .turns()
        .collect::<Vec<_>>()
        .iter()
        .rposition(|t| matches!(t.role, Role::Assistant));

    // A turn's group ID is its `group_id`; an assistant turn without one
    // is its own group (a unique synthesized ID) so its own total survives.
    let group_of = |idx: usize, turn: &Turn| -> String {
        turn.group_id
            .clone()
            .unwrap_or_else(|| format!("{}-t{}", view.id, idx))
    };

    // Line 2: an opening turn_context (real Codex writes it right after
    // session_meta, before the first user turn). Its turn_id is the first
    // group's, so leading user turns and the first assistant share it; later
    // group boundaries emit their own. This is what makes the source's
    // grouping survive the round-trip — the reader keys `Turn.group_id`
    // off the turn_context `turn_id`.
    let first_group = view
        .turns()
        .enumerate()
        .find(|(_, t)| matches!(t.role, Role::Assistant))
        .map(|(i, t)| group_of(i, t))
        .unwrap_or_else(|| view.id.clone());
    lines.push(make_turn_context_line(
        &first_group,
        &session_timestamp,
        &cwd,
        &model,
    ));
    let mut current_group = Some(first_group);

    // Running session-cumulative usage. Codex's `total_token_usage` is
    // cumulative; we advance it by each turn's per-step contribution and
    // emit it after the turn, so a re-read differences it back to the same
    // per-step spend.
    let mut running = toolpath_convo::TokenUsage::default();

    // Last turn-index of each message group, so a group whose total is
    // repeated on every member turn (e.g. a Gemini split message) advances
    // `running` exactly once — on the group's final turn — rather than once
    // per member, which would double-count.
    let group_last_idx = group_last_indices(view);

    // Walk the full ordered item stream so compaction boundaries land at
    // their true position between the surrounding turns. Events have no
    // Codex analog on the return path and are dropped; turns and compactions
    // both project to rollout lines.
    let mut turn_idx = 0usize;
    for item in &view.items {
        match item {
            Item::Turn(turn) => {
                if matches!(turn.role, Role::Assistant) {
                    let group = group_of(turn_idx, turn);
                    if current_group.as_deref() != Some(&group) {
                        lines.push(make_turn_context_line(
                            &group,
                            &turn.timestamp,
                            &cwd,
                            &model,
                        ));
                        current_group = Some(group);
                    }
                }
                let codex = codex_extras(turn).cloned().unwrap_or_default();
                let is_final_assistant = Some(turn_idx) == last_assistant_idx;
                let contribution = running_contribution(turn, turn_idx, &group_last_idx);
                emit_turn_lines(
                    turn,
                    &codex,
                    is_final_assistant,
                    contribution,
                    &cwd,
                    &mut lines,
                    &mut running,
                );
                turn_idx += 1;
            }
            Item::Compaction(c) => emit_compaction(c, &mut lines),
            Item::Event(_) => {}
        }
    }

    Ok(crate::types::Session {
        id: view.id.clone(),
        file_path: PathBuf::new(),
        lines,
    })
}

/// Last turn-index of each message group, indexed over the turn stream
/// (matching `turn_idx` in `project_view`). A turn with no `group_id` is its
/// own group and is omitted.
fn group_last_indices(view: &ConversationView) -> HashMap<String, usize> {
    let mut last: HashMap<String, usize> = HashMap::new();
    for (idx, turn) in view.turns().enumerate() {
        if let Some(mid) = &turn.group_id {
            last.insert(mid.clone(), idx);
        }
    }
    last
}

/// The per-step usage this turn contributes to the session-cumulative
/// `running` counter (and thus the `token_count` line emitted after it):
///
/// - `attributed_token_usage` when the source reports a per-step split
///   (codex-native): each member of a group carries its own slice, summing
///   to the group total.
/// - otherwise the turn's `token_usage` — but for a grouped turn whose total
///   is repeated on every member (no per-step split), only on the group's
///   last turn, so the message total is counted once, not once per member.
fn running_contribution<'a>(
    turn: &'a Turn,
    turn_idx: usize,
    group_last_idx: &HashMap<String, usize>,
) -> Option<&'a toolpath_convo::TokenUsage> {
    if let Some(attr) = turn.attributed_token_usage.as_ref() {
        return Some(attr);
    }
    let usage = turn.token_usage.as_ref()?;
    match &turn.group_id {
        Some(mid) if group_last_idx.get(mid) != Some(&turn_idx) => None,
        _ => Some(usage),
    }
}

fn make_session_meta_line(
    cfg: &CodexProjector,
    view: &ConversationView,
    timestamp: &str,
    cwd: &str,
) -> RolloutLine {
    let meta = SessionMeta {
        id: view.id.clone(),
        timestamp: timestamp.to_string(),
        cwd: PathBuf::from(cwd),
        originator: cfg
            .originator
            .clone()
            .unwrap_or_else(|| "codex-toolpath".to_string()),
        cli_version: cfg
            .cli_version
            .clone()
            .unwrap_or_else(|| "0.0.0-projected".to_string()),
        source: "cli".to_string(),
        forked_from_id: None,
        agent_nickname: None,
        agent_role: None,
        agent_path: None,
        model_provider: Some("openai".to_string()),
        base_instructions: None,
        dynamic_tools: None,
        memory_mode: None,
        git: None,
        extra: HashMap::new(),
    };
    RolloutLine {
        timestamp: timestamp.to_string(),
        kind: "session_meta".to_string(),
        payload: serde_json::to_value(&meta).unwrap_or(Value::Null),
        extra: HashMap::new(),
    }
}

fn make_turn_context_line(turn_id: &str, timestamp: &str, cwd: &str, model: &str) -> RolloutLine {
    let tc = TurnContext {
        turn_id: turn_id.to_string(),
        cwd: PathBuf::from(cwd),
        current_date: None,
        timezone: None,
        approval_policy: None,
        sandbox_policy: None,
        model: Some(model.to_string()),
        personality: None,
        collaboration_mode: None,
        extra: HashMap::new(),
    };
    RolloutLine {
        timestamp: timestamp.to_string(),
        kind: "turn_context".to_string(),
        payload: serde_json::to_value(&tc).unwrap_or(Value::Null),
        extra: HashMap::new(),
    }
}

/// Used to return `Turn.extra["codex"]`; the IR no longer carries
/// provider-namespaced extras. Always `None`. Callers fall back to
/// reconstructing source-format details from typed IR fields and
/// reasonable defaults.
fn codex_extras(_turn: &Turn) -> Option<&'static Map<String, Value>> {
    None
}

/// Emit a `compacted` rollout line for a [`Compaction`] boundary — the
/// inverse of `Builder::handle_compacted`. Codex's payload is
/// `{message, replacement_history?, window_id?}`; only `summary` survives
/// the forward path, so we round-trip it as `message` (defaulting to the
/// empty string) and leave the other fields absent.
fn emit_compaction(c: &Compaction, lines: &mut Vec<RolloutLine>) {
    let payload = CompactedItem {
        message: c.summary.clone().unwrap_or_default(),
        replacement_history: None,
        window_id: None,
        extra: HashMap::new(),
    };
    lines.push(RolloutLine {
        timestamp: c.timestamp.clone(),
        kind: "compacted".to_string(),
        payload: serde_json::to_value(&payload).unwrap_or(Value::Null),
        extra: HashMap::new(),
    });
}

fn emit_turn_lines(
    turn: &Turn,
    codex: &Map<String, Value>,
    is_final_assistant: bool,
    contribution: Option<&toolpath_convo::TokenUsage>,
    session_cwd: &str,
    lines: &mut Vec<RolloutLine>,
    running: &mut toolpath_convo::TokenUsage,
) {
    match &turn.role {
        Role::User => emit_user_message(turn, lines),
        Role::Assistant => emit_assistant(
            turn,
            codex,
            is_final_assistant,
            contribution,
            session_cwd,
            lines,
            running,
        ),
        Role::System => emit_developer_message(turn, lines),
        Role::Other(_) => {
            // Unknown roles don't have a clean Codex analog; emit them
            // as `developer`-role messages so the text survives in the
            // log even if the role label is foreign.
            emit_developer_message(turn, lines);
        }
    }
}

fn emit_user_message(turn: &Turn, lines: &mut Vec<RolloutLine>) {
    let msg = Message {
        role: "user".to_string(),
        content: vec![ContentPart::InputText {
            text: turn.text.clone(),
            extra: HashMap::new(),
        }],
        id: None,
        end_turn: None,
        phase: None,
        extra: HashMap::new(),
    };
    lines.push(response_item_line(
        &turn.timestamp,
        "message",
        serde_json::to_value(&msg).unwrap_or(Value::Null),
    ));
    if !turn.text.is_empty() && !is_system_caveat(&turn.text) {
        lines.push(event_msg_line(
            &turn.timestamp,
            json!({
                "type": "user_message",
                "message": turn.text,
                "images": [],
                "local_images": [],
                "text_elements": [],
            }),
        ));
    }
}

fn is_system_caveat(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('<') && trimmed.contains('>')
}

fn emit_developer_message(turn: &Turn, lines: &mut Vec<RolloutLine>) {
    let msg = Message {
        role: "developer".to_string(),
        content: vec![ContentPart::InputText {
            text: turn.text.clone(),
            extra: HashMap::new(),
        }],
        id: None,
        end_turn: None,
        phase: None,
        extra: HashMap::new(),
    };
    lines.push(response_item_line(
        &turn.timestamp,
        "message",
        serde_json::to_value(&msg).unwrap_or(Value::Null),
    ));
}

fn emit_assistant(
    turn: &Turn,
    codex: &Map<String, Value>,
    is_final_assistant: bool,
    contribution: Option<&toolpath_convo::TokenUsage>,
    session_cwd: &str,
    lines: &mut Vec<RolloutLine>,
    running: &mut toolpath_convo::TokenUsage,
) {
    // Order matches what Codex itself emits per turn:
    //   reasoning? → message → (function_call → function_call_output)*
    //
    // For Codex→View→Codex round-trips, encrypted ciphertext lives on
    // `Turn.extra["codex"]["reasoning_encrypted"]` and round-trips
    // verbatim. For cross-harness sources, `Turn.thinking` is the
    // public reasoning summary; we put it in `summary` instead of
    // re-encrypting it.
    let encrypted_blobs = codex
        .get("reasoning_encrypted")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !encrypted_blobs.is_empty() {
        for blob in encrypted_blobs {
            let enc = blob.as_str().map(str::to_string);
            let r = Reasoning {
                id: None,
                summary: vec![],
                content: None,
                encrypted_content: enc,
                extra: HashMap::new(),
            };
            lines.push(response_item_line(
                &turn.timestamp,
                "reasoning",
                serde_json::to_value(&r).unwrap_or(Value::Null),
            ));
        }
    } else if let Some(thinking) = &turn.thinking
        && !thinking.is_empty()
    {
        // Foreign-source: render thinking as a single summary entry.
        // This is how Codex serializes plain reasoning summaries when
        // it has them.
        let r = Reasoning {
            id: None,
            summary: vec![json!({"type": "summary_text", "text": thinking})],
            content: None,
            encrypted_content: None,
            extra: HashMap::new(),
        };
        lines.push(response_item_line(
            &turn.timestamp,
            "reasoning",
            serde_json::to_value(&r).unwrap_or(Value::Null),
        ));
    }

    // The TUI gates scrollback rendering on `final_answer` exactly —
    // `final` would silently drop the closing message from view.
    let phase = Some(if is_final_assistant {
        "final_answer".to_string()
    } else {
        "commentary".to_string()
    });
    // The forward path uses `response_item.message` as the turn anchor:
    // tool calls without a preceding message attach to whatever assistant
    // turn was last seen, even across user messages. Emit a message line
    // for every non-empty assistant turn so consecutive cross-harness
    // assistants don't merge. `thinking: Some("")` is treated as absent —
    // codex's forward path drops empty thinking, so emitting on the
    // first pass and not the second would be non-idempotent.
    let has_thinking = turn.thinking.as_ref().is_some_and(|s| !s.is_empty());
    if is_final_assistant || !turn.text.is_empty() || !turn.tool_uses.is_empty() || has_thinking {
        let msg = Message {
            role: "assistant".to_string(),
            content: vec![ContentPart::OutputText {
                text: turn.text.clone(),
                extra: HashMap::new(),
            }],
            id: None,
            end_turn: None,
            phase: phase.clone(),
            extra: HashMap::new(),
        };
        lines.push(response_item_line(
            &turn.timestamp,
            "message",
            serde_json::to_value(&msg).unwrap_or(Value::Null),
        ));
        if !turn.text.is_empty() {
            lines.push(event_msg_line(
                &turn.timestamp,
                json!({
                    "type": "agent_message",
                    "message": turn.text,
                    "phase": phase,
                    "memory_citation": Value::Null,
                }),
            ));
        }
    }

    let tool_extras = codex
        .get("tool_extras")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for tu in &turn.tool_uses {
        let name = tool_native_name(tu);
        emit_tool_call(turn, tu, &name, &tool_extras, session_cwd, lines);
    }

    // Advance the session-cumulative counter by this step's contribution
    // (its attributed per-step spend, or its group total once when no
    // per-step split exists — see `running_contribution`), then emit
    // `token_count` AFTER the turn — the reader differences the cumulative
    // and attributes the delta to the step it follows. Mirrors how real
    // Codex streams cumulative counts per step.
    if let Some(contribution) = contribution {
        add_codex_usage(running, contribution);
        lines.push(event_msg_line(
            &turn.timestamp,
            json!({
                "type": "token_count",
                "info": {
                    "total_token_usage": convo_usage_to_codex_json(running),
                },
                "rate_limits": Value::Null,
            }),
        ));
    }
}

/// Component-wise `acc += delta` on the convo usage type (None as 0).
fn add_codex_usage(acc: &mut toolpath_convo::TokenUsage, delta: &toolpath_convo::TokenUsage) {
    let add = |a: &mut Option<u32>, b: Option<u32>| {
        if let Some(b) = b {
            *a = Some(a.unwrap_or(0) + b);
        }
    };
    add(&mut acc.input_tokens, delta.input_tokens);
    add(&mut acc.output_tokens, delta.output_tokens);
    add(&mut acc.cache_read_tokens, delta.cache_read_tokens);
    add(&mut acc.cache_write_tokens, delta.cache_write_tokens);
}

fn emit_tool_call(
    turn: &Turn,
    tu: &ToolInvocation,
    name: &str,
    tool_extras: &Map<String, Value>,
    session_cwd: &str,
    lines: &mut Vec<RolloutLine>,
) {
    let extras_for_call = tool_extras
        .get(&tu.id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if name == "apply_patch" {
        let input_str = match &tu.input {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let status = extras_for_call
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string);
        let call = CustomToolCall {
            name: name.to_string(),
            input: input_str,
            call_id: tu.id.clone(),
            status,
            id: None,
            extra: HashMap::new(),
        };
        lines.push(response_item_line(
            &turn.timestamp,
            "custom_tool_call",
            serde_json::to_value(&call).unwrap_or(Value::Null),
        ));
        if let Some(result) = &tu.result {
            let mut out_extra = HashMap::new();
            if result.is_error {
                out_extra.insert("is_error".to_string(), Value::Bool(true));
            }
            let out = CustomToolCallOutput {
                call_id: tu.id.clone(),
                output: result.content.clone(),
                extra: out_extra,
            };
            lines.push(response_item_line(
                &turn.timestamp,
                "custom_tool_call_output",
                serde_json::to_value(&out).unwrap_or(Value::Null),
            ));
            lines.push(event_msg_line(
                &turn.timestamp,
                json!({
                    "type": "patch_apply_end",
                    "call_id": tu.id,
                    "stdout": result.content,
                    "stderr": "",
                    "success": !result.is_error,
                    "changes": {},
                }),
            ));
        }
    } else {
        // Codex's `arguments` field is a JSON-encoded string, not a parsed value.
        let arguments = serde_json::to_string(&tu.input).unwrap_or_else(|_| "{}".into());
        let call = FunctionCall {
            name: name.to_string(),
            arguments,
            call_id: tu.id.clone(),
            id: None,
            namespace: None,
            extra: HashMap::new(),
        };
        lines.push(response_item_line(
            &turn.timestamp,
            "function_call",
            serde_json::to_value(&call).unwrap_or(Value::Null),
        ));
        if let Some(result) = &tu.result {
            // Codex's wire format has no first-class error flag for
            // non-shell tools (only `exec_command_end.exit_code`). Stash
            // it on the function_call_output's extras so the forward
            // path can recover it.
            let mut out_extra = HashMap::new();
            if result.is_error {
                out_extra.insert("is_error".to_string(), Value::Bool(true));
            }
            let out = FunctionCallOutput {
                call_id: tu.id.clone(),
                output: result.content.clone(),
                extra: out_extra,
            };
            lines.push(response_item_line(
                &turn.timestamp,
                "function_call_output",
                serde_json::to_value(&out).unwrap_or(Value::Null),
            ));
            // The TUI requires `status: "completed"` and a populated
            // `parsed_cmd` to render the entry; synthetic values are fine.
            if name == "exec_command" || name == "shell" {
                let cmd_str = tu
                    .input
                    .get("cmd")
                    .or_else(|| tu.input.get("command"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let command = if cmd_str.is_empty() {
                    Vec::<String>::new()
                } else {
                    vec!["bash".to_string(), "-lc".to_string(), cmd_str.clone()]
                };
                let exit_code = if result.is_error { 1 } else { 0 };
                lines.push(event_msg_line(
                    &turn.timestamp,
                    json!({
                        "type": "exec_command_end",
                        "call_id": tu.id,
                        "turn_id": turn.id,
                        "command": command,
                        "cwd": session_cwd,
                        "parsed_cmd": [{"type": "unknown", "cmd": cmd_str}],
                        "source": "unified_exec_startup",
                        "stdout": "",
                        "stderr": "",
                        "aggregated_output": result.content,
                        "exit_code": exit_code,
                        "duration": {"secs": 0, "nanos": 0},
                        "formatted_output": "",
                        "status": "completed",
                    }),
                ));
            }
        }
    }
}

/// Pick Codex's native tool name. Same logic as the other projectors:
/// keep the source name when it's already in Codex's vocabulary;
/// otherwise route through the source's `category` plus
/// `native_name(args)` to land on a Codex-canonical name; otherwise
/// pass the source name through verbatim (Codex's protocol accepts
/// any string here, so MCP tools and unknown future tools survive).
fn tool_native_name(tu: &ToolInvocation) -> String {
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

fn response_item_line(timestamp: &str, inner_type: &str, mut payload: Value) -> RolloutLine {
    // Ensure the inner discriminant is set; serde sometimes drops it
    // when the value is reconstructed from a struct without an
    // explicit `type` field.
    if let Value::Object(m) = &mut payload {
        m.entry("type".to_string())
            .or_insert_with(|| Value::String(inner_type.to_string()));
    }
    RolloutLine {
        timestamp: timestamp.to_string(),
        kind: "response_item".to_string(),
        payload,
        extra: HashMap::new(),
    }
}

fn event_msg_line(timestamp: &str, payload: Value) -> RolloutLine {
    RolloutLine {
        timestamp: timestamp.to_string(),
        kind: "event_msg".to_string(),
        payload,
        extra: HashMap::new(),
    }
}

fn convo_usage_to_codex_json(u: &toolpath_convo::TokenUsage) -> Value {
    let mut m = Map::new();
    if let Some(v) = u.input_tokens {
        m.insert("input_tokens".to_string(), Value::from(v));
    }
    if let Some(v) = u.cache_read_tokens {
        m.insert("cached_input_tokens".to_string(), Value::from(v));
    }
    if let Some(v) = u.output_tokens {
        m.insert("output_tokens".to_string(), Value::from(v));
    }
    Value::Object(m)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{TokenUsage, ToolCategory, ToolInvocation, ToolResult};

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-04-20T16:00:00.000Z".into(),
            text: text.into(),
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
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-04-20T16:00:01.000Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: Some("gpt-5.4".into()),
            stop_reason: Some("stop".into()),
            token_usage: Some(TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_tokens: None,
                cache_write_tokens: None,
                ..Default::default()
            }),
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "session-uuid".into(),
            started_at: None,
            last_activity: None,
            items: turns.into_iter().map(toolpath_convo::Item::Turn).collect(),
            total_usage: None,
            provider_id: Some("codex".into()),
            files_changed: vec![],
            session_ids: vec![],
            ..Default::default()
        }
    }

    fn line_kinds(s: &crate::types::Session) -> Vec<String> {
        s.lines.iter().map(|l| l.kind.clone()).collect()
    }

    fn inner_types(s: &crate::types::Session) -> Vec<String> {
        s.lines
            .iter()
            .map(|l| {
                l.payload
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn empty_view_yields_session_meta_plus_turn_context() {
        let s = CodexProjector::default()
            .project(&view_with(vec![]))
            .unwrap();
        assert_eq!(s.id, "session-uuid");
        assert_eq!(line_kinds(&s), vec!["session_meta", "turn_context"]);
    }

    #[test]
    fn user_turn_becomes_user_role_message() {
        let s = CodexProjector::default()
            .project(&view_with(vec![user_turn("u1", "hi")]))
            .unwrap();
        let kinds = line_kinds(&s);
        // response_item carries the model-facing message; event_msg is
        // the TUI scrollback counterpart that `codex resume` renders.
        assert_eq!(
            kinds,
            vec!["session_meta", "turn_context", "response_item", "event_msg"]
        );
        let payload = &s.lines[2].payload;
        assert_eq!(payload["type"], "message");
        assert_eq!(payload["role"], "user");
        assert_eq!(payload["content"][0]["type"], "input_text");
        assert_eq!(payload["content"][0]["text"], "hi");
        let event = &s.lines[3].payload;
        assert_eq!(event["type"], "user_message");
        assert_eq!(event["message"], "hi");
    }

    #[test]
    fn user_turn_with_system_caveat_skips_event_msg() {
        // System-injected `<…>` envelopes (caveats, environment_context)
        // never appear in the TUI scrollback. Skip the event_msg.
        let s = CodexProjector::default()
            .project(&view_with(vec![user_turn(
                "u1",
                "<local-command-caveat>do not respond</local-command-caveat>",
            )]))
            .unwrap();
        let kinds = line_kinds(&s);
        assert_eq!(kinds, vec!["session_meta", "turn_context", "response_item"]);
    }

    #[test]
    fn assistant_turn_with_function_call_and_output() {
        let mut t = assistant_turn("a1", "Let me check.");
        t.tool_uses = vec![ToolInvocation {
            id: "call_001".into(),
            name: "exec_command".into(),
            input: json!({"cmd": "pwd"}),
            result: Some(ToolResult {
                content: "/tmp\n".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::Shell),
        }];
        let s = CodexProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let inner = inner_types(&s);
        // session_meta + turn_context + message + agent_message
        // + function_call + function_call_output + exec_command_end
        // + token_count. The token_count follows the turn (the reader
        // differences the cumulative and attributes the delta to the step
        // it follows); the per-step spend is the turn's contribution.
        assert_eq!(
            inner,
            vec![
                "",
                "",
                "message",
                "agent_message",
                "function_call",
                "function_call_output",
                "exec_command_end",
                "token_count"
            ]
        );

        // FunctionCall.arguments is a JSON STRING, not a parsed value.
        let fc_payload = &s.lines[4].payload;
        assert_eq!(fc_payload["type"], "function_call");
        assert_eq!(fc_payload["call_id"], "call_001");
        assert_eq!(fc_payload["name"], "exec_command");
        let args = fc_payload["arguments"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(args).unwrap();
        assert_eq!(parsed["cmd"], "pwd");

        let fco_payload = &s.lines[5].payload;
        assert_eq!(fco_payload["type"], "function_call_output");
        assert_eq!(fco_payload["call_id"], "call_001");
        assert_eq!(fco_payload["output"], "/tmp\n");

        // exec_command_end: TUI counterpart with the aggregated output.
        let exec = &s.lines[6].payload;
        assert_eq!(exec["type"], "exec_command_end");
        assert_eq!(exec["call_id"], "call_001");
        assert_eq!(exec["aggregated_output"], "/tmp\n");
        assert_eq!(exec["exit_code"], 0);
    }

    #[test]
    fn foreign_tool_name_remaps_to_codex_via_category() {
        // Claude's `Bash` should land as Codex's `exec_command` because
        // category routes it through `native_name(Shell, _)`.
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "call_x".into(),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            result: None,
            category: Some(ToolCategory::Shell),
        }];
        let s = CodexProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let fc = &s
            .lines
            .iter()
            .find(|l| l.payload.get("type").and_then(Value::as_str) == Some("function_call"))
            .expect("function_call line")
            .payload;
        assert_eq!(fc["name"], "exec_command");
    }

    #[test]
    fn apply_patch_preserves_free_form_input_as_custom_tool_call() {
        // Codex apply_patch uses CustomToolCall + free-form input
        // (V4A patch text), not JSON args. Source name passes through
        // verbatim because Codex classifies `apply_patch` as FileWrite.
        let patch_body =
            "*** Begin Patch\n*** Add File: hello.rs\n+fn main(){}\n*** End Patch".to_string();
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "call_p".into(),
            name: "apply_patch".into(),
            input: Value::String(patch_body.clone()),
            result: Some(ToolResult {
                content: "ok".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileWrite),
        }];
        let s = CodexProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let inner = inner_types(&s);
        assert!(inner.contains(&"custom_tool_call".to_string()));
        assert!(inner.contains(&"custom_tool_call_output".to_string()));
        let ctc = s
            .lines
            .iter()
            .find(|l| l.payload.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
            .unwrap();
        assert_eq!(ctc.payload["input"], patch_body);
    }

    #[test]
    fn assistant_thinking_emits_reasoning_summary() {
        let mut t = assistant_turn("a1", "Done.");
        t.thinking = Some("hmm let me consider".into());
        let s = CodexProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let reasoning_line = s
            .lines
            .iter()
            .find(|l| l.payload.get("type").and_then(Value::as_str) == Some("reasoning"));
        assert!(reasoning_line.is_some(), "expected a reasoning line");
        let summary = &reasoning_line.unwrap().payload["summary"];
        assert!(summary.is_array());
        assert_eq!(summary[0]["type"], "summary_text");
        assert_eq!(summary[0]["text"], "hmm let me consider");
    }

    #[test]
    fn session_meta_carries_default_originator() {
        let s = CodexProjector::default()
            .project(&view_with(vec![]))
            .unwrap();
        let meta = &s.lines[0].payload;
        assert_eq!(meta["originator"], "codex-toolpath");
        assert_eq!(meta["source"], "cli");
    }

    #[test]
    fn last_assistant_gets_phase_final_others_commentary() {
        // The closing assistant turn in the view — regardless of
        // stop_reason — gets `phase: "final"`. Every other assistant
        // turn gets `phase: "commentary"`. `end_turn` is never set
        // (matches real Codex rollouts: `phase` alone discriminates).
        let mut t1 = assistant_turn("a1", "first");
        t1.stop_reason = Some("tool_use".into());
        let mut t2 = assistant_turn("a2", "second");
        t2.stop_reason = Some("tool_use".into());
        let mut t3 = assistant_turn("a3", "All done.");
        t3.stop_reason = Some("end_turn".into());

        let s = CodexProjector::default()
            .project(&view_with(vec![t1, t2, t3]))
            .unwrap();
        let messages: Vec<&RolloutLine> = s
            .lines
            .iter()
            .filter(|l| {
                l.payload.get("type").and_then(Value::as_str) == Some("message")
                    && l.payload.get("role").and_then(Value::as_str) == Some("assistant")
            })
            .collect();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].payload["phase"], "commentary");
        assert_eq!(messages[1].payload["phase"], "commentary");
        assert_eq!(messages[2].payload["phase"], "final_answer");
        // No message should carry `end_turn` — real rollouts omit it.
        for m in &messages {
            assert!(
                m.payload.get("end_turn").is_none(),
                "end_turn should be absent: {}",
                m.payload
            );
        }
    }

    #[test]
    fn grouped_turns_repeating_one_total_advance_running_once() {
        // A message split into two turns that each repeat the SAME total
        // (no per-step attribution — e.g. a Gemini split message) must
        // advance the cumulative `token_count` ONCE (the group's total),
        // not once per member. Otherwise a re-read differences a doubled
        // cumulative and over-attributes.
        let mut a1 = assistant_turn("a1", "");
        a1.group_id = Some("g".into());
        a1.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            ..Default::default()
        });
        let mut a2 = assistant_turn("a2", "answer");
        a2.group_id = Some("g".into());
        a2.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            ..Default::default()
        });

        let s = CodexProjector::default()
            .project(&view_with(vec![a1, a2]))
            .unwrap();

        // Every token_count is cumulative; the last one is the session total.
        let totals: Vec<&Value> = s
            .lines
            .iter()
            .filter(|l| l.payload.get("type").and_then(Value::as_str) == Some("token_count"))
            .map(|l| &l.payload["info"]["total_token_usage"])
            .collect();
        assert!(!totals.is_empty(), "expected at least one token_count");
        let last = totals.last().unwrap();
        // Counted once: 20, not 40.
        assert_eq!(last["output_tokens"], 20);
        assert_eq!(last["input_tokens"], 100);
    }

    #[test]
    fn jsonl_serializes_one_line_per_entry() {
        let s = CodexProjector::default()
            .project(&view_with(vec![user_turn("u1", "hi")]))
            .unwrap();
        for line in &s.lines {
            let serialized = serde_json::to_string(line).unwrap();
            assert!(
                !serialized.contains('\n'),
                "line serialized with newline: {}",
                serialized
            );
        }
    }
}
