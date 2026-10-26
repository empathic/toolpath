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
//! [`Turn`] has no per-harness extras, so `codex_extras` always returns
//! `None`: encrypted reasoning and per-`call_id` tool extras (exec exit
//! codes, custom-call statuses, patch manifests) are not carried through
//! the view and are not round-tripped. The code that would re-emit them
//! stays in place behind that `None`.
//!
//! For every source, the projector synthesizes defaults for fields the
//! view does not carry (originator: `"codex-toolpath"`, source: `"cli"`,
//! model_provider: `"openai"` unless the view names one).

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Result, Role, ToolInvocation, Turn,
};

use crate::types::{
    ContentPart, CustomToolCall, CustomToolCallOutput, FunctionCall, FunctionCallOutput, Message,
    Reasoning, RolloutLine, SandboxPolicy, SessionMeta, TurnContext,
};

// ── CodexProjector ───────────────────────────────────────────────────

/// Values this writer invents because codex needs them and the view does not
/// carry them, as `(key, value)` in the written rollout. The baseline checks
/// each is really written, and that a round trip gains nothing else
/// (`crates/toolpath-goldens/fixtures/contract/codex.json`).
/// `model_provider` is only invented when no turn of the view names a provider.
pub const SYNTHESIZED: &[(&str, &str)] = &[
    ("originator", "codex-toolpath"),
    ("cli_version", "0.0.0-projected"),
    ("model_provider", DEFAULT_MODEL_PROVIDER),
];

/// The `model_provider` Codex itself writes for OpenAI-served sessions.
const DEFAULT_MODEL_PROVIDER: &str = "openai";

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
            view.turns
                .iter()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
        })
        .unwrap_or_else(|| "/".to_string());

    let model = cfg
        .model
        .clone()
        .or_else(|| view.turns.iter().find_map(|t| t.model.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    let session_timestamp = view
        .started_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| view.turns.first().map(|t| t.timestamp.clone()))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string());

    let settings = TurnSettings::from_view(view);

    let mut lines: Vec<RolloutLine> = Vec::new();

    // Line 1: session_meta. Codex always writes this first.
    lines.push(make_session_meta_line(cfg, view, &session_timestamp, &cwd));

    // Find the last assistant turn so we can mark it `phase: "final"`.
    // Codex annotates every other assistant turn with `phase: "commentary"`,
    // matching what real rollouts look like.
    let last_assistant_idx = view
        .turns
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
        .turns
        .iter()
        .enumerate()
        .find(|(i, t)| {
            matches!(t.role, Role::Assistant)
                && assistant_writes_lines(
                    t,
                    &codex_extras(t).cloned().unwrap_or_default(),
                    Some(*i) == last_assistant_idx,
                )
        })
        .map(|(i, t)| group_of(i, t))
        .unwrap_or_else(|| view.id.clone());
    lines.push(make_turn_context_line(
        &first_group,
        &session_timestamp,
        &cwd,
        &model,
        &settings,
    ));
    let mut current_group = Some(first_group);

    // Running session-cumulative usage. Codex's `total_token_usage` is
    // cumulative; we advance it by each turn's per-step contribution and
    // emit it after the turn, so a re-read differences it back to the same
    // per-step spend.
    let mut running = toolpath_convo::TokenUsage::default();
    let mut passthrough = codex_passthrough(view).into_iter().peekable();
    for (idx, turn) in view.turns.iter().enumerate() {
        // Codex's own items, in time order, ahead of the first turn that follows them.
        while let Some(line) = passthrough.next_if(|l| l.timestamp <= turn.timestamp) {
            lines.push(line);
        }
        let codex_extra = codex_extras(turn).cloned().unwrap_or_default();
        // A turn that writes no line must not open a group: its turn_context would be
        // stamped on nothing, and the reader would move it (and its step id) to the next turn.
        if matches!(turn.role, Role::Assistant)
            && assistant_writes_lines(turn, &codex_extra, Some(idx) == last_assistant_idx)
        {
            let group = group_of(idx, turn);
            if current_group.as_deref() != Some(&group) {
                lines.push(make_turn_context_line(
                    &group,
                    &turn.timestamp,
                    &cwd,
                    &model,
                    &settings,
                ));
                current_group = Some(group);
            }
        }
        let codex = codex_extras(turn).cloned().unwrap_or_default();
        let is_final_assistant = Some(idx) == last_assistant_idx;
        emit_turn_lines(
            turn,
            &codex,
            is_final_assistant,
            &cwd,
            &mut lines,
            &mut running,
        );
    }

    lines.extend(passthrough);

    Ok(crate::types::Session {
        id: view.id.clone(),
        file_path: PathBuf::new(),
        lines,
    })
}

/// The response items Codex wrote that the IR keeps only as events (`tool_search_call` and
/// `tool_search_output`, whose output holds the tool definitions it found), as rollout lines.
/// Only for a view Codex recorded: no other harness has them. In event order.
fn codex_passthrough(view: &ConversationView) -> Vec<RolloutLine> {
    if view.provider_id.as_deref() != Some("codex") {
        return Vec::new();
    }
    view.events
        .iter()
        .filter_map(|e| {
            let payload = Value::Object(e.data.clone().into_iter().collect());
            if e.event_type == "compacted" {
                // The compaction Codex wrote, with its replacement history.
                return Some(RolloutLine {
                    timestamp: e.timestamp.clone(),
                    kind: "compacted".to_string(),
                    payload,
                    extra: HashMap::new(),
                });
            }
            if matches!(e.event_type.as_str(), "task_started" | "task_complete") {
                // The task lifecycle: times, duration, time to first token, mode, context window.
                return Some(event_msg_line(&e.timestamp, payload));
            }
            let kind = e.event_type.strip_prefix("response_item.")?;
            Some(response_item_line(&e.timestamp, kind, payload))
        })
        .collect()
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
        // Who served the session's model, when the view says (the first turn that names one);
        // else the default Codex itself writes.
        model_provider: Some(
            view.turns
                .iter()
                .find_map(|t| t.provider.clone().filter(|p| !p.is_empty()))
                .unwrap_or_else(|| DEFAULT_MODEL_PROVIDER.to_string()),
        ),
        // The session's own system prompt, when a Codex view carries it.
        base_instructions: (view.provider_id.as_deref() == Some("codex"))
            .then(|| view.events.iter().find(|e| e.event_type == "session_meta"))
            .flatten()
            .and_then(|e| e.data.get("base_instructions").cloned())
            .and_then(|v| serde_json::from_value(v).ok()),
        dynamic_tools: None,
        memory_mode: None,
        git: None,
        // What Codex states on session_meta beyond the typed fields (a Codex view only).
        extra: (view.provider_id.as_deref() == Some("codex"))
            .then(|| view.events.iter().find(|e| e.event_type == "session_meta"))
            .flatten()
            .and_then(|e| e.data.get("thread_source").cloned())
            .map(|v| HashMap::from([("thread_source".to_string(), v)]))
            .unwrap_or_default(),
    };
    RolloutLine {
        timestamp: timestamp.to_string(),
        kind: "session_meta".to_string(),
        payload: serde_json::to_value(&meta).unwrap_or(Value::Null),
        extra: HashMap::new(),
    }
}

/// The settings a Codex session states on its `turn_context` lines, taken
/// from the view's first `turn_context` event, so a Codex session projected
/// back to Codex keeps them. Only for a view Codex recorded
/// (`provider_id == "codex"`): no other harness has these settings, and
/// what they lose is declared in the writer contract
/// (`toolpath-goldens/fixtures/contract/codex.json`).
#[derive(Default)]
struct TurnSettings {
    approval_policy: Option<String>,
    sandbox_policy: Option<SandboxPolicy>,
    /// The permission settings Codex states besides `sandbox_policy`, kept as the JSON it wrote.
    permission_profile: Option<Value>,
    file_system_sandbox_policy: Option<Value>,
    personality: Option<String>,
    current_date: Option<String>,
    timezone: Option<String>,
    /// The workspace roots as Codex wrote them, and the cwd that turn_context named: when the
    /// roots were just that cwd, they follow the cwd the session is written for.
    workspace_roots: Option<Vec<String>>,
    source_cwd: Option<String>,
    collaboration_mode: Option<Value>,
}

impl TurnSettings {
    fn from_view(view: &ConversationView) -> Self {
        if view.provider_id.as_deref() != Some("codex") {
            return Self::default();
        }
        let Some(event) = view.events.iter().find(|e| e.event_type == "turn_context") else {
            return Self::default();
        };
        fn field<T: serde::de::DeserializeOwned>(
            data: &HashMap<String, Value>,
            key: &str,
        ) -> Option<T> {
            serde_json::from_value(data.get(key)?.clone()).ok()
        }
        Self {
            approval_policy: field(&event.data, "approval_policy"),
            sandbox_policy: field(&event.data, "sandbox_policy"),
            permission_profile: event.data.get("permission_profile").cloned(),
            file_system_sandbox_policy: event.data.get("file_system_sandbox_policy").cloned(),
            personality: field(&event.data, "personality"),
            current_date: field(&event.data, "current_date"),
            timezone: field(&event.data, "timezone"),
            workspace_roots: field(&event.data, "workspace_roots"),
            source_cwd: field(&event.data, "cwd"),
            collaboration_mode: field(&event.data, "collaboration_mode"),
        }
    }
}

fn make_turn_context_line(
    turn_id: &str,
    timestamp: &str,
    cwd: &str,
    model: &str,
    settings: &TurnSettings,
) -> RolloutLine {
    let mut extra = HashMap::new();
    if let Some(v) = &settings.permission_profile {
        extra.insert("permission_profile".to_string(), v.clone());
    }
    if let Some(v) = &settings.file_system_sandbox_policy {
        extra.insert("file_system_sandbox_policy".to_string(), v.clone());
    }
    if let Some(roots) = &settings.workspace_roots {
        let just_the_cwd = settings
            .source_cwd
            .as_ref()
            .is_some_and(|c| roots == std::slice::from_ref(c));
        let roots = if just_the_cwd {
            vec![cwd.to_string()]
        } else {
            roots.clone()
        };
        extra.insert("workspace_roots".to_string(), serde_json::json!(roots));
    }
    let tc = TurnContext {
        turn_id: turn_id.to_string(),
        cwd: PathBuf::from(cwd),
        current_date: settings.current_date.clone(),
        timezone: settings.timezone.clone(),
        approval_policy: settings.approval_policy.clone(),
        sandbox_policy: settings.sandbox_policy.clone(),
        model: Some(model.to_string()),
        personality: settings.personality.clone(),
        collaboration_mode: settings.collaboration_mode.clone(),
        extra,
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

fn emit_turn_lines(
    turn: &Turn,
    codex: &Map<String, Value>,
    is_final_assistant: bool,
    session_cwd: &str,
    lines: &mut Vec<RolloutLine>,
    running: &mut toolpath_convo::TokenUsage,
) {
    match &turn.role {
        Role::User => emit_user_message(turn, lines),
        Role::Assistant => {
            emit_assistant(turn, codex, is_final_assistant, session_cwd, lines, running)
        }
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

/// Whether [`emit_assistant`] writes any line for this turn: encrypted or plain reasoning, a
/// message (any final assistant, or one with text, tool calls or thinking). Keep in step with it.
fn assistant_writes_lines(
    turn: &Turn,
    codex: &Map<String, Value>,
    is_final_assistant: bool,
) -> bool {
    let encrypted = codex
        .get("reasoning_encrypted")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    let has_thinking = turn.thinking.as_ref().is_some_and(|s| !s.is_empty());
    encrypted
        || is_final_assistant
        || !turn.text.is_empty()
        || !turn.tool_uses.is_empty()
        || has_thinking
}

fn emit_assistant(
    turn: &Turn,
    codex: &Map<String, Value>,
    is_final_assistant: bool,
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
    // (its attributed per-step spend, or its group total when no per-step
    // split exists), then emit `token_count` AFTER the turn — the reader
    // differences the cumulative and attributes the delta to the step it
    // follows. Mirrors how real Codex streams cumulative counts per step.
    if let Some(contribution) = turn
        .attributed_token_usage
        .as_ref()
        .or(turn.token_usage.as_ref())
    {
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
            provider: None,
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-04-20T16:00:00.000Z".into(),
            text: text.into(),
            thinking: None,
            thinking_signature: None,
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
            provider: None,
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-04-20T16:00:01.000Z".into(),
            text: text.into(),
            thinking: None,
            thinking_signature: None,
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
            turns,
            total_usage: None,
            provider_id: Some("codex".into()),
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
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

    fn with_turn_context(mut view: ConversationView) -> ConversationView {
        let payload = json!({
            "turn_id": "t1",
            "cwd": "/tmp/proj",
            "approval_policy": "on-request",
            "sandbox_policy": {"type": "workspace-write", "network_access": false},
            "permission_profile": {
                "type": "managed",
                "network": "restricted",
                "file_system": {
                    "type": "restricted",
                    "entries": [{"access": "read", "path": {"type": "special", "value": {"kind": "root"}}}]
                }
            },
            "file_system_sandbox_policy": {
                "kind": "restricted",
                "entries": [{"access": "write", "path": {"type": "path", "path": "/tmp/proj"}}]
            },
            "personality": "pragmatic",
            "current_date": "2026-04-20",
            "timezone": "America/New_York",
            "workspace_roots": ["/tmp/proj", "/tmp/other"],
            "collaboration_mode": {"mode": "default", "settings": {"model": "gpt-5.4"}},
        });
        view.events.push(toolpath_convo::ConversationEvent {
            id: "turn_context-1".into(),
            timestamp: "2026-04-20T16:00:00.000Z".into(),
            parent_id: None,
            event_type: "turn_context".into(),
            data: payload
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        });
        view
    }

    fn turn_contexts(s: &crate::types::Session) -> Vec<&Value> {
        s.lines
            .iter()
            .filter(|l| l.kind == "turn_context")
            .map(|l| &l.payload)
            .collect()
    }

    #[test]
    fn codex_session_keeps_its_turn_settings() {
        let view = with_turn_context(view_with(vec![
            user_turn("u1", "hi"),
            assistant_turn("a1", "hello"),
        ]));
        let s = CodexProjector::default().project(&view).unwrap();
        let tcs = turn_contexts(&s);
        assert!(!tcs.is_empty());
        for tc in tcs {
            assert_eq!(tc["approval_policy"], "on-request");
            assert_eq!(tc["sandbox_policy"]["type"], "workspace-write");
            assert_eq!(tc["sandbox_policy"]["network_access"], false);
            assert_eq!(tc["personality"], "pragmatic");
            assert_eq!(tc["current_date"], "2026-04-20");
            assert_eq!(tc["timezone"], "America/New_York");
            assert_eq!(tc["workspace_roots"][1], "/tmp/other");
            assert_eq!(tc["collaboration_mode"]["mode"], "default");
            // the permission settings Codex states besides `sandbox_policy`
            assert_eq!(tc["permission_profile"]["type"], "managed");
            assert_eq!(tc["permission_profile"]["network"], "restricted");
            assert_eq!(
                tc["permission_profile"]["file_system"]["entries"][0]["path"]["value"]["kind"],
                "root"
            );
            assert_eq!(tc["file_system_sandbox_policy"]["kind"], "restricted");
            assert_eq!(
                tc["file_system_sandbox_policy"]["entries"][0]["path"]["path"],
                "/tmp/proj"
            );
        }
    }

    fn tool_search_events(mut view: ConversationView) -> ConversationView {
        let ev = |ts: &str, kind: &str, payload: Value| toolpath_convo::ConversationEvent {
            id: format!("{kind}-{ts}"),
            timestamp: ts.into(),
            parent_id: None,
            event_type: format!("response_item.{kind}"),
            data: payload
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        view.events.push(ev(
            "2026-04-20T16:00:01.000Z",
            "tool_search_call",
            json!({"type": "tool_search_call", "call_id": "c1", "status": "completed",
                   "execution": "client", "arguments": {"query": "spawn agent", "limit": 8}}),
        ));
        view.events.push(ev(
            "2026-04-20T16:00:01.500Z",
            "tool_search_output",
            json!({"type": "tool_search_output", "call_id": "c1", "status": "completed",
                   "execution": "client",
                   "tools": [{"type": "namespace", "name": "multi_agent_v1",
                              "tools": [{"name": "spawn_agent", "strict": false}]}]}),
        ));
        view
    }

    /// Codex's own tool-search items hold the tool definitions it found; a Codex view gives them
    /// back, the call before its output, ahead of the first turn that follows them.
    #[test]
    fn codex_tool_search_items_are_written_back() {
        let mut u = user_turn("u1", "hi");
        u.timestamp = "2026-04-20T16:00:00.000Z".into();
        let mut a = assistant_turn("a1", "hello");
        a.timestamp = "2026-04-20T16:00:02.000Z".into();
        let view = tool_search_events(view_with(vec![u, a]));
        let s = CodexProjector::default().project(&view).unwrap();
        let kinds = inner_types(&s);
        let call = kinds
            .iter()
            .position(|k| k == "tool_search_call")
            .expect("call");
        let out = kinds
            .iter()
            .position(|k| k == "tool_search_output")
            .expect("output");
        assert_eq!(out, call + 1, "the output follows its call: {kinds:?}");
        let line = &s.lines[out];
        assert_eq!(line.kind, "response_item");
        assert_eq!(line.timestamp, "2026-04-20T16:00:01.500Z");
        assert_eq!(line.payload["tools"][0]["tools"][0]["name"], "spawn_agent");
        assert_eq!(line.payload["call_id"], "c1");
    }

    fn compacted_and_meta_events(mut view: ConversationView) -> ConversationView {
        let mut ev = |ts: &str, kind: &str, payload: Value| {
            view.events.push(toolpath_convo::ConversationEvent {
                id: format!("{kind}-{ts}"),
                timestamp: ts.into(),
                parent_id: None,
                event_type: kind.into(),
                data: payload
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            });
        };
        ev(
            "2026-04-20T16:00:00.000Z",
            "session_meta",
            json!({"id": "s", "thread_source": "user",
                   "base_instructions": {"text": "You are Codex."}}),
        );
        ev(
            "2026-04-20T16:00:01.000Z",
            "compacted",
            json!({"message": "", "replacement_history": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "kept"}]}]}),
        );
        view
    }

    /// The compaction Codex wrote (`compacted`, with its replacement history) and the session's
    /// base instructions go back for a Codex view.
    #[test]
    fn codex_compaction_and_base_instructions_are_written_back() {
        let mut u = user_turn("u1", "hi");
        u.timestamp = "2026-04-20T16:00:00.000Z".into();
        let mut a = assistant_turn("a1", "hello");
        a.timestamp = "2026-04-20T16:00:02.000Z".into();
        let view = compacted_and_meta_events(view_with(vec![u, a]));
        let s = CodexProjector::default().project(&view).unwrap();
        let c = s
            .lines
            .iter()
            .find(|l| l.kind == "compacted")
            .expect("compacted line");
        assert_eq!(
            c.payload["replacement_history"][0]["content"][0]["text"],
            "kept"
        );
        let meta = &s.lines[0];
        assert_eq!(meta.kind, "session_meta");
        assert_eq!(meta.payload["base_instructions"]["text"], "You are Codex.");
        assert_eq!(meta.payload["thread_source"], "user");
    }

    #[test]
    fn other_harness_compaction_and_base_instructions_are_not_copied() {
        let mut view = compacted_and_meta_events(view_with(vec![assistant_turn("a1", "hi")]));
        view.provider_id = Some("claude".into());
        let s = CodexProjector::default().project(&view).unwrap();
        assert!(s.lines.iter().all(|l| l.kind != "compacted"));
        assert!(s.lines[0].payload.get("base_instructions").is_none());
    }

    /// The task lifecycle Codex wrote (`task_started` / `task_complete`: start and completion
    /// times, duration, time to first token, the mode and context window) goes back.
    #[test]
    fn codex_task_lifecycle_events_are_written_back() {
        let mut view = view_with(vec![assistant_turn("a1", "hello")]);
        let mut push = |ts: &str, kind: &str, payload: Value| {
            view.events.push(toolpath_convo::ConversationEvent {
                id: format!("{kind}-{ts}"),
                timestamp: ts.into(),
                parent_id: None,
                event_type: kind.into(),
                data: payload
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            });
        };
        push(
            "2026-04-20T16:00:00.000Z",
            "task_started",
            json!({"type": "task_started", "turn_id": "t1", "started_at": 1,
                   "model_context_window": 258400, "collaboration_mode_kind": "default"}),
        );
        push(
            "2026-04-20T16:00:09.000Z",
            "task_complete",
            json!({"type": "task_complete", "turn_id": "t1", "completed_at": 9,
                   "duration_ms": 8000, "time_to_first_token_ms": 120,
                   "last_agent_message": "done"}),
        );
        let s = CodexProjector::default().project(&view).unwrap();
        let started = s
            .lines
            .iter()
            .find(|l| l.kind == "event_msg" && l.payload["type"] == "task_started")
            .expect("task_started");
        assert_eq!(started.payload["model_context_window"], 258400);
        assert_eq!(started.payload["collaboration_mode_kind"], "default");
        let done = s
            .lines
            .iter()
            .find(|l| l.kind == "event_msg" && l.payload["type"] == "task_complete")
            .expect("task_complete");
        assert_eq!(done.payload["duration_ms"], 8000);
        assert_eq!(done.payload["time_to_first_token_ms"], 120);
        assert_eq!(done.payload["last_agent_message"], "done");
    }

    #[test]
    fn other_harness_tool_search_items_are_not_copied() {
        let mut view = tool_search_events(view_with(vec![assistant_turn("a1", "hello")]));
        view.provider_id = Some("claude".into());
        let s = CodexProjector::default().project(&view).unwrap();
        assert!(
            !inner_types(&s).iter().any(|k| k.starts_with("tool_search")),
            "copied from a claude view"
        );
    }

    /// When Codex's workspace roots were only its cwd, they follow the cwd the session is
    /// written for (a resume in another directory must not point at the old one).
    #[test]
    fn workspace_roots_that_were_just_the_cwd_follow_the_new_cwd() {
        let mut view = with_turn_context(view_with(vec![assistant_turn("a1", "hello")]));
        for e in &mut view.events {
            e.data
                .insert("workspace_roots".into(), json!(["/tmp/proj"]));
        }
        let s = CodexProjector::new()
            .with_cwd("/work/new")
            .project(&view)
            .unwrap();
        for tc in turn_contexts(&s) {
            assert_eq!(tc["workspace_roots"], json!(["/work/new"]));
        }
    }

    #[test]
    fn other_harness_settings_are_not_copied() {
        let mut view = with_turn_context(view_with(vec![assistant_turn("a1", "hello")]));
        view.provider_id = Some("claude".into());
        let s = CodexProjector::default().project(&view).unwrap();
        for tc in turn_contexts(&s) {
            for key in [
                "approval_policy",
                "sandbox_policy",
                "permission_profile",
                "file_system_sandbox_policy",
                "personality",
                "current_date",
                "timezone",
                "workspace_roots",
                "collaboration_mode",
            ] {
                assert!(tc.get(key).is_none(), "{key} copied from a claude view");
            }
        }
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
