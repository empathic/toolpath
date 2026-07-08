//! [`PiProjector`] — maps a [`ConversationView`] back to a Pi
//! [`PiSession`].
//!
//! This is the inverse of [`crate::provider::session_to_view`]: where
//! `session_to_view` reads a Pi JSONL session into a provider-agnostic
//! view, `PiProjector` serializes that view back into Pi's on-disk
//! shape (a [`SessionHeader`] plus a list of [`Entry`]).
//!
//! Pi's metadata-only entries (`ModelChange` / `ThinkingLevelChange` /
//! `Label`) ride `ConversationView.events` and are re-materialized here
//! as real Pi entries with their original ids and parentIds (see the
//! private `insert_event_entries` helper).
//!
//! One fidelity edge: the metadata entries' own ids/parentIds survive,
//! but a *message* entry whose source `parentId` pointed at a metadata
//! entry (e.g. `a1.parentID = "mc-1"`) loses that one parent edge on a
//! Pi→View→Path→View→Pi round-trip. `derive_path` resolves a turn's
//! parent only against other turns, not against event steps, so a turn
//! parented to an eventified entry becomes a root and its re-emitted
//! entry drops the `parentId`. Pi's reader is id/order-tolerant so
//! nothing breaks; closing the gap would need `derive_path` to resolve
//! turn parents against event step ids too (a future toolpath-convo
//! change).
//!
//! Everything else Pi-specific that the forward path didn't lift into a
//! typed `Turn` field cannot be recovered on a Pi→View→Pi round-trip:
//! `api`/`provider`, the structured `stopReason`, bash-execution
//! metadata (command/exit code/etc. beyond what's folded into
//! `Turn.text` and the synthetic `bash` tool call),
//! `SessionHeader.parent_session`, and the synthetic-turn markers
//! (`compaction`, `branchSummary`, `custom`, `customMessage`) — those
//! turns are indistinguishable from ordinary ones once round-tripped,
//! so the projector always synthesizes sensible defaults (api:
//! "anthropic", stop_reason: "stop", etc.) instead.

use std::collections::HashMap;

use serde_json::json;
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Result, Role, ToolInvocation, Turn,
};

use crate::reader::PiSession;
use crate::types::{
    AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, KnownStopReason, MessageContent,
    SessionHeader, StopReason, ToolResultContent, Usage,
};

// ── PiProjector ───────────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Pi [`PiSession`].
///
/// Config fields are optional. `cwd` overrides the source view's
/// working directory (which is otherwise pulled from
/// `Turn.environment.working_dir`). Default API metadata fills in
/// `api`/`provider` for assistant turns coming from a non-Pi source.
///
/// # Example
///
/// ```rust
/// use toolpath_convo::{ConversationProjector, ConversationView};
/// use toolpath_pi::project::PiProjector;
///
/// let view = ConversationView {
///     id: "session-uuid".into(),
///     provider_id: Some("pi".into()),
///     ..Default::default()
/// };
///
/// let session = PiProjector::default().project(&view).unwrap();
/// assert_eq!(session.header.id, "session-uuid");
/// ```
#[derive(Debug, Clone, Default)]
pub struct PiProjector {
    /// Override the session header's `cwd`. When `None`, the projector
    /// pulls it from the first turn's environment (or falls back to
    /// `"/"` if absent).
    pub cwd: Option<String>,
    /// Default `api` field for assistant turns — the IR has no field
    /// to recover the source's original value from. Defaults to
    /// `"anthropic"`.
    pub default_api: Option<String>,
    /// Default `provider` field for assistant turns — the IR has no
    /// field to recover the source's original value from. Defaults to
    /// `"anthropic"`.
    pub default_provider: Option<String>,
}

impl PiProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_default_api(mut self, api: impl Into<String>) -> Self {
        self.default_api = Some(api.into());
        self
    }

    pub fn with_default_provider(mut self, provider: impl Into<String>) -> Self {
        self.default_provider = Some(provider.into());
        self
    }
}

impl ConversationProjector for PiProjector {
    type Output = PiSession;

    fn project(&self, view: &ConversationView) -> Result<PiSession> {
        project_view(self, view).map_err(ConvoError::Provider)
    }
}

// ── Projection logic ─────────────────────────────────────────────────

fn project_view(
    cfg: &PiProjector,
    view: &ConversationView,
) -> std::result::Result<PiSession, String> {
    let cwd = cfg
        .cwd
        .clone()
        .or_else(|| {
            view.turns
                .iter()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
        })
        .unwrap_or_else(|| "/".to_string());

    let timestamp = view
        .started_at
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .or_else(|| view.turns.first().map(|t| t.timestamp.clone()))
        .unwrap_or_default();

    // Pi's session header optionally carries `parentSession`, but the
    // IR has no field to preserve it in, so a Pi→View→Pi round-trip
    // can't recover it.
    let parent_session = None;

    let header = SessionHeader {
        version: 3,
        id: view.id.clone(),
        timestamp,
        cwd,
        parent_session,
        extra: HashMap::new(),
    };

    let mut entries: Vec<Entry> = Vec::new();
    entries.push(Entry::Session(header.clone()));

    // A dedicated `Role::Other("tool")` turn elsewhere in the view (as
    // some non-Pi providers emit) can represent the same call as an
    // assistant's `tool_uses[i].result`; without a preserved call id to
    // correlate them, we can no longer tell whether one covers the
    // other, so every tool use with a result gets its own synthesized
    // `toolResult` entry.
    for turn in &view.turns {
        emit_turn_entries(cfg, turn, &mut entries);
    }

    // Re-materialize Pi metadata entries the forward path routed into
    // `view.events` (`model_change` / `thinking_level_change` / `label`).
    // Original ids and parentIds are preserved, so Pi's id/parentId tree
    // is faithful regardless of file position; for sane reading order,
    // insert each entry right after its parent (falling back to the tail
    // when the parent isn't in this session).
    insert_event_entries(&mut entries, &view.events);

    Ok(PiSession {
        header,
        entries,
        file_path: std::path::PathBuf::new(),
        parent: None,
    })
}

/// Map a `ConversationEvent` back to the Pi entry it was derived from.
/// Returns `None` for event types with no Pi analog (foreign-provider
/// events like Claude attachments), which are dropped rather than
/// emitted as garbage entries.
fn event_to_entry(event: &toolpath_convo::ConversationEvent) -> Option<Entry> {
    let base = EntryBase {
        id: event.id.clone(),
        parent_id: event.parent_id.clone(),
        timestamp: event.timestamp.clone(),
    };
    let str_field = |key: &str| -> String {
        event
            .data
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let extra_without = |consumed: &[&str]| -> HashMap<String, serde_json::Value> {
        event
            .data
            .iter()
            .filter(|(k, _)| !consumed.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    match event.event_type.as_str() {
        "model_change" => Some(Entry::ModelChange {
            base,
            provider: str_field("provider"),
            model_id: str_field("modelId"),
            extra: extra_without(&["provider", "modelId"]),
        }),
        "thinking_level_change" => Some(Entry::ThinkingLevelChange {
            base,
            thinking_level: str_field("thinkingLevel"),
            extra: extra_without(&["thinkingLevel"]),
        }),
        "label" => Some(Entry::Label {
            base,
            extra: event.data.clone(),
        }),
        _ => None,
    }
}

/// Insert re-materialized event entries into `entries`, each directly
/// after the entry whose id matches its `parent_id`. Events whose parent
/// isn't present (or who have none) append at the end. Siblings keep
/// their `view.events` order: insertion skips past event entries already
/// placed after the same parent.
fn insert_event_entries(entries: &mut Vec<Entry>, events: &[toolpath_convo::ConversationEvent]) {
    let entry_id = |e: &Entry| -> Option<String> {
        match e {
            Entry::Session(_) => None,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. } => Some(base.id.clone()),
        }
    };
    let mut inserted_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for event in events {
        let Some(entry) = event_to_entry(event) else {
            continue;
        };
        inserted_ids.insert(event.id.clone());
        let parent_pos = event.parent_id.as_ref().and_then(|pid| {
            entries
                .iter()
                .position(|e| entry_id(e).as_deref() == Some(pid.as_str()))
        });
        match parent_pos {
            Some(pos) => {
                let mut at = pos + 1;
                while at < entries.len()
                    && entry_id(&entries[at]).is_some_and(|id| inserted_ids.contains(&id))
                {
                    at += 1;
                }
                entries.insert(at, entry);
            }
            None => entries.push(entry),
        }
    }
}

/// Emit the entry (or entries) corresponding to a single turn's role
/// and content. Most turns produce a single `Entry::Message`; a turn
/// with assistant-side tool calls that have results produces both the
/// assistant message AND one tool-result message per result.
///
/// Metadata entries (`ModelChange`/`ThinkingLevelChange`/`Label`) never
/// reach this function — they ride `view.events` and are re-emitted by
/// [`insert_event_entries`]. The IR has no field to distinguish a
/// Pi-native `Entry::Compaction`/`BranchSummary`/`Custom`/
/// `CustomMessage` from an ordinary turn once round-tripped through
/// `Turn`, so those entry types are never re-synthesized — they always
/// fall through to the generic role-based mapping below.
fn emit_turn_entries(cfg: &PiProjector, turn: &Turn, entries: &mut Vec<Entry>) {
    match &turn.role {
        Role::User => emit_user(turn, entries),
        Role::Assistant => emit_assistant(cfg, turn, entries),
        Role::System => {
            // System turns from non-Pi sources don't have a direct
            // analog; fold them into a custom-system message.
            emit_system_as_custom(turn, entries);
        }
        Role::Other(other) => match other.as_str() {
            "tool" => emit_tool_result(turn, entries),
            "bash" => emit_bash_execution(turn, entries),
            o if o.starts_with("custom:") => {
                let custom_type = o.strip_prefix("custom:").unwrap_or(o).to_string();
                emit_custom_role_message(turn, &custom_type, entries);
            }
            _ => {
                // Unknown role — best-effort: store as user-role custom
                // message so the text survives in the log.
                emit_custom_role_message(turn, other, entries);
            }
        },
    }
}

fn emit_user(turn: &Turn, entries: &mut Vec<Entry>) {
    let content = MessageContent::Text(turn.text.clone());
    let timestamp = ts_millis(&turn.timestamp);
    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::User {
            content,
            timestamp,
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_assistant(cfg: &PiProjector, turn: &Turn, entries: &mut Vec<Entry>) {
    // Build the content blocks: optional thinking, then text, then
    // each tool call. Real Pi assistant turns interleave these in
    // arbitrary order, but for projection a thinking-then-text-then-
    // tool-calls layout reads cleanly.
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if let Some(t) = &turn.thinking
        && !t.is_empty()
    {
        blocks.push(ContentBlock::Thinking {
            thinking: t.clone(),
            extra: HashMap::new(),
        });
    }
    if !turn.text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: turn.text.clone(),
            extra: HashMap::new(),
        });
    }
    for tu in &turn.tool_uses {
        blocks.push(ContentBlock::ToolCall {
            id: tu.id.clone(),
            name: tool_native_name(tu),
            arguments: tu.input.clone(),
            extra: HashMap::new(),
        });
    }

    // The IR carries no provider-namespaced extras, so the source's
    // original `api`/`provider`/`errorMessage` can't be recovered on a
    // round-trip; fall back to the configured defaults.
    let api = cfg
        .default_api
        .clone()
        .unwrap_or_else(|| "anthropic".to_string());
    let provider = cfg
        .default_provider
        .clone()
        .unwrap_or_else(|| "anthropic".to_string());
    let model = turn.model.clone().unwrap_or_default();
    let usage = build_usage(turn);
    let stop_reason = parse_stop_reason(turn.stop_reason.as_deref());
    let error_message = None;
    let timestamp = ts_millis(&turn.timestamp);

    let assistant_id = turn.id.clone();
    let assistant_parent = turn.parent_id.clone();

    entries.push(Entry::Message {
        base: EntryBase {
            id: assistant_id.clone(),
            parent_id: assistant_parent,
            timestamp: turn.timestamp.clone(),
        },
        message: AgentMessage::Assistant {
            content: blocks,
            api,
            provider,
            model,
            usage,
            stop_reason,
            error_message,
            timestamp,
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });

    // Each tool invocation with a result produces a separate
    // `toolResult` entry parented to the assistant entry, mirroring
    // how Pi separates calls from results in the JSONL stream.
    let mut prev_id = assistant_id;
    let mut suffix = 0usize;
    for tu in &turn.tool_uses {
        let Some(result) = &tu.result else { continue };
        suffix += 1;
        let tr_id = format!("{}-tr-{}", turn.id, suffix);
        let entry = Entry::Message {
            base: EntryBase {
                id: tr_id.clone(),
                parent_id: Some(prev_id.clone()),
                timestamp: turn.timestamp.clone(),
            },
            message: AgentMessage::ToolResult {
                tool_call_id: tu.id.clone(),
                tool_name: tool_native_name(tu),
                content: vec![ToolResultContent::Text {
                    text: result.content.clone(),
                    extra: HashMap::new(),
                }],
                details: None,
                is_error: result.is_error,
                timestamp: ts_millis(&turn.timestamp),
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        entries.push(entry);
        prev_id = tr_id;
    }
}

fn emit_tool_result(turn: &Turn, entries: &mut Vec<Entry>) {
    // The IR carries no provider-namespaced extras, so the original
    // `toolCallId`/`toolName`/`isError`/`details` can't be recovered;
    // only `turn.text` survives.
    let tool_call_id = String::new();
    let tool_name = String::new();
    let is_error = false;
    let details = None;
    let content = vec![ToolResultContent::Text {
        text: turn.text.clone(),
        extra: HashMap::new(),
    }];
    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::ToolResult {
            tool_call_id,
            tool_name,
            content,
            details,
            is_error,
            timestamp: ts_millis(&turn.timestamp),
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_bash_execution(turn: &Turn, entries: &mut Vec<Entry>) {
    // The IR carries no provider-namespaced extras, so the original
    // `command`/`exitCode`/`cancelled`/`truncated`/`fullOutputPath`
    // can't be recovered; `turn.text` (the forward path's `$
    // <command>\n<truncated_output>` rendering) is the only surviving
    // record, and it's used verbatim as the output.
    let command = String::new();
    let exit_code = None;
    let cancelled = false;
    let truncated = false;
    let full_output_path = None;
    let output = turn.text.clone();

    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::BashExecution {
            command,
            output,
            exit_code,
            cancelled,
            truncated,
            full_output_path,
            exclude_from_context: None,
            timestamp: ts_millis(&turn.timestamp),
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_custom_role_message(turn: &Turn, custom_type: &str, entries: &mut Vec<Entry>) {
    let timestamp = ts_millis(&turn.timestamp);
    entries.push(Entry::Message {
        base: base_for(turn),
        message: AgentMessage::Custom {
            custom_type: custom_type.to_string(),
            content: MessageContent::Text(turn.text.clone()),
            display: true,
            details: None,
            timestamp,
            extra: HashMap::new(),
        },
        extra: HashMap::new(),
    });
}

fn emit_system_as_custom(turn: &Turn, entries: &mut Vec<Entry>) {
    emit_custom_role_message(turn, "system", entries);
}

// ── Helpers ──────────────────────────────────────────────────────────

fn base_for(turn: &Turn) -> EntryBase {
    EntryBase {
        id: turn.id.clone(),
        parent_id: turn.parent_id.clone(),
        timestamp: turn.timestamp.clone(),
    }
}

/// Convert an RFC3339 timestamp to Pi's `timestamp: u64` (epoch ms on
/// the inner message). Returns `0` if the timestamp is unparseable —
/// non-fatal since the outer `EntryBase.timestamp` keeps the original
/// string.
fn ts_millis(rfc3339: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

/// Build a `Usage` from `Turn.token_usage` and any `pi.cost` extras.
/// Non-Pi sources won't have cost information; default to zeros.
fn build_usage(turn: &Turn) -> Usage {
    let (input, output, cache_read, cache_write) = turn
        .token_usage
        .as_ref()
        .map(|u| {
            (
                u.input_tokens.unwrap_or(0) as u64,
                u.output_tokens.unwrap_or(0) as u64,
                u.cache_read_tokens.unwrap_or(0) as u64,
                u.cache_write_tokens.unwrap_or(0) as u64,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    let total_tokens = input + output;
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens,
        cost: CostBreakdown::default(),
    }
}

/// Resolve the assistant's `stopReason` from `Turn.stop_reason` (a
/// string), defaulting to `Stop`. The IR carries no provider-namespaced
/// extras, so a structured Pi-specific stop reason can't be recovered
/// verbatim on a round-trip.
fn parse_stop_reason(turn_stop: Option<&str>) -> StopReason {
    let s = turn_stop.unwrap_or("stop");
    serde_json::from_value::<StopReason>(json!(s))
        .unwrap_or(StopReason::Known(KnownStopReason::Stop))
}

/// Pick Pi's native tool name.
///
/// If the source tool has a category, route it through `native_name`
/// to land on Pi's canonical lowercase name (`bash`, `read`, `edit`,
/// etc.). This handles both same-harness pass-through (Pi's `read`
/// stays `read`) and cross-harness remapping (Claude's `Bash` becomes
/// `bash`). When the category is unknown, fall through to the source
/// name verbatim — Pi's format accepts any string here, so a custom
/// MCP tool name passes through cleanly.
fn tool_native_name(tu: &ToolInvocation) -> String {
    if let Some(cat) = tu.category
        && let Some(remap) = crate::provider::native_name(cat, &tu.input)
    {
        return remap.to_string();
    }
    tu.name.clone()
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
            timestamp: "2026-04-16T10:00:00Z".into(),
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
            timestamp: "2026-04-16T10:00:01Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: Some("claude-sonnet-4-5".into()),
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
            provider_id: Some("pi".into()),
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn test_empty_view_projects_session_with_just_header() {
        let session = PiProjector::default().project(&view_with(vec![])).unwrap();
        assert_eq!(session.header.id, "session-uuid");
        // Just the session header, no message entries.
        assert_eq!(session.entries.len(), 1);
        assert!(matches!(session.entries[0], Entry::Session(_)));
    }

    #[test]
    fn test_user_turn_becomes_user_message() {
        let session = PiProjector::default()
            .project(&view_with(vec![user_turn("u1", "hello")]))
            .unwrap();
        assert_eq!(session.entries.len(), 2);
        match &session.entries[1] {
            Entry::Message {
                base,
                message: AgentMessage::User { content, .. },
                ..
            } => {
                assert_eq!(base.id, "u1");
                match content {
                    MessageContent::Text(t) => assert_eq!(t, "hello"),
                    other => panic!("expected Text, got {:?}", other),
                }
            }
            other => panic!("expected User message, got {:?}", other),
        }
    }

    #[test]
    fn test_assistant_turn_with_tool_call_and_result() {
        let mut t = assistant_turn("a1", "I'll read it.");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "read".into(),
            input: serde_json::json!({"path": "x.rs"}),
            result: Some(ToolResult {
                content: "fn main(){}".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];
        let session = PiProjector::default().project(&view_with(vec![t])).unwrap();
        // session header + assistant + tool-result = 3 entries
        assert_eq!(session.entries.len(), 3);
        match &session.entries[1] {
            Entry::Message {
                message: AgentMessage::Assistant { content, .. },
                ..
            } => {
                // text + tool call = 2 blocks
                assert_eq!(content.len(), 2);
                assert!(
                    matches!(&content[0], ContentBlock::Text { text, .. } if text == "I'll read it.")
                );
                assert!(
                    matches!(&content[1], ContentBlock::ToolCall { id, name, .. } if id == "tc1" && name == "read")
                );
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
        match &session.entries[2] {
            Entry::Message {
                message:
                    AgentMessage::ToolResult {
                        tool_call_id,
                        tool_name,
                        content,
                        is_error,
                        ..
                    },
                ..
            } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(tool_name, "read");
                assert!(!is_error);
                assert_eq!(content.len(), 1);
                let ToolResultContent::Text { text, .. } = &content[0] else {
                    panic!("expected text content");
                };
                assert_eq!(text, "fn main(){}");
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn test_foreign_tool_name_remaps_via_category() {
        // Claude's `Bash` should land as Pi's `bash` because the category
        // routes it through `native_name(Shell, _)`.
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
            result: None,
            category: Some(ToolCategory::Shell),
        }];
        let session = PiProjector::default().project(&view_with(vec![t])).unwrap();
        match &session.entries[1] {
            Entry::Message {
                message: AgentMessage::Assistant { content, .. },
                ..
            } => match &content[0] {
                ContentBlock::ToolCall { name, .. } => assert_eq!(name, "bash"),
                other => panic!("expected ToolCall, got {:?}", other),
            },
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    #[test]
    fn test_assistant_thinking_becomes_thinking_block() {
        let mut t = assistant_turn("a1", "Done.");
        t.thinking = Some("hmm".into());
        let session = PiProjector::default().project(&view_with(vec![t])).unwrap();
        match &session.entries[1] {
            Entry::Message {
                message: AgentMessage::Assistant { content, .. },
                ..
            } => {
                assert_eq!(content.len(), 2);
                assert!(
                    matches!(&content[0], ContentBlock::Thinking { thinking, .. } if thinking == "hmm")
                );
                assert!(matches!(&content[1], ContentBlock::Text { text, .. } if text == "Done."));
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn test_session_header_uses_view_id_and_first_turn_cwd() {
        use toolpath_convo::EnvironmentSnapshot;
        let mut t = user_turn("u1", "hi");
        t.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/tmp/proj".into()),
            vcs_branch: None,
            vcs_revision: None,
        });
        let session = PiProjector::default().project(&view_with(vec![t])).unwrap();
        assert_eq!(session.header.cwd, "/tmp/proj");
    }

    #[test]
    fn test_cwd_override_wins_over_turn_environment() {
        use toolpath_convo::EnvironmentSnapshot;
        let mut t = user_turn("u1", "hi");
        t.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/tmp/proj".into()),
            vcs_branch: None,
            vcs_revision: None,
        });
        let session = PiProjector::new()
            .with_cwd("/abs/override")
            .project(&view_with(vec![t]))
            .unwrap();
        assert_eq!(session.header.cwd, "/abs/override");
    }

    #[test]
    fn test_assistant_default_api_provider_for_non_pi_source() {
        // The IR has no field to carry a source's original api/provider
        // — defaults should kick in regardless of source.
        let session = PiProjector::default()
            .project(&view_with(vec![assistant_turn("a1", "hi")]))
            .unwrap();
        match &session.entries[1] {
            Entry::Message {
                message:
                    AgentMessage::Assistant {
                        api,
                        provider,
                        usage,
                        ..
                    },
                ..
            } => {
                assert_eq!(api, "anthropic");
                assert_eq!(provider, "anthropic");
                assert_eq!(usage.input, 100);
                assert_eq!(usage.output, 50);
                assert_eq!(usage.total_tokens, 150);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn test_jsonl_serializes_per_entry_one_per_line() {
        // Sanity: each emitted Entry should serialize as a single
        // JSON object, suitable for line-by-line writes.
        let session = PiProjector::default()
            .project(&view_with(vec![user_turn("u1", "hi")]))
            .unwrap();
        for entry in &session.entries {
            let s = serde_json::to_string(entry).unwrap();
            assert!(
                !s.contains('\n'),
                "entry serialized with embedded newline: {}",
                s
            );
        }
    }

    fn event(
        id: &str,
        parent: Option<&str>,
        event_type: &str,
        data: &[(&str, serde_json::Value)],
    ) -> toolpath_convo::ConversationEvent {
        toolpath_convo::ConversationEvent {
            id: id.into(),
            timestamp: "2026-04-16T10:00:03Z".into(),
            parent_id: parent.map(String::from),
            event_type: event_type.into(),
            data: data.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    #[test]
    fn test_model_change_event_re_emitted_as_entry() {
        let mut view = view_with(vec![user_turn("u1", "hi")]);
        view.events.push(event(
            "mc-1",
            Some("u1"),
            "model_change",
            &[
                ("provider", serde_json::json!("anthropic")),
                ("modelId", serde_json::json!("claude-opus-4-7")),
                ("note", serde_json::json!("switched")),
            ],
        ));
        let session = PiProjector::default().project(&view).unwrap();
        let mc = session
            .entries
            .iter()
            .find_map(|e| match e {
                Entry::ModelChange {
                    base,
                    provider,
                    model_id,
                    extra,
                } => Some((base.clone(), provider.clone(), model_id.clone(), extra.clone())),
                _ => None,
            })
            .expect("expected a ModelChange entry in projected session");
        assert_eq!(mc.0.id, "mc-1");
        assert_eq!(mc.0.parent_id.as_deref(), Some("u1"));
        assert_eq!(mc.0.timestamp, "2026-04-16T10:00:03Z");
        assert_eq!(mc.1, "anthropic");
        assert_eq!(mc.2, "claude-opus-4-7");
        assert_eq!(mc.3.get("note"), Some(&serde_json::json!("switched")));
    }

    #[test]
    fn test_thinking_level_change_event_re_emitted_as_entry() {
        let mut view = view_with(vec![user_turn("u1", "hi")]);
        view.events.push(event(
            "tlc-1",
            Some("u1"),
            "thinking_level_change",
            &[("thinkingLevel", serde_json::json!("high"))],
        ));
        let session = PiProjector::default().project(&view).unwrap();
        let found = session.entries.iter().any(|e| {
            matches!(e, Entry::ThinkingLevelChange { base, thinking_level, .. }
                if base.id == "tlc-1" && thinking_level == "high")
        });
        assert!(found, "expected a ThinkingLevelChange entry");
    }

    #[test]
    fn test_label_event_re_emitted_as_entry() {
        let mut view = view_with(vec![user_turn("u1", "hi")]);
        view.events.push(event(
            "lbl-1",
            Some("u1"),
            "label",
            &[("label", serde_json::json!("checkpoint"))],
        ));
        let session = PiProjector::default().project(&view).unwrap();
        let found = session.entries.iter().any(|e| {
            matches!(e, Entry::Label { base, extra }
                if base.id == "lbl-1" && extra.get("label") == Some(&serde_json::json!("checkpoint")))
        });
        assert!(found, "expected a Label entry");
    }

    #[test]
    fn test_event_entry_inserted_after_its_parent() {
        let mut view = view_with(vec![user_turn("u1", "hi"), assistant_turn("a1", "reply")]);
        view.events.push(event(
            "mc-1",
            Some("u1"),
            "model_change",
            &[
                ("provider", serde_json::json!("anthropic")),
                ("modelId", serde_json::json!("m")),
            ],
        ));
        let session = PiProjector::default().project(&view).unwrap();
        let ids: Vec<&str> = session
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Session(_) => None,
                Entry::Message { base, .. }
                | Entry::ModelChange { base, .. }
                | Entry::ThinkingLevelChange { base, .. }
                | Entry::Compaction { base, .. }
                | Entry::BranchSummary { base, .. }
                | Entry::Custom { base, .. }
                | Entry::CustomMessage { base, .. }
                | Entry::Label { base, .. } => Some(base.id.as_str()),
            })
            .collect();
        let u1 = ids.iter().position(|i| *i == "u1").unwrap();
        let mc = ids.iter().position(|i| *i == "mc-1").unwrap();
        let a1 = ids.iter().position(|i| *i == "a1").unwrap();
        assert!(
            u1 < mc && mc < a1,
            "expected mc-1 between u1 and a1, got order {:?}",
            ids
        );
    }

    #[test]
    fn test_foreign_event_types_are_not_emitted() {
        // Events from other providers (e.g. a Claude attachment) have no
        // Pi entry analog and must not produce garbage entries.
        let mut view = view_with(vec![user_turn("u1", "hi")]);
        view.events.push(event(
            "att-1",
            Some("u1"),
            "attachment",
            &[("path", serde_json::json!("/tmp/x"))],
        ));
        let session = PiProjector::default().project(&view).unwrap();
        // header + one user message only
        assert_eq!(session.entries.len(), 2);
    }
}
