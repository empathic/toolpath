//! ConversationProvider bridge: map Pi sessions to `toolpath_convo::ConversationView`.
//!
//! Walks `PiSession.entries` in file order. Each `Entry::Message` becomes a
//! `Turn`; metadata-only entries like `ModelChange` / `ThinkingLevelChange` /
//! `Label` buffer and attach to the next message's `extra["pi"]`. `Compaction`,
//! `BranchSummary`, `Custom`, and `CustomMessage` emit synthetic turns with
//! appropriate roles.
//!
//! Tool-result correlation is a two-pass process: we record tool-call ids as
//! assistant turns are built, then in a second pass populate matching tool
//! invocations' `.result` fields (and any sibling `DelegatedWork.result`).

use crate::PiConvo;
use crate::error::PiError;
use crate::reader::PiSession;
use crate::types::{
    AgentMessage, ContentBlock, Entry, MessageContent, StopReason, ToolResultContent, Usage,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError, DelegatedWork,
    EnvironmentSnapshot, Role, SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult,
    Turn,
};

// ── Classification helpers ───────────────────────────────────────────

/// Classify a Pi tool name into toolpath's category ontology.
///
/// Pi tool names are case-insensitive in the wild; we lowercase before
/// matching and let names containing `task` / `agent` collapse to
/// [`ToolCategory::Delegation`]. Unknown names return `None`.
pub fn classify_tool(name: &str) -> Option<ToolCategory> {
    let lower = name.to_lowercase();
    if lower.contains("task") || lower.contains("agent") {
        return Some(ToolCategory::Delegation);
    }
    match lower.as_str() {
        "read" => Some(ToolCategory::FileRead),
        "write" | "edit" => Some(ToolCategory::FileWrite),
        "bash" | "shell" | "run" | "exec" => Some(ToolCategory::Shell),
        "grep" | "glob" | "find" | "ls" => Some(ToolCategory::FileSearch),
        "webfetch" | "websearch" | "fetch" => Some(ToolCategory::Network),
        _ => None,
    }
}

/// Reverse of [`classify_tool`]: pick Pi's preferred native tool name
/// for a generic [`ToolCategory`], disambiguating by call args when
/// multiple Pi tools share the same category.
///
/// Used by [`crate::project::PiProjector`] when projecting tool calls
/// from foreign harnesses (Claude, Gemini, etc.) — we know the
/// category from the source's classifier and need to pick a Pi name
/// whose arg shape matches the call's actual args. Returns `None` for
/// categories with no obvious Pi analog.
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("bash"),
        ToolCategory::FileRead => Some("read"),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep"
        } else {
            "glob"
        }),
        // Edit-shape calls carry old_string/new_string (or `edits[]` for
        // multi-edit); whole-file writes carry `content`. Pi has both
        // `write` and `edit`; pick `edit` for in-place mutations.
        ToolCategory::FileWrite => Some(
            if args.get("old_string").is_some() || args.get("edits").is_some() {
                "edit"
            } else {
                "write"
            },
        ),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "webfetch"
        } else {
            "websearch"
        }),
        ToolCategory::Delegation => Some("task"),
    }
}

fn extract_prompt(args: &Value) -> String {
    for key in ["prompt", "input", "instructions"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return s.to_string();
        }
    }
    args.to_string()
}

fn extract_file_path(args: &Value) -> Option<String> {
    for key in ["file_path", "path", "filename", "file"] {
        if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn stop_reason_to_string(sr: &StopReason) -> String {
    match serde_json::to_value(sr).ok().and_then(|v| match v {
        Value::String(s) => Some(s),
        _ => None,
    }) {
        Some(s) => s,
        None => format!("{:?}", sr).to_lowercase(),
    }
}

fn extract_user_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            texts.join("\n")
        }
    }
}

fn extract_assistant_text(blocks: &[ContentBlock]) -> String {
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    texts.join("\n")
}

fn extract_assistant_thinking(blocks: &[ContentBlock]) -> Option<String> {
    let thinking: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
            _ => None,
        })
        .collect();
    if thinking.is_empty() {
        None
    } else {
        Some(thinking.join("\n"))
    }
}

fn extract_tool_result_text(content: &[ToolResultContent]) -> String {
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    texts.join("\n")
}

/// Pi's wire requires a `usage` object on every assistant message, so
/// foreign-source projections fill it with zeros when the spend is
/// unknown. A real API message can never cost zero tokens, so an
/// all-zero `usage` decodes as "no usage recorded", not `Some(zeros)`.
fn usage_to_token_usage(usage: &Usage) -> Option<TokenUsage> {
    if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
        return None;
    }
    Some(TokenUsage {
        // Same absence rule as the cache fields: the wire can't express
        // "unknown", so a zero written by a foreign-source projection decodes
        // back to `None` (a real API message never has zero input tokens).
        // Output stays as-is — it's the field pi genuinely reports.
        input_tokens: if usage.input > 0 {
            Some(usage.input as u32)
        } else {
            None
        },
        output_tokens: Some(usage.output as u32),
        cache_read_tokens: if usage.cache_read > 0 {
            Some(usage.cache_read as u32)
        } else {
            None
        },
        cache_write_tokens: if usage.cache_write > 0 {
            Some(usage.cache_write as u32)
        } else {
            None
        },
        ..Default::default()
    })
}

fn environment_for(session: &PiSession) -> EnvironmentSnapshot {
    EnvironmentSnapshot {
        working_dir: Some(session.header.cwd.clone()),
        vcs_branch: None,
        vcs_revision: None,
    }
}

fn truncate_output(output: &str, max: usize) -> String {
    if output.chars().count() <= max {
        output.to_string()
    } else {
        let truncated: String = output.chars().take(max).collect();
        format!("{}…(truncated)", truncated)
    }
}

// ── Main conversion ──────────────────────────────────────────────────

/// Convert a PiSession into a provider-agnostic ConversationView.
pub fn session_to_view(session: &PiSession) -> ConversationView {
    let env = environment_for(session);

    // Two-pass strategy:
    //  Pass 1: walk entries, emit turns. Track tool-call invocation locations
    //          (turn_idx, tool_idx) by id for later correlation.
    //  Pass 2: walk turns again for tool-result roles; find the matching
    //          invocation by id and populate `.result` (and any delegation
    //          result).
    let mut turns: Vec<Turn> = Vec::new();
    // Map tool-call id → (turn_idx, tool_idx).
    let mut tool_call_locs: HashMap<String, (usize, usize)> = HashMap::new();
    // Map tool-call id → delegation index within the turn (if any).
    let mut delegation_locs: HashMap<String, (usize, usize)> = HashMap::new();
    // Per-turn tool-result info: (tool_call_id, content, is_error).
    let mut tool_result_payloads: Vec<(usize, String, String, bool)> = Vec::new();

    for entry in &session.entries {
        match entry {
            Entry::Session(_) => continue,

            Entry::ModelChange { .. } | Entry::ThinkingLevelChange { .. } | Entry::Label { .. } => {
                // Discarded — these influence rendering only and don't map onto
                // a cross-harness IR field.
            }

            Entry::Compaction { base, summary, .. } => {
                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id: None,
                    role: Role::System,
                    timestamp: base.timestamp.clone(),
                    text: format!("Compacted (summary): {}", summary),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations: vec![],
                    file_mutations: Vec::new(),
                });
            }

            Entry::BranchSummary { base, summary, .. } => {
                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id: None,
                    role: Role::System,
                    timestamp: base.timestamp.clone(),
                    text: format!("Branch summary: {}", summary),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations: vec![],
                    file_mutations: Vec::new(),
                });
            }

            Entry::Custom { base, .. } => {
                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id: None,
                    role: Role::Other("custom".to_string()),
                    timestamp: base.timestamp.clone(),
                    text: String::new(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations: vec![],
                    file_mutations: Vec::new(),
                });
            }

            Entry::CustomMessage {
                base,
                custom_type,
                content,
                ..
            } => {
                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id: None,
                    role: Role::Other(format!("custom:{}", custom_type)),
                    timestamp: base.timestamp.clone(),
                    text: extract_user_text(content),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations: vec![],
                    file_mutations: Vec::new(),
                });
            }

            Entry::Message { base, message, .. } => {
                let text;
                let mut thinking = None;
                let mut tool_uses: Vec<ToolInvocation> = Vec::new();
                let mut model: Option<String> = None;
                let mut stop_reason_s: Option<String> = None;
                let mut token_usage: Option<TokenUsage> = None;
                let mut delegations: Vec<DelegatedWork> = Vec::new();
                let role: Role;

                match message {
                    AgentMessage::User { content, .. } => {
                        role = Role::User;
                        text = extract_user_text(content);
                    }

                    AgentMessage::Assistant {
                        content,
                        model: m,
                        usage,
                        stop_reason,
                        ..
                    } => {
                        role = Role::Assistant;
                        text = extract_assistant_text(content);
                        thinking = extract_assistant_thinking(content);
                        model = Some(m.clone());
                        stop_reason_s = Some(stop_reason_to_string(stop_reason));
                        token_usage = usage_to_token_usage(usage);

                        let turn_idx = turns.len();
                        for block in content {
                            if let ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                                ..
                            } = block
                            {
                                let category = classify_tool(name);
                                let tool_idx = tool_uses.len();
                                tool_call_locs.insert(id.clone(), (turn_idx, tool_idx));
                                if category == Some(ToolCategory::Delegation) {
                                    let deleg_idx = delegations.len();
                                    delegations.push(DelegatedWork {
                                        agent_id: id.clone(),
                                        prompt: extract_prompt(arguments),
                                        turns: vec![],
                                        result: None,
                                    });
                                    delegation_locs.insert(id.clone(), (turn_idx, deleg_idx));
                                }
                                tool_uses.push(ToolInvocation {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: arguments.clone(),
                                    result: None,
                                    category,
                                });
                            }
                        }
                    }

                    AgentMessage::ToolResult {
                        tool_call_id,
                        content,
                        is_error,
                        ..
                    } => {
                        // Tool results fold onto the matching assistant
                        // turn's `tool_uses[i].result` via pass 2. We don't
                        // emit them as standalone turns — that mirrors how
                        // claude/gemini/codex/opencode derive treats tool
                        // results, and keeps Pi → Pi idempotent without
                        // smuggling tool_call_id through Turn.extra.
                        tool_result_payloads.push((
                            usize::MAX,
                            tool_call_id.clone(),
                            extract_tool_result_text(content),
                            *is_error,
                        ));
                        continue;
                    }

                    AgentMessage::BashExecution {
                        command,
                        output,
                        exit_code,
                        ..
                    } => {
                        role = Role::Other("bash".to_string());
                        let out_trunc = truncate_output(output, 4096);
                        text = format!("$ {}\n{}", command, out_trunc);
                        // Synthetic ToolInvocation representing the bash run itself.
                        tool_uses.push(ToolInvocation {
                            id: base.id.clone(),
                            name: "bash".to_string(),
                            input: json!({ "command": command }),
                            result: Some(ToolResult {
                                content: output.clone(),
                                is_error: !matches!(exit_code, Some(0)),
                            }),
                            category: Some(ToolCategory::Shell),
                        });
                    }

                    AgentMessage::Custom {
                        custom_type,
                        content,
                        ..
                    } => {
                        role = Role::Other(format!("custom:{}", custom_type));
                        text = extract_user_text(content);
                    }

                    AgentMessage::BranchSummary { .. } | AgentMessage::CompactionSummary { .. } => {
                        role = Role::System;
                        text = String::new();
                    }
                }

                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id: None,
                    role,
                    timestamp: base.timestamp.clone(),
                    text,
                    thinking,
                    tool_uses,
                    model,
                    stop_reason: stop_reason_s,
                    token_usage,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations,
                    file_mutations: Vec::new(),
                });
            }
        }
    }

    // Pass 2: tool-result correlation.
    for (_tr_turn_idx, tool_call_id, content, is_error) in &tool_result_payloads {
        if let Some((turn_idx, tool_idx)) = tool_call_locs.get(tool_call_id)
            && let Some(turn) = turns.get_mut(*turn_idx)
            && let Some(inv) = turn.tool_uses.get_mut(*tool_idx)
        {
            inv.result = Some(ToolResult {
                content: content.clone(),
                is_error: *is_error,
            });
        }
        if let Some((turn_idx, deleg_idx)) = delegation_locs.get(tool_call_id)
            && let Some(turn) = turns.get_mut(*turn_idx)
            && let Some(d) = turn.delegations.get_mut(*deleg_idx)
        {
            d.result = Some(content.clone());
        }
    }

    // Aggregate token usage from Assistant turns.
    let mut have_any_usage = false;
    let mut total = TokenUsage::default();
    for turn in &turns {
        if let Some(u) = &turn.token_usage {
            have_any_usage = true;
            total.input_tokens =
                Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
            total.output_tokens =
                Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
            if let Some(r) = u.cache_read_tokens {
                total.cache_read_tokens = Some(total.cache_read_tokens.unwrap_or(0) + r);
            }
            if let Some(w) = u.cache_write_tokens {
                total.cache_write_tokens = Some(total.cache_write_tokens.unwrap_or(0) + w);
            }
        }
    }
    let total_usage = if have_any_usage { Some(total) } else { None };

    // files_changed: dedup-in-order from FileWrite tool inputs.
    let mut files_changed: Vec<String> = Vec::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for turn in &turns {
        for inv in &turn.tool_uses {
            if inv.category == Some(ToolCategory::FileWrite)
                && let Some(p) = extract_file_path(&inv.input)
                && seen_files.insert(p.clone())
            {
                files_changed.push(p);
            }
        }
    }

    // session_ids: walk parent chain, oldest first.
    let mut session_ids: Vec<String> = Vec::new();
    fn walk_parents(s: &PiSession, out: &mut Vec<String>) {
        if let Some(p) = &s.parent {
            walk_parents(p, out);
        }
        out.push(s.header.id.clone());
    }
    walk_parents(session, &mut session_ids);

    let started_at = parse_ts(&session.header.timestamp);
    let last_activity = turns.last().and_then(|t| parse_ts(&t.timestamp));

    let base = if session.header.cwd.is_empty() {
        None
    } else {
        Some(SessionBase {
            working_dir: Some(session.header.cwd.clone()),
            ..Default::default()
        })
    };

    ConversationView {
        id: session.header.id.clone(),
        started_at,
        last_activity,
        turns,
        total_usage,
        provider_id: Some("pi".to_string()),
        files_changed,
        session_ids,
        events: vec![],
        base,
        ..Default::default()
    }
}

// ── ConversationProvider impl for PiConvo ────────────────────────────

fn to_convo_err(e: PiError) -> ConvoError {
    ConvoError::Provider(e.to_string())
}

impl ConversationProvider for PiConvo {
    fn list_conversations(&self, project: &str) -> Result<Vec<String>, ConvoError> {
        let metas = self.list_sessions(project).map_err(to_convo_err)?;
        Ok(metas.into_iter().map(|m| m.id).collect())
    }

    fn load_conversation(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> Result<ConversationView, ConvoError> {
        let session = self
            .read_session(project, conversation_id)
            .map_err(to_convo_err)?;
        Ok(session_to_view(&session))
    }

    fn load_metadata(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> Result<ConversationMeta, ConvoError> {
        let metas = self.list_sessions(project).map_err(to_convo_err)?;
        let meta = metas
            .into_iter()
            .find(|m| m.id == conversation_id)
            .ok_or_else(|| {
                ConvoError::Provider(format!("session not found: {}", conversation_id))
            })?;
        Ok(meta_to_conversation_meta(meta))
    }

    fn list_metadata(&self, project: &str) -> Result<Vec<ConversationMeta>, ConvoError> {
        let metas = self.list_sessions(project).map_err(to_convo_err)?;
        Ok(metas.into_iter().map(meta_to_conversation_meta).collect())
    }
}

fn meta_to_conversation_meta(meta: crate::reader::SessionMeta) -> ConversationMeta {
    let ts = parse_ts(&meta.timestamp);
    ConversationMeta {
        id: meta.id,
        started_at: ts,
        // Without reading the full file we can't distinguish; use header ts.
        last_activity: ts,
        // `entry_count` counts all non-header entries (including non-message
        // metadata). Treat it as an approximation of message_count.
        message_count: meta.entry_count,
        file_path: Some(meta.file_path),
        predecessor: None,
        successor: None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::PathResolver;
    use crate::reader::PiSession;
    use crate::types::{
        AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, KnownStopReason,
        MessageContent, SessionHeader, StopReason, ToolResultContent, Usage,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_all_zero_usage_decodes_as_none() {
        // Pi's wire requires `usage`; foreign projections zero-fill it
        // when spend is unknown. Zero is not a real spend.
        let zero = Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: CostBreakdown::default(),
        };
        assert!(usage_to_token_usage(&zero).is_none());

        let real = Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 15,
            cost: CostBreakdown::default(),
        };
        let u = usage_to_token_usage(&real).unwrap();
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(5));
        assert_eq!(u.cache_read_tokens, None);
    }

    fn header(id: &str, cwd: &str) -> SessionHeader {
        SessionHeader {
            version: 3,
            id: id.into(),
            timestamp: "2026-04-16T00:00:00Z".into(),
            cwd: cwd.into(),
            parent_session: None,
            extra: HashMap::new(),
        }
    }

    fn base(id: &str, parent: Option<&str>, ts: &str) -> EntryBase {
        EntryBase {
            id: id.into(),
            parent_id: parent.map(String::from),
            timestamp: ts.into(),
        }
    }

    fn user_text_entry(id: &str, parent: Option<&str>, text: &str) -> Entry {
        Entry::Message {
            base: base(id, parent, "2026-04-16T00:00:01Z"),
            message: AgentMessage::User {
                content: MessageContent::Text(text.into()),
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        }
    }

    fn assistant_entry(
        id: &str,
        parent: Option<&str>,
        content: Vec<ContentBlock>,
        usage: Usage,
        stop_reason: StopReason,
        model: &str,
    ) -> Entry {
        Entry::Message {
            base: base(id, parent, "2026-04-16T00:00:02Z"),
            message: AgentMessage::Assistant {
                content,
                api: "anthropic".into(),
                provider: "anthropic".into(),
                model: model.into(),
                usage,
                stop_reason,
                error_message: None,
                timestamp: 2,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        }
    }

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            total_tokens: input + output,
            cost: CostBreakdown::default(),
        }
    }

    fn session_from(entries: Vec<Entry>, cwd: &str) -> PiSession {
        let h = header("sess-1", cwd);
        let mut all = vec![Entry::Session(h.clone())];
        all.extend(entries);
        PiSession {
            header: h,
            entries: all,
            file_path: PathBuf::from("/tmp/fake.jsonl"),
            parent: None,
        }
    }

    #[test]
    fn test_empty_session_produces_view() {
        let session = session_from(vec![], "/tmp/p");
        let v = session_to_view(&session);
        assert_eq!(v.turns.len(), 0);
        assert_eq!(v.provider_id.as_deref(), Some("pi"));
        assert_eq!(v.id, "sess-1");
    }

    #[test]
    fn test_user_message_becomes_user_turn() {
        let session = session_from(vec![user_text_entry("a", None, "hello")], "/tmp/p");
        let v = session_to_view(&session);
        assert_eq!(v.turns.len(), 1);
        assert_eq!(v.turns[0].role, Role::User);
        assert_eq!(v.turns[0].text, "hello");
    }

    #[test]
    fn test_user_message_with_blocks_extracts_text() {
        let entry = Entry::Message {
            base: base("a", None, "t"),
            message: AgentMessage::User {
                content: MessageContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "first".into(),
                        extra: HashMap::new(),
                    },
                    ContentBlock::Image {
                        data: "xx".into(),
                        mime_type: "image/png".into(),
                        extra: HashMap::new(),
                    },
                    ContentBlock::Text {
                        text: "second".into(),
                        extra: HashMap::new(),
                    },
                ]),
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        let session = session_from(vec![entry], "/tmp/p");
        let v = session_to_view(&session);
        assert_eq!(v.turns[0].text, "first\nsecond");
    }

    #[test]
    fn test_assistant_message_becomes_assistant_turn() {
        let entry = assistant_entry(
            "a",
            None,
            vec![ContentBlock::Text {
                text: "ok".into(),
                extra: HashMap::new(),
            }],
            usage(10, 20),
            StopReason::Known(KnownStopReason::Stop),
            "claude-opus",
        );
        let v = session_to_view(&session_from(vec![entry], "/tmp/p"));
        assert_eq!(v.turns[0].role, Role::Assistant);
        assert_eq!(v.turns[0].model.as_deref(), Some("claude-opus"));
        assert_eq!(v.turns[0].stop_reason.as_deref(), Some("stop"));
        let u = v.turns[0].token_usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(20));
    }

    #[test]
    fn test_assistant_text_and_thinking_separated() {
        let entry = assistant_entry(
            "a",
            None,
            vec![
                ContentBlock::Text {
                    text: "one".into(),
                    extra: HashMap::new(),
                },
                ContentBlock::Thinking {
                    thinking: "mmm".into(),
                    extra: HashMap::new(),
                },
                ContentBlock::Text {
                    text: "two".into(),
                    extra: HashMap::new(),
                },
            ],
            usage(1, 2),
            StopReason::Known(KnownStopReason::Stop),
            "m",
        );
        let v = session_to_view(&session_from(vec![entry], "/tmp/p"));
        assert_eq!(v.turns[0].text, "one\ntwo");
        assert_eq!(v.turns[0].thinking.as_deref(), Some("mmm"));
    }

    #[test]
    fn test_assistant_tool_call_becomes_tool_invocation() {
        let entry = assistant_entry(
            "a",
            None,
            vec![ContentBlock::ToolCall {
                id: "tc1".into(),
                name: "Read".into(),
                arguments: json!({"path": "/x"}),
                extra: HashMap::new(),
            }],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let v = session_to_view(&session_from(vec![entry], "/tmp/p"));
        assert_eq!(v.turns[0].tool_uses.len(), 1);
        let inv = &v.turns[0].tool_uses[0];
        assert_eq!(inv.id, "tc1");
        assert_eq!(inv.name, "Read");
        assert_eq!(inv.category, Some(ToolCategory::FileRead));
    }

    #[test]
    fn test_tool_classification() {
        assert_eq!(classify_tool("read"), Some(ToolCategory::FileRead));
        assert_eq!(classify_tool("write"), Some(ToolCategory::FileWrite));
        assert_eq!(classify_tool("bash"), Some(ToolCategory::Shell));
        assert_eq!(classify_tool("grep"), Some(ToolCategory::FileSearch));
        assert_eq!(classify_tool("webfetch"), Some(ToolCategory::Network));
        assert_eq!(classify_tool("Task"), Some(ToolCategory::Delegation));
        assert_eq!(
            classify_tool("some-agent-run"),
            Some(ToolCategory::Delegation)
        );
        assert_eq!(classify_tool("obscure"), None);
    }

    #[test]
    fn test_tool_result_correlates_back_to_invocation() {
        let assistant = assistant_entry(
            "a1",
            None,
            vec![ContentBlock::ToolCall {
                id: "t1".into(),
                name: "read".into(),
                arguments: json!({}),
                extra: HashMap::new(),
            }],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let tr = Entry::Message {
            base: base("a2", Some("a1"), "t"),
            message: AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                tool_name: "read".into(),
                content: vec![ToolResultContent::Text {
                    text: "result".into(),
                    extra: HashMap::new(),
                }],
                details: None,
                is_error: false,
                timestamp: 3,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![assistant, tr], "/tmp/p"));
        let inv = &v.turns[0].tool_uses[0];
        let res = inv.result.as_ref().unwrap();
        assert_eq!(res.content, "result");
        assert!(!res.is_error);
    }

    #[test]
    fn test_orphan_tool_result_is_dropped() {
        // A ToolResult entry without a matching assistant turn folds
        // into nothing — the IR doesn't model standalone tool turns.
        let tr = Entry::Message {
            base: base("a", None, "t"),
            message: AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                tool_name: "x".into(),
                content: vec![ToolResultContent::Text {
                    text: "r".into(),
                    extra: HashMap::new(),
                }],
                details: None,
                is_error: false,
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![tr], "/tmp/p"));
        assert_eq!(v.turns.len(), 0);
    }

    #[test]
    fn test_bash_execution_turn() {
        let e = Entry::Message {
            base: base("a", None, "t"),
            message: AgentMessage::BashExecution {
                command: "ls".into(),
                output: "a\nb".into(),
                exit_code: Some(0),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                exclude_from_context: None,
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![e], "/tmp/p"));
        assert_eq!(v.turns[0].role, Role::Other("bash".to_string()));
        assert!(v.turns[0].text.starts_with("$ ls"));
        assert_eq!(v.turns[0].tool_uses.len(), 1);
        assert_eq!(v.turns[0].tool_uses[0].category, Some(ToolCategory::Shell));
    }

    #[test]
    fn test_parent_id_preserved() {
        let v = session_to_view(&session_from(
            vec![
                user_text_entry("a", None, "x"),
                user_text_entry("b", Some("a"), "y"),
            ],
            "/tmp/p",
        ));
        assert_eq!(v.turns[1].parent_id.as_deref(), Some("a"));
    }

    #[test]
    fn test_compaction_produces_system_turn() {
        let c = Entry::Compaction {
            base: base("c", None, "t"),
            summary: "sum".into(),
            first_kept_entry_id: "x".into(),
            tokens_before: 100,
            details: None,
            from_hook: Some(false),
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![c], "/tmp/p"));
        assert_eq!(v.turns[0].role, Role::System);
        assert!(v.turns[0].text.starts_with("Compacted"));
    }

    #[test]
    fn test_branch_summary_produces_system_turn() {
        let bs = Entry::BranchSummary {
            base: base("bs", None, "t"),
            from_id: "fromX".into(),
            summary: "branched".into(),
            details: None,
            from_hook: None,
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![bs], "/tmp/p"));
        assert_eq!(v.turns[0].role, Role::System);
        assert!(v.turns[0].text.starts_with("Branch summary"));
    }

    #[test]
    fn test_model_change_drops_silently() {
        let mc = Entry::ModelChange {
            base: base("mc", None, "t"),
            provider: "anthropic".into(),
            model_id: "claude-opus".into(),
            extra: HashMap::new(),
        };
        let msg = user_text_entry("u", None, "hi");
        let v = session_to_view(&session_from(vec![mc, msg], "/tmp/p"));
        assert_eq!(v.turns.len(), 1);
    }

    #[test]
    fn test_environment_populated_on_every_turn() {
        let v = session_to_view(&session_from(
            vec![
                user_text_entry("a", None, "x"),
                user_text_entry("b", Some("a"), "y"),
            ],
            "/Users/alex/p",
        ));
        for t in &v.turns {
            assert_eq!(
                t.environment.as_ref().unwrap().working_dir.as_deref(),
                Some("/Users/alex/p")
            );
        }
    }

    #[test]
    fn test_total_usage_aggregates_assistant_turns() {
        let a1 = assistant_entry(
            "a1",
            None,
            vec![],
            usage(10, 20),
            StopReason::Known(KnownStopReason::Stop),
            "m",
        );
        let a2 = assistant_entry(
            "a2",
            Some("a1"),
            vec![],
            usage(10, 20),
            StopReason::Known(KnownStopReason::Stop),
            "m",
        );
        let v = session_to_view(&session_from(vec![a1, a2], "/tmp/p"));
        let tu = v.total_usage.unwrap();
        assert_eq!(tu.input_tokens, Some(20));
        assert_eq!(tu.output_tokens, Some(40));
    }

    #[test]
    fn test_files_changed_extracted_from_filewrite_tools() {
        let a = assistant_entry(
            "a",
            None,
            vec![
                ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "write".into(),
                    arguments: json!({"path": "a.rs"}),
                    extra: HashMap::new(),
                },
                ContentBlock::ToolCall {
                    id: "t2".into(),
                    name: "edit".into(),
                    arguments: json!({"file_path": "b.rs"}),
                    extra: HashMap::new(),
                },
            ],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let v = session_to_view(&session_from(vec![a], "/tmp/p"));
        assert_eq!(v.files_changed, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn test_files_changed_deduplicated() {
        let a = assistant_entry(
            "a",
            None,
            vec![
                ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "write".into(),
                    arguments: json!({"path": "a.rs"}),
                    extra: HashMap::new(),
                },
                ContentBlock::ToolCall {
                    id: "t2".into(),
                    name: "write".into(),
                    arguments: json!({"path": "a.rs"}),
                    extra: HashMap::new(),
                },
            ],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let v = session_to_view(&session_from(vec![a], "/tmp/p"));
        assert_eq!(v.files_changed, vec!["a.rs"]);
    }

    #[test]
    fn test_session_ids_includes_self_when_no_parent() {
        let v = session_to_view(&session_from(vec![], "/tmp/p"));
        assert_eq!(v.session_ids, vec!["sess-1"]);
    }

    #[test]
    fn test_session_ids_chains_with_parent() {
        let parent_header = SessionHeader {
            version: 3,
            id: "parent".into(),
            timestamp: "2026-04-16T00:00:00Z".into(),
            cwd: "/tmp/p".into(),
            parent_session: None,
            extra: HashMap::new(),
        };
        let parent = PiSession {
            header: parent_header.clone(),
            entries: vec![Entry::Session(parent_header)],
            file_path: PathBuf::from("/tmp/p.jsonl"),
            parent: None,
        };
        let mut child = session_from(vec![], "/tmp/p");
        child.parent = Some(Box::new(parent));
        let v = session_to_view(&child);
        assert_eq!(v.session_ids, vec!["parent", "sess-1"]);
    }

    #[test]
    fn test_started_at_from_header_timestamp() {
        let session = session_from(vec![], "/tmp/p");
        let v = session_to_view(&session);
        assert!(v.started_at.is_some());

        let mut bad = session;
        bad.header.timestamp = "not-a-timestamp".into();
        let v = session_to_view(&bad);
        assert!(v.started_at.is_none());
    }

    // ── Provider tests ───────────────────────────────────────────────

    fn write_session_file(dir: &std::path::Path, id: &str, ts: &str) -> PathBuf {
        let path = dir.join(format!("{}.jsonl", id));
        let line = format!(
            r#"{{"type":"session","version":3,"id":"{id}","timestamp":"{ts}","cwd":"/tmp/p"}}
{{"type":"message","id":"u","parentId":null,"timestamp":"{ts}","message":{{"role":"user","content":"hi","timestamp":1}}}}"#,
            id = id,
            ts = ts
        );
        std::fs::write(&path, line).unwrap();
        path
    }

    #[test]
    fn test_provider_list_conversations_delegates_to_manager() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(&sessions);
        let proj = resolver.project_dir("/tmp/p");
        std::fs::create_dir_all(&proj).unwrap();
        write_session_file(&proj, "s1", "2026-04-16T00:00:00Z");

        let pi = PiConvo::with_resolver(resolver);
        let ids = ConversationProvider::list_conversations(&pi, "/tmp/p").unwrap();
        assert_eq!(ids, vec!["s1".to_string()]);
    }

    #[test]
    fn test_provider_load_conversation_returns_view() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(&sessions);
        let proj = resolver.project_dir("/tmp/p");
        std::fs::create_dir_all(&proj).unwrap();
        write_session_file(&proj, "s1", "2026-04-16T00:00:00Z");

        let pi = PiConvo::with_resolver(resolver);
        let v = ConversationProvider::load_conversation(&pi, "/tmp/p", "s1").unwrap();
        assert_eq!(v.id, "s1");
        assert_eq!(v.turns.len(), 1);
        assert_eq!(v.turns[0].role, Role::User);
    }

    #[test]
    fn test_provider_load_metadata_has_expected_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(&sessions);
        let proj = resolver.project_dir("/tmp/p");
        std::fs::create_dir_all(&proj).unwrap();
        let path = write_session_file(&proj, "s1", "2026-04-16T00:00:00Z");

        let pi = PiConvo::with_resolver(resolver);
        let m = ConversationProvider::load_metadata(&pi, "/tmp/p", "s1").unwrap();
        assert_eq!(m.id, "s1");
        assert!(m.started_at.is_some());
        assert_eq!(m.file_path.as_ref(), Some(&path));
    }

    #[test]
    fn test_provider_list_metadata_returns_all() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new("/tmp/fake-home").with_sessions_dir(&sessions);
        let proj = resolver.project_dir("/tmp/p");
        std::fs::create_dir_all(&proj).unwrap();
        write_session_file(&proj, "older", "2026-04-16T00:00:00Z");
        std::thread::sleep(std::time::Duration::from_millis(30));
        write_session_file(&proj, "newer", "2026-04-16T01:00:00Z");

        let pi = PiConvo::with_resolver(resolver);
        let all = ConversationProvider::list_metadata(&pi, "/tmp/p").unwrap();
        assert_eq!(all.len(), 2);
        // Newest first (via mtime)
        assert_eq!(all[0].id, "newer");
    }

    #[test]
    fn test_delegation_builds_delegated_work() {
        let a = assistant_entry(
            "a",
            None,
            vec![ContentBlock::ToolCall {
                id: "d1".into(),
                name: "Task".into(),
                arguments: json!({"prompt": "do the thing"}),
                extra: HashMap::new(),
            }],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let v = session_to_view(&session_from(vec![a], "/tmp/p"));
        assert_eq!(v.turns[0].delegations.len(), 1);
        assert_eq!(v.turns[0].delegations[0].prompt, "do the thing");
        assert_eq!(v.turns[0].delegations[0].agent_id, "d1");
    }

    #[test]
    fn test_stop_reason_string_form() {
        let a = assistant_entry(
            "a",
            None,
            vec![],
            usage(1, 1),
            StopReason::Known(KnownStopReason::ToolUse),
            "m",
        );
        let v = session_to_view(&session_from(vec![a], "/tmp/p"));
        let sr = v.turns[0].stop_reason.as_deref().unwrap();
        assert!(sr.to_lowercase().contains("tool"), "got: {}", sr);
    }

    #[test]
    fn test_custom_message_becomes_other_role_turn() {
        let cm = Entry::CustomMessage {
            base: base("cm", None, "t"),
            custom_type: "foo".into(),
            content: MessageContent::Text("body".into()),
            display: true,
            details: None,
            extra: HashMap::new(),
        };
        let v = session_to_view(&session_from(vec![cm], "/tmp/p"));
        assert_eq!(v.turns[0].role, Role::Other("custom:foo".to_string()));
        assert_eq!(v.turns[0].text, "body");
    }
}
