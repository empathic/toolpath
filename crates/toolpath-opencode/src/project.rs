//! [`OpencodeProjector`] — maps a [`ConversationView`] back to an
//! opencode [`Session`] (with messages and parts populated).

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Map, Value};
use toolpath_convo::{
    Compaction, CompactionTrigger, ConversationProjector, ConversationView, ConvoError, Result,
    Role, ToolInvocation, Turn,
};

use crate::types::{
    AssistantMessage, CompactionPart, Message, MessageData, MessagePath, MessageTime, ModelRef,
    Part, PartData, ReasoningPart, Session, StepFinishPart, StepStartPart, TextPart, TimeRange,
    Tokens, ToolPart, ToolRunTime, ToolState, ToolStateCompleted, ToolStateError, UserMessage,
    UserSummary,
};

const DEFAULT_AGENT: &str = "build";
const DEFAULT_MODEL_PROVIDER: &str = "anthropic";
const DEFAULT_MODEL_ID: &str = "unknown";
const DEFAULT_VERSION: &str = "0.0.0-projected";

#[derive(Debug, Clone, Default)]
pub struct OpencodeProjector {
    pub project_id: Option<String>,
    pub directory: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub agent: Option<String>,
    pub default_model_provider: Option<String>,
    pub default_model_id: Option<String>,
    pub version: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
}

impl OpencodeProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_project_id(mut self, id: impl Into<String>) -> Self {
        self.project_id = Some(id.into());
        self
    }

    pub fn with_directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.directory = Some(dir.into());
        self
    }

    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.version = Some(v.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

impl ConversationProjector for OpencodeProjector {
    type Output = Session;

    fn project(&self, view: &ConversationView) -> Result<Session> {
        project_view(self, view).map_err(ConvoError::Provider)
    }
}

fn project_view(
    cfg: &OpencodeProjector,
    view: &ConversationView,
) -> std::result::Result<Session, String> {
    let directory = cfg
        .directory
        .clone()
        .or_else(|| {
            view.turns()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/"));

    let project_id = cfg
        .project_id
        .clone()
        .unwrap_or_else(|| derive_project_id(&directory));

    let agent = cfg
        .agent
        .clone()
        .unwrap_or_else(|| DEFAULT_AGENT.to_string());
    let version = cfg
        .version
        .clone()
        .unwrap_or_else(|| DEFAULT_VERSION.to_string());

    let session_id = if view.id.starts_with("ses_") {
        view.id.clone()
    } else {
        mint_session_id(&view.id)
    };

    let time_created = view
        .started_at
        .map(|t| t.timestamp_millis())
        .or_else(|| {
            view.turns()
                .next()
                .and_then(|t| parse_timestamp_ms(&t.timestamp))
        })
        .unwrap_or(0);
    let time_updated = view
        .last_activity
        .map(|t| t.timestamp_millis())
        .or_else(|| {
            view.turns()
                .last()
                .and_then(|t| parse_timestamp_ms(&t.timestamp))
        })
        .unwrap_or(time_created);

    let title = cfg
        .title
        .clone()
        .or_else(|| {
            view.turns()
                .filter(|t| matches!(t.role, Role::User))
                .map(|t| t.text.as_str())
                .find(|t| !t.is_empty() && !is_system_envelope(t))
                .map(truncate_title)
        })
        .unwrap_or_else(|| "Projected session".to_string());

    let slug = cfg.slug.clone().unwrap_or_else(|| slugify(&title));

    let mut messages: Vec<Message> = Vec::new();
    let mut prev_msg_id: Option<String> = None;
    let mut counter: u32 = 0;
    // IR turn id → the message id we re-mint for it, so a compaction's kept
    // tail anchor (an IR turn id) can be rewritten to the id the projected
    // session actually carries — otherwise `tailStartID` would dangle and the
    // kept anchor would collapse to `None` on re-read.
    let mut id_map: HashMap<String, String> = HashMap::new();
    // Indexes (into `messages`) of boundary messages whose `tailStartID`
    // couldn't be resolved to an already-projected message — wholesale
    // boundaries (`kept_from: None`) and anchors with no projected turn
    // before the boundary. Their `tailStartID` is the first message
    // written after the boundary, patched in once it exists.
    let mut wholesale_boundaries: Vec<usize> = Vec::new();
    // IR id of the assistant turn that carries a boundary's summary text
    // (opencode ≥ 1.17 writes the compaction summary as an assistant
    // message flagged `summary: true`). Set when a compaction's summary
    // matches the next turn's text; consumed when that turn is projected.
    let mut pending_summary_turn: Option<String> = None;

    let default_provider = cfg
        .default_model_provider
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL_PROVIDER.to_string());
    let default_model = cfg
        .default_model_id
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string());

    // Walk the ordered item stream so compaction boundaries land at their
    // true position (between the turns they separate) — the inverse of the
    // forward Builder, which reads `compaction` parts in message order.
    for (idx, item) in view.items.iter().enumerate() {
        match item {
            toolpath_convo::Item::Turn(turn) => match turn.role {
                Role::User => {
                    let msg = build_user_message(
                        turn,
                        &session_id,
                        &mut counter,
                        &agent,
                        &default_provider,
                        &default_model,
                    );
                    id_map.insert(turn.id.clone(), msg.id.clone());
                    prev_msg_id = Some(msg.id.clone());
                    messages.push(msg);
                }
                Role::Assistant => {
                    let parent = prev_msg_id
                        .clone()
                        .unwrap_or_else(|| mint_message_id(&session_id, counter));
                    let mut msg = build_assistant_message(
                        turn,
                        &session_id,
                        &mut counter,
                        parent,
                        &directory,
                        &agent,
                        &default_provider,
                        &default_model,
                    );
                    if pending_summary_turn.as_deref() == Some(turn.id.as_str()) {
                        pending_summary_turn = None;
                        if let MessageData::Assistant(a) = &mut msg.data {
                            a.summary = Some(true);
                        }
                    }
                    id_map.insert(turn.id.clone(), msg.id.clone());
                    prev_msg_id = Some(msg.id.clone());
                    messages.push(msg);
                }
                Role::System | Role::Other(_) => {
                    // opencode has no system-role message variant; fold the
                    // text into the next user/assistant turn's context by
                    // skipping. The system prompt itself rides on
                    // UserMessage.system if needed.
                }
            },
            toolpath_convo::Item::Compaction(c) => {
                // Inverse of the forward Builder's compaction handling:
                // emit a synthetic compaction-bearing user message so a
                // re-read reproduces the `Item::Compaction`. When the
                // boundary carries a summary, either the next assistant
                // turn already IS the summary (its text matches — the
                // opencode ≥ 1.17 shape; mark it `summary: true` when it's
                // projected) or we emit the synthetic summary user message
                // the forward path reads from `UserMessage.summary.body`.
                // Rewrite the kept anchor (an IR turn id) to the message id
                // we minted for its turn — resolving forward past turns
                // that project no message (see `resolve_tail_anchor`).
                // When nothing before the boundary projects (wholesale, or
                // a System-anchored run with no projectable turn), the
                // boundary anchors on the first message written after it —
                // patched in below — or omits `tailStartID` when nothing
                // follows.
                // Synthetic boundary/summary messages don't advance
                // `prev_msg_id`: assistant turns parent on the previous
                // conversational message, never on a marker message that
                // won't re-read as a turn.
                let tail_anchor = c
                    .kept_from
                    .as_ref()
                    .and_then(|k| resolve_tail_anchor(k, view, &id_map));
                if tail_anchor.is_none() {
                    wholesale_boundaries.push(messages.len());
                }
                let summary_turn = c.summary.as_deref().and_then(|s| {
                    view.items[idx + 1..]
                        .iter()
                        .find_map(toolpath_convo::Item::as_turn)
                        .filter(|t| t.role == Role::Assistant && t.text == s)
                        .map(|t| t.id.clone())
                });
                let emit_summary_message = summary_turn.is_none();
                if summary_turn.is_some() {
                    pending_summary_turn = summary_turn;
                }
                messages.extend(build_compaction_messages(
                    c,
                    tail_anchor,
                    emit_summary_message,
                    &session_id,
                    &mut counter,
                    &agent,
                    &default_provider,
                    &default_model,
                ));
            }
            toolpath_convo::Item::Event(_) => {
                // Non-conversational events have no opencode message form;
                // they're metadata that doesn't round-trip through parts.
            }
        }
    }

    for idx in wholesale_boundaries {
        let next_id = match messages.get(idx + 1) {
            Some(m) => m.id.clone(),
            None => continue,
        };
        if let Some(msg) = messages.get_mut(idx) {
            for part in &mut msg.parts {
                if let PartData::Compaction(cp) = &mut part.data {
                    cp.tail_start_id = Some(next_id.clone());
                }
            }
        }
    }

    monotonize_times(&mut messages);

    Ok(Session {
        id: session_id,
        project_id,
        workspace_id: cfg.workspace_id.clone(),
        parent_id: None,
        slug,
        directory,
        title,
        version,
        share_url: None,
        summary_additions: None,
        summary_deletions: None,
        summary_files: None,
        time_created,
        time_updated,
        time_compacting: None,
        time_archived: None,
        messages,
    })
}

/// Force strictly increasing `time_created` across the emitted message
/// sequence. The real reader orders rows by `time_created ASC, id ASC`,
/// so a message whose native time is at-or-before its predecessor's (e.g.
/// a compaction boundary stamped later than the host assistant message
/// that follows it) would MOVE on re-read. Bumping each such message to
/// its predecessor's time + 1 keeps re-read order identical to emission
/// order.
fn monotonize_times(messages: &mut [Message]) {
    let mut prev: Option<i64> = None;
    for msg in messages {
        let t = match prev {
            Some(p) if msg.time_created <= p => p + 1,
            _ => msg.time_created,
        };
        if t != msg.time_created {
            msg.time_created = t;
            msg.time_updated = msg.time_updated.max(t);
            match &mut msg.data {
                MessageData::User(u) => u.time.created = t,
                MessageData::Assistant(a) => {
                    a.time.created = t;
                    if a.time.completed.is_some_and(|c| c < t) {
                        a.time.completed = Some(t);
                    }
                }
                MessageData::Other => {}
            }
        }
        prev = Some(t);
    }
}

fn build_user_message(
    turn: &Turn,
    session_id: &str,
    counter: &mut u32,
    agent: &str,
    default_provider: &str,
    default_model: &str,
) -> Message {
    *counter += 1;
    let msg_id = mint_message_id(session_id, *counter);
    let time_created = parse_timestamp_ms(&turn.timestamp).unwrap_or(0);

    let opencode_extras = opencode_extras(turn);
    let model = opencode_extras
        .as_ref()
        .and_then(|m| m.get("model"))
        .and_then(|v| serde_json::from_value::<ModelRef>(v.clone()).ok())
        .unwrap_or_else(|| ModelRef {
            provider_id: default_provider.to_string(),
            model_id: default_model.to_string(),
            variant: None,
        });

    let user = UserMessage {
        time: MessageTime {
            created: time_created,
            completed: None,
        },
        agent: agent.to_string(),
        model,
        format: None,
        summary: Some(crate::types::UserSummary {
            title: None,
            body: None,
            diffs: vec![],
            extra: HashMap::new(),
        }),
        system: None,
        tools: None,
        extra: HashMap::new(),
    };

    let mut parts: Vec<Part> = Vec::new();
    if !turn.text.is_empty() {
        *counter += 1;
        parts.push(Part {
            id: mint_part_id(session_id, *counter),
            message_id: msg_id.clone(),
            session_id: session_id.to_string(),
            time_created,
            time_updated: time_created,
            data: PartData::Text(TextPart {
                text: turn.text.clone(),
                synthetic: None,
                ignored: None,
                time: None,
                metadata: None,
                extra: HashMap::new(),
            }),
        });
    }

    Message {
        id: msg_id,
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: MessageData::User(user),
        parts,
    }
}

/// Rewrite a compaction's kept anchor (an IR turn id) to a message id the
/// projected session actually carries. A directly projected anchor maps
/// through `id_map`; an anchor whose turn projects no message (System /
/// Other roles) resolves forward to the first projected turn at-or-after
/// it in the view's item order — a grown kept run is acceptable, a
/// dangling foreign id is not. `None` when nothing at-or-after the anchor
/// has been projected yet; the caller then patches in the first
/// post-boundary message, or omits `tailStartID` when nothing follows.
fn resolve_tail_anchor(
    anchor: &str,
    view: &ConversationView,
    id_map: &HashMap<String, String>,
) -> Option<String> {
    if let Some(mapped) = id_map.get(anchor) {
        return Some(mapped.clone());
    }
    let pos = view
        .items
        .iter()
        .position(|i| matches!(i, toolpath_convo::Item::Turn(t) if t.id == anchor))?;
    view.items[pos..]
        .iter()
        .filter_map(toolpath_convo::Item::as_turn)
        .find_map(|t| id_map.get(&t.id).cloned())
}

/// Inverse of the forward Builder's `push_compaction`: project an
/// [`Item::Compaction`] back into opencode messages so a re-read
/// reproduces it.
///
/// opencode writes a compaction as a synthetic user message carrying a
/// single `compaction` part. We mirror that: one user message whose only
/// part is a [`CompactionPart`]. When the boundary has a `summary` and
/// `emit_summary_message` is set (no assistant turn already carries the
/// text), we also emit, immediately after, the synthetic summary user
/// message the forward path pairs with this boundary via its
/// `UserMessage.summary.body`.
#[allow(clippy::too_many_arguments)]
fn build_compaction_messages(
    c: &Compaction,
    tail_anchor: Option<String>,
    emit_summary_message: bool,
    session_id: &str,
    counter: &mut u32,
    agent: &str,
    default_provider: &str,
    default_model: &str,
) -> Vec<Message> {
    let time_created = parse_timestamp_ms(&c.timestamp).unwrap_or(0);
    let model = ModelRef {
        provider_id: default_provider.to_string(),
        model_id: default_model.to_string(),
        variant: None,
    };

    let mut out = Vec::new();

    *counter += 1;
    let msg_id = mint_message_id(session_id, *counter);
    let user = UserMessage {
        time: MessageTime {
            created: time_created,
            completed: None,
        },
        agent: agent.to_string(),
        model: model.clone(),
        format: None,
        summary: Some(UserSummary {
            title: None,
            body: None,
            diffs: vec![],
            extra: HashMap::new(),
        }),
        system: None,
        tools: None,
        extra: HashMap::new(),
    };

    *counter += 1;
    let compaction_part = Part {
        id: mint_part_id(session_id, *counter),
        message_id: msg_id.clone(),
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: PartData::Compaction(CompactionPart {
            auto: c.trigger == Some(CompactionTrigger::Auto),
            overflow: None,
            // The kept tail anchors on the earliest surviving turn — already
            // rewritten by the caller to the message id this projection minted
            // for it, so the `tailStartID` wire key resolves on re-read.
            tail_start_id: tail_anchor,
            extra: HashMap::new(),
        }),
    };

    out.push(Message {
        id: msg_id,
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: MessageData::User(user),
        parts: vec![compaction_part],
    });

    if let Some(summary) = c
        .summary
        .as_ref()
        .filter(|s| !s.is_empty() && emit_summary_message)
    {
        // The summary message must sort strictly AFTER the compaction message
        // so the reader (which orders by `(time_created ASC, id ASC)`) sees the
        // boundary first and pairs this summary with it. Sharing `time_created`
        // would leave the order to the non-monotonic minted ids — dropping the
        // summary whenever it happened to sort first.
        let summary_time = time_created + 1;
        *counter += 1;
        let summary_msg_id = mint_message_id(session_id, *counter);
        let summary_user = UserMessage {
            time: MessageTime {
                created: summary_time,
                completed: None,
            },
            agent: agent.to_string(),
            model,
            format: None,
            summary: Some(UserSummary {
                title: None,
                body: Some(summary.clone()),
                diffs: vec![],
                extra: HashMap::new(),
            }),
            system: None,
            tools: None,
            extra: HashMap::new(),
        };
        out.push(Message {
            id: summary_msg_id,
            session_id: session_id.to_string(),
            time_created: summary_time,
            time_updated: summary_time,
            data: MessageData::User(summary_user),
            parts: Vec::new(),
        });
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn build_assistant_message(
    turn: &Turn,
    session_id: &str,
    counter: &mut u32,
    parent_id: String,
    cwd: &std::path::Path,
    agent: &str,
    default_provider: &str,
    default_model: &str,
) -> Message {
    *counter += 1;
    let msg_id = mint_message_id(session_id, *counter);
    let time_created = parse_timestamp_ms(&turn.timestamp).unwrap_or(0);

    let extras = opencode_extras(turn);
    let provider_id = extras
        .as_ref()
        .and_then(|m| m.get("providerID"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default_provider.to_string());
    let model_id = turn
        .model
        .clone()
        .or_else(|| {
            extras
                .as_ref()
                .and_then(|m| m.get("modelID"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| default_model.to_string());

    let tokens = turn
        .token_usage
        .as_ref()
        .map(|u| {
            let input = u.input_tokens.unwrap_or(0) as u64;
            let output = u.output_tokens.unwrap_or(0) as u64;
            let cache_read = u.cache_read_tokens.unwrap_or(0) as u64;
            let cache_write = u.cache_write_tokens.unwrap_or(0) as u64;
            Tokens {
                total: Some(input + output + cache_read + cache_write),
                input,
                output,
                reasoning: 0,
                cache: crate::types::TokenCache {
                    read: cache_read,
                    write: cache_write,
                },
            }
        })
        .unwrap_or_default();

    let assistant = AssistantMessage {
        parent_id,
        time: MessageTime {
            created: time_created,
            completed: Some(time_created),
        },
        error: None,
        agent: agent.to_string(),
        // Required by opencode's schema (deprecated but still validated).
        mode: Some(agent.to_string()),
        model_id: model_id.clone(),
        provider_id: provider_id.clone(),
        path: MessagePath {
            cwd: cwd.to_path_buf(),
            root: cwd.to_path_buf(),
        },
        summary: None,
        cost: 0.0,
        tokens: tokens.clone(),
        structured: None,
        variant: None,
        finish: turn.stop_reason.clone(),
        extra: HashMap::new(),
    };

    let mut parts: Vec<Part> = Vec::new();
    let snapshot = extras
        .as_ref()
        .and_then(|m| m.get("snapshots"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string);

    *counter += 1;
    parts.push(Part {
        id: mint_part_id(session_id, *counter),
        message_id: msg_id.clone(),
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: PartData::StepStart(StepStartPart {
            snapshot: snapshot.clone(),
            extra: HashMap::new(),
        }),
    });

    if let Some(thinking) = &turn.thinking
        && !thinking.is_empty()
    {
        *counter += 1;
        parts.push(Part {
            id: mint_part_id(session_id, *counter),
            message_id: msg_id.clone(),
            session_id: session_id.to_string(),
            time_created,
            time_updated: time_created,
            data: PartData::Reasoning(ReasoningPart {
                text: thinking.clone(),
                time: Some(TimeRange {
                    start: time_created,
                    end: Some(time_created),
                }),
                metadata: None,
                extra: HashMap::new(),
            }),
        });
    }

    if !turn.text.is_empty() {
        *counter += 1;
        parts.push(Part {
            id: mint_part_id(session_id, *counter),
            message_id: msg_id.clone(),
            session_id: session_id.to_string(),
            time_created,
            time_updated: time_created,
            data: PartData::Text(TextPart {
                text: turn.text.clone(),
                synthetic: None,
                ignored: None,
                time: Some(TimeRange {
                    start: time_created,
                    end: Some(time_created),
                }),
                metadata: None,
                extra: HashMap::new(),
            }),
        });
    }

    for tu in &turn.tool_uses {
        *counter += 1;
        let part_id = mint_part_id(session_id, *counter);
        parts.push(Part {
            id: part_id,
            message_id: msg_id.clone(),
            session_id: session_id.to_string(),
            time_created,
            time_updated: time_created,
            data: PartData::Tool(build_tool_part(tu, time_created)),
        });
    }

    *counter += 1;
    parts.push(Part {
        id: mint_part_id(session_id, *counter),
        message_id: msg_id.clone(),
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: PartData::StepFinish(StepFinishPart {
            reason: turn
                .stop_reason
                .clone()
                .unwrap_or_else(|| "stop".to_string()),
            snapshot,
            cost: 0.0,
            tokens,
            extra: HashMap::new(),
        }),
    });

    Message {
        id: msg_id,
        session_id: session_id.to_string(),
        time_created,
        time_updated: time_created,
        data: MessageData::Assistant(assistant),
        parts,
    }
}

fn build_tool_part(tu: &ToolInvocation, time_created: i64) -> ToolPart {
    let tool_name = native_tool_name(tu);
    let input = normalize_tool_input(&tool_name, &tu.input);
    let title = synthesize_title(&tool_name, &input);

    let state = match &tu.result {
        Some(r) if r.is_error => ToolState::Error(ToolStateError {
            input,
            error: r.content.clone(),
            metadata: None,
            time: ToolRunTime {
                start: time_created,
                end: time_created,
                compacted: None,
            },
        }),
        Some(r) => {
            let metadata = synthesize_metadata(&tool_name, &r.content, &input);
            ToolState::Completed(ToolStateCompleted {
                input,
                output: r.content.clone(),
                title: title.clone(),
                metadata,
                time: ToolRunTime {
                    start: time_created,
                    end: time_created,
                    compacted: None,
                },
                attachments: None,
            })
        }
        None => ToolState::Completed(ToolStateCompleted {
            input,
            output: String::new(),
            title: title.clone(),
            metadata: Value::Object(Map::new()),
            time: ToolRunTime {
                start: time_created,
                end: time_created,
                compacted: None,
            },
            attachments: None,
        }),
    };

    ToolPart {
        tool: tool_name,
        call_id: tu.id.clone(),
        state,
        metadata: None,
        extra: HashMap::new(),
    }
}

fn native_tool_name(tu: &ToolInvocation) -> String {
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

fn synthesize_title(tool: &str, input: &Value) -> String {
    match tool {
        "bash" => input
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| input.get("command").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        "read" | "edit" | "write" => input
            .get("filePath")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "grep" => input
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "glob" => input
            .get("pattern")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// Map Claude/Codex-shape arg keys onto opencode's camelCase vocabulary
/// — opencode's TUI reads `filePath`, `oldString`, `newString` and shows
/// "preparing edit..." when those are missing.
fn normalize_tool_input(tool: &str, input: &Value) -> Value {
    let Some(obj) = input.as_object() else {
        return input.clone();
    };
    let rename = |obj: &mut Map<String, Value>, from: &str, to: &str| {
        if obj.contains_key(to) {
            return;
        }
        if let Some(v) = obj.remove(from) {
            obj.insert(to.to_string(), v);
        }
    };
    let mut out = obj.clone();
    match tool {
        "read" => {
            rename(&mut out, "file_path", "filePath");
        }
        "write" => {
            rename(&mut out, "file_path", "filePath");
        }
        "edit" => {
            rename(&mut out, "file_path", "filePath");
            rename(&mut out, "old_string", "oldString");
            rename(&mut out, "new_string", "newString");
        }
        _ => {}
    }
    Value::Object(out)
}

fn synthesize_metadata(tool: &str, output: &str, input: &Value) -> Value {
    let mut m = Map::new();
    match tool {
        "bash" => {
            m.insert("output".to_string(), Value::String(output.to_string()));
            m.insert("exit".to_string(), Value::Number(0.into()));
            m.insert("truncated".to_string(), Value::Bool(false));
        }
        "edit" => {
            m.insert("diagnostics".to_string(), Value::Object(Map::new()));
            if let Some(diff) = synthesize_edit_diff(input) {
                m.insert("diff".to_string(), Value::String(diff));
            }
        }
        "write" => {
            m.insert("diagnostics".to_string(), Value::Object(Map::new()));
        }
        "read" => {
            m.insert("diagnostics".to_string(), Value::Object(Map::new()));
        }
        _ => {}
    }
    Value::Object(m)
}

fn synthesize_edit_diff(input: &Value) -> Option<String> {
    let path = input.get("filePath").and_then(Value::as_str)?;
    let old = input.get("oldString").and_then(Value::as_str)?;
    let new_s = input.get("newString").and_then(Value::as_str)?;
    let old_lines: Vec<&str> = if old.is_empty() {
        vec![]
    } else {
        old.split('\n').collect()
    };
    let new_lines: Vec<&str> = if new_s.is_empty() {
        vec![]
    } else {
        new_s.split('\n').collect()
    };
    let old_count = old_lines.len();
    let new_count = new_lines.len();
    let old_start = if old_count == 0 { 0 } else { 1 };
    let new_start = if new_count == 0 { 0 } else { 1 };
    let mut out = String::new();
    out.push_str(&format!("Index: {}\n", path));
    out.push_str("===================================================================\n");
    out.push_str(&format!("--- {}\n", path));
    out.push_str(&format!("+++ {}\n", path));
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        old_start, old_count, new_start, new_count
    ));
    for line in &old_lines {
        out.push_str(&format!("-{}\n", line));
    }
    for line in &new_lines {
        out.push_str(&format!("+{}\n", line));
    }
    Some(out)
}

fn opencode_extras(_turn: &Turn) -> Option<&'static Map<String, Value>> {
    None
}

fn mint_session_id(seed: &str) -> String {
    format!("ses_{}", stable_hex24(seed))
}

fn mint_message_id(session_id: &str, n: u32) -> String {
    format!("msg_{}", stable_hex24(&format!("{}-msg-{}", session_id, n)))
}

fn mint_part_id(session_id: &str, n: u32) -> String {
    format!("prt_{}", stable_hex24(&format!("{}-prt-{}", session_id, n)))
}

fn stable_hex24(seed: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(seed.as_bytes());
    let bytes = h.finalize();
    hex::encode(&bytes[..12])
}

fn parse_timestamp_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn truncate_title(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    first_line.chars().take(120).collect()
}

fn is_system_envelope(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('<') && trimmed.contains('>')
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in title.chars().take(60) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed
    }
}

/// Best-effort fallback when the caller hasn't supplied a project_id.
/// Real opencode keys this off the first-root-commit SHA; for projected
/// sessions we use the SHA-1 of the worktree path as a stable
/// substitute.
fn derive_project_id(directory: &std::path::Path) -> String {
    stable_hex40(directory.to_string_lossy().as_bytes())
}

fn stable_hex40(seed: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(seed);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolpath_convo::{ToolCategory, ToolInvocation, ToolResult};

    fn user_turn(text: &str) -> Turn {
        Turn {
            id: "u1".into(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-04-21T12:00:00.000Z".into(),
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

    fn assistant_turn(text: &str) -> Turn {
        Turn {
            id: "a1".into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-04-21T12:00:01.000Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: Some("claude-sonnet-4-6".into()),
            stop_reason: Some("stop".into()),
            token_usage: None,
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
            provider_id: Some("opencode".into()),
            files_changed: vec![],
            session_ids: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn empty_view_yields_session_with_no_messages() {
        let s = OpencodeProjector::default()
            .project(&view_with(vec![]))
            .unwrap();
        assert!(s.id.starts_with("ses_"));
        assert!(s.messages.is_empty());
    }

    #[test]
    fn compaction_summary_and_kept_anchor_survive_projection() {
        use toolpath_convo::Item;
        // A compaction with a summary and a kept tail anchored on the
        // assistant turn `a1`. The projector must (a) rewrite the anchor to
        // the message id it mints for `a1` (not the raw IR id, which no
        // projected message carries), and (b) give the summary message a
        // strictly-later timestamp so the reader — which orders by
        // (time_created, id) — pairs it with the boundary instead of dropping
        // it on a hash-id tie.
        let mut view = view_with(vec![user_turn("first"), assistant_turn("ans")]);
        view.items.push(Item::Compaction(Compaction {
            id: "c".into(),
            parent_id: Some("a1".into()),
            timestamp: "2026-04-21T12:00:02.000Z".into(),
            trigger: Some(CompactionTrigger::Auto),
            summary: Some("condensed".into()),
            pre_tokens: None,
            kept_from: Some("a1".into()),
        }));

        let s = OpencodeProjector::default().project(&view).unwrap();

        let assistant_id = s
            .messages
            .iter()
            .find(|m| matches!(m.data, MessageData::Assistant(_)))
            .map(|m| m.id.clone())
            .expect("assistant message");

        let comp_msg = s
            .messages
            .iter()
            .find(|m| {
                m.parts
                    .iter()
                    .any(|p| matches!(p.data, PartData::Compaction(_)))
            })
            .expect("compaction-bearing message");
        let cp = comp_msg
            .parts
            .iter()
            .find_map(|p| match &p.data {
                PartData::Compaction(cp) => Some(cp),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            cp.tail_start_id.as_deref(),
            Some(assistant_id.as_str()),
            "kept anchor rewritten to the minted message id"
        );

        let summary_msg = s
            .messages
            .iter()
            .find(|m| match &m.data {
                MessageData::User(u) => {
                    u.summary.as_ref().and_then(|sm| sm.body.as_ref()).is_some()
                }
                _ => false,
            })
            .expect("summary message");
        assert!(
            summary_msg.time_created > comp_msg.time_created,
            "summary must sort after the compaction message"
        );
    }

    #[test]
    fn system_anchored_kept_from_resolves_forward_to_projected_turn() {
        use toolpath_convo::Item;
        // The kept anchor names a System-role turn — a role opencode can't
        // project, so the anchor has no minted message id. It must resolve
        // FORWARD to the first projected turn at-or-after it (keep-more),
        // never write the raw IR id into `tailStartID` as a dangling
        // foreign id.
        let mut sys = user_turn("branch context");
        sys.id = "sys1".into();
        sys.role = Role::System;
        let mut u1 = user_turn("first");
        u1.parent_id = Some("sys1".into());
        u1.timestamp = "2026-04-21T12:00:01.000Z".into();
        let mut a1 = assistant_turn("ans");
        a1.parent_id = Some("u1".into());
        a1.timestamp = "2026-04-21T12:00:02.000Z".into();
        let mut view = view_with(vec![sys, u1, a1]);
        view.items.push(Item::Compaction(Compaction {
            id: "c".into(),
            parent_id: Some("a1".into()),
            timestamp: "2026-04-21T12:00:03.000Z".into(),
            trigger: Some(CompactionTrigger::Auto),
            summary: None,
            pre_tokens: None,
            kept_from: Some("sys1".into()),
        }));
        let mut u2 = user_turn("after");
        u2.id = "u2".into();
        u2.parent_id = Some("c".into());
        u2.timestamp = "2026-04-21T12:00:04.000Z".into();
        view.items.push(Item::Turn(u2));

        let s = OpencodeProjector::default().project(&view).unwrap();

        let first_msg_id = s.messages.first().map(|m| m.id.clone()).unwrap();
        let cp = s
            .messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match &p.data {
                PartData::Compaction(cp) => Some(cp),
                _ => None,
            })
            .expect("compaction part");
        assert_eq!(
            cp.tail_start_id.as_deref(),
            Some(first_msg_id.as_str()),
            "anchor resolves forward to u1's minted message id"
        );

        let reread = crate::provider::to_view(&s);
        let c = reread.compactions().next().expect("boundary survives");
        let kept = toolpath_convo::expand_kept(&reread.items, c);
        let kept_texts: Vec<&str> = kept
            .iter()
            .map(|id| reread.turns().find(|t| &t.id == id).unwrap().text.as_str())
            .collect();
        assert_eq!(
            kept_texts,
            vec!["first", "ans"],
            "the re-read kept run covers every projectable original kept turn"
        );
        assert!(
            toolpath_convo::testing::check_view_invariants(&reread).is_empty(),
            "invariants must accept the re-read view"
        );
    }

    #[test]
    fn system_anchor_with_no_prior_projected_turn_patches_past_boundary() {
        use toolpath_convo::Item;
        // Nothing between the System anchor and the boundary projects; the
        // anchor patches forward to the first post-boundary message (still
        // a real projected message at-or-after the anchor).
        let mut sys = user_turn("branch context");
        sys.id = "sys1".into();
        sys.role = Role::System;
        let boundary = Compaction {
            id: "c".into(),
            parent_id: Some("sys1".into()),
            timestamp: "2026-04-21T12:00:01.000Z".into(),
            trigger: Some(CompactionTrigger::Auto),
            summary: None,
            pre_tokens: None,
            kept_from: Some("sys1".into()),
        };
        let mut u2 = user_turn("after");
        u2.id = "u2".into();
        u2.parent_id = Some("c".into());
        u2.timestamp = "2026-04-21T12:00:02.000Z".into();

        let mut view = view_with(vec![sys.clone()]);
        view.items.push(Item::Compaction(boundary.clone()));
        view.items.push(Item::Turn(u2));
        let s = OpencodeProjector::default().project(&view).unwrap();
        let u2_msg_id = s
            .messages
            .iter()
            .find(|m| m.parts.iter().any(|p| matches!(p.data, PartData::Text(_))))
            .map(|m| m.id.clone())
            .expect("post-boundary user message");
        let cp = s
            .messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match &p.data {
                PartData::Compaction(cp) => Some(cp),
                _ => None,
            })
            .unwrap();
        assert_eq!(cp.tail_start_id.as_deref(), Some(u2_msg_id.as_str()));

        // With nothing at-or-after the anchor at all, `tailStartID` is
        // omitted rather than left dangling.
        let mut view = view_with(vec![sys]);
        view.items.push(Item::Compaction(boundary));
        let s = OpencodeProjector::default().project(&view).unwrap();
        let cp = s
            .messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match &p.data {
                PartData::Compaction(cp) => Some(cp),
                _ => None,
            })
            .unwrap();
        assert_eq!(cp.tail_start_id, None);
    }

    #[test]
    fn user_turn_becomes_user_message_with_text_part() {
        let s = OpencodeProjector::default()
            .project(&view_with(vec![user_turn("hello")]))
            .unwrap();
        assert_eq!(s.messages.len(), 1);
        let m = &s.messages[0];
        assert!(m.id.starts_with("msg_"));
        assert!(matches!(m.data, MessageData::User(_)));
        assert_eq!(m.parts.len(), 1);
        match &m.parts[0].data {
            PartData::Text(t) => assert_eq!(t.text, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn assistant_turn_emits_step_start_text_step_finish() {
        let s = OpencodeProjector::default()
            .project(&view_with(vec![assistant_turn("done")]))
            .unwrap();
        assert_eq!(s.messages.len(), 1);
        let kinds: Vec<&str> = s.messages[0].parts.iter().map(|p| p.data.kind()).collect();
        assert_eq!(kinds, vec!["step-start", "text", "step-finish"]);
    }

    #[test]
    fn tool_call_lands_as_tool_part() {
        let mut t = assistant_turn("");
        t.tool_uses = vec![ToolInvocation {
            id: "call_x".into(),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            result: Some(ToolResult {
                content: "out\n".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::Shell),
        }];
        let s = OpencodeProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let parts = &s.messages[0].parts;
        let tool_part = parts
            .iter()
            .find(|p| matches!(p.data, PartData::Tool(_)))
            .expect("tool part");
        match &tool_part.data {
            PartData::Tool(tp) => {
                assert_eq!(tp.tool, "bash");
                assert_eq!(tp.call_id, "call_x");
                match &tp.state {
                    ToolState::Completed(c) => {
                        assert_eq!(c.output, "out\n");
                        assert_eq!(c.input["command"], "ls");
                        assert_eq!(c.metadata["exit"], 0);
                    }
                    _ => panic!("expected completed state"),
                }
            }
            _ => panic!("expected tool part"),
        }
    }

    #[test]
    fn errored_tool_use_produces_error_state() {
        let mut t = assistant_turn("");
        t.tool_uses = vec![ToolInvocation {
            id: "c".into(),
            name: "bash".into(),
            input: json!({"command": "false"}),
            result: Some(ToolResult {
                content: "exit 1".into(),
                is_error: true,
            }),
            category: Some(ToolCategory::Shell),
        }];
        let s = OpencodeProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let tp = s.messages[0]
            .parts
            .iter()
            .find_map(|p| match &p.data {
                PartData::Tool(tp) => Some(tp),
                _ => None,
            })
            .unwrap();
        assert!(matches!(tp.state, ToolState::Error(_)));
    }

    #[test]
    fn foreign_tool_name_remaps_via_category() {
        let mut t = assistant_turn("");
        t.tool_uses = vec![ToolInvocation {
            id: "c".into(),
            name: "Edit".into(),
            input: json!({"file_path": "x.rs", "old_string": "a", "new_string": "b"}),
            result: None,
            category: Some(ToolCategory::FileWrite),
        }];
        let s = OpencodeProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let tp = s.messages[0]
            .parts
            .iter()
            .find_map(|p| match &p.data {
                PartData::Tool(tp) => Some(tp),
                _ => None,
            })
            .unwrap();
        assert_eq!(tp.tool, "edit");
    }

    #[test]
    fn assistant_thinking_emits_reasoning_part() {
        let mut t = assistant_turn("ok");
        t.thinking = Some("considering options".into());
        let s = OpencodeProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let kinds: Vec<&str> = s.messages[0].parts.iter().map(|p| p.data.kind()).collect();
        assert_eq!(
            kinds,
            vec!["step-start", "reasoning", "text", "step-finish"]
        );
    }

    #[test]
    fn assistant_parent_id_chains_to_prior_user_message() {
        let s = OpencodeProjector::default()
            .project(&view_with(vec![user_turn("hi"), assistant_turn("ok")]))
            .unwrap();
        let user_id = s.messages[0].id.clone();
        match &s.messages[1].data {
            MessageData::Assistant(a) => assert_eq!(a.parent_id, user_id),
            _ => panic!("expected assistant"),
        }
    }

    fn user_msg(id: &str, t: i64) -> Message {
        Message {
            id: id.into(),
            session_id: "s".into(),
            time_created: t,
            time_updated: t,
            data: MessageData::User(UserMessage {
                time: MessageTime {
                    created: t,
                    completed: None,
                },
                agent: "build".into(),
                model: ModelRef {
                    provider_id: "anthropic".into(),
                    model_id: "m".into(),
                    variant: None,
                },
                format: None,
                summary: None,
                system: None,
                tools: None,
                extra: HashMap::new(),
            }),
            parts: vec![],
        }
    }

    fn assistant_msg(id: &str, t: i64) -> Message {
        Message {
            id: id.into(),
            session_id: "s".into(),
            time_created: t,
            time_updated: t,
            data: MessageData::Assistant(AssistantMessage {
                parent_id: String::new(),
                time: MessageTime {
                    created: t,
                    completed: Some(t),
                },
                error: None,
                agent: "build".into(),
                mode: Some("build".into()),
                model_id: "m".into(),
                provider_id: "anthropic".into(),
                path: MessagePath {
                    cwd: "/p".into(),
                    root: "/p".into(),
                },
                summary: None,
                cost: 0.0,
                tokens: Tokens::default(),
                structured: None,
                variant: None,
                finish: None,
                extra: HashMap::new(),
            }),
            parts: vec![],
        }
    }

    #[test]
    fn monotonize_times_bumps_ties_and_regressions() {
        let mut msgs = vec![
            user_msg("a", 100),
            user_msg("b", 100),
            assistant_msg("c", 50),
        ];
        monotonize_times(&mut msgs);
        let times: Vec<i64> = msgs.iter().map(|m| m.time_created).collect();
        assert_eq!(times, vec![100, 101, 102]);
        match &msgs[1].data {
            MessageData::User(u) => assert_eq!(u.time.created, 101),
            _ => panic!("expected user"),
        }
        match &msgs[2].data {
            MessageData::Assistant(a) => {
                assert_eq!(a.time.created, 102);
                assert_eq!(a.time.completed, Some(102));
            }
            _ => panic!("expected assistant"),
        }
        assert!(msgs.iter().all(|m| m.time_updated >= m.time_created));
    }

    #[test]
    fn monotonize_times_keeps_reader_order_stable() {
        // The reader orders rows by (time_created, id); ids are arranged
        // so an unbumped tie would reorder relative to emission order.
        let mut msgs = vec![user_msg("z", 100), user_msg("a", 100), user_msg("m", 100)];
        monotonize_times(&mut msgs);
        let emitted: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
        let mut reread = msgs.clone();
        reread
            .sort_by(|x, y| (x.time_created, x.id.as_str()).cmp(&(y.time_created, y.id.as_str())));
        let reread_ids: Vec<String> = reread.iter().map(|m| m.id.clone()).collect();
        assert_eq!(reread_ids, emitted);
    }

    #[test]
    fn monotonize_times_leaves_increasing_sequence_untouched() {
        let mut msgs = vec![user_msg("a", 1), user_msg("b", 2)];
        monotonize_times(&mut msgs);
        assert_eq!(msgs[0].time_created, 1);
        assert_eq!(msgs[1].time_created, 2);
    }
}
