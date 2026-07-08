//! Forward derivation: [`OpenClawSession`] -> [`ConversationView`].
//!
//! Mirrors the two-pass structure used by the other providers: pass 1 emits a
//! [`Turn`] per entry and records tool-call locations; pass 2 folds each
//! `toolResult` entry onto its matching invocation. The tree (`id`/`parentId`)
//! is preserved on `Turn.parent_id` and turned into a step DAG by
//! [`toolpath_convo::derive_path`].

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use toolpath_convo::{
    ConversationMarker, ConversationMeta, ConversationProvider, ConversationView, ConvoError,
    DelegatedWork, EnvironmentSnapshot, FileMutation, Role, SessionBase, TokenUsage, ToolCategory,
    ToolInvocation, ToolResult, Turn,
};

use crate::OpenClawConvo;
use crate::paths::ParsedKey;
use crate::reader::OpenClawSession;
use crate::types::{AgentMessage, ContentBlock, Entry, StopReason, Usage};

/// Provider identifier written to `path.meta.source`.
pub const PROVIDER_ID: &str = "openclaw";

/// Classify an OpenClaw tool name into a cross-harness [`ToolCategory`].
///
/// Tool names are free-form; the matching is case-insensitive and lenient.
pub fn classify_tool(name: &str) -> Option<ToolCategory> {
    let lower = name.to_lowercase();
    // `sessions_spawn` is OpenClaw's sub-agent dispatch (observed in real
    // sessions); `spawn` alone is the discriminating token. Session
    // control-flow tools (`sessions_yield`, `sessions_send`, …) are not
    // delegation and fall through unclassified.
    if lower.contains("task")
        || lower.contains("agent")
        || lower.contains("subagent")
        || lower.contains("spawn")
    {
        return Some(ToolCategory::Delegation);
    }
    match lower.as_str() {
        "read" | "read_file" | "readfile" | "cat" => Some(ToolCategory::FileRead),
        "write" | "write_file" | "writefile" | "edit" | "edit_file" | "editfile" | "apply_patch"
        | "str_replace" => Some(ToolCategory::FileWrite),
        "bash" | "shell" | "run" | "exec" | "run_shell" | "run_command" => {
            Some(ToolCategory::Shell)
        }
        "grep" | "glob" | "find" | "ls" | "list_directory" | "search" => {
            Some(ToolCategory::FileSearch)
        }
        "webfetch" | "websearch" | "fetch" | "web_search" | "web_fetch" => {
            Some(ToolCategory::Network)
        }
        _ => None,
    }
}

/// Reverse of [`classify_tool`]: pick an OpenClaw-native tool name for a
/// generic [`ToolCategory`], disambiguating by call args. Used by the
/// projector when projecting tool calls from foreign harnesses. The names are
/// best-effort for OpenClaw's vocabulary.
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("bash"),
        ToolCategory::FileRead => Some("read_file"),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep"
        } else {
            "glob"
        }),
        ToolCategory::FileWrite => Some(
            if args.get("old").is_some()
                || args.get("old_string").is_some()
                || args.get("edits").is_some()
            {
                "edit_file"
            } else {
                "write_file"
            },
        ),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "web_fetch"
        } else {
            "web_search"
        }),
        ToolCategory::Delegation => Some("task"),
    }
}

fn extract_prompt(args: &Value) -> String {
    for key in ["prompt", "input", "instructions", "task"] {
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

/// `update` for in-place edits, `add` for whole-file writes.
fn write_operation(name: &str, args: &Value) -> String {
    let lower = name.to_lowercase();
    if lower.contains("edit")
        || lower.contains("patch")
        || lower.contains("replace")
        || args.get("old").is_some()
        || args.get("old_string").is_some()
        || args.get("edits").is_some()
    {
        "update".to_string()
    } else {
        "add".to_string()
    }
}

fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn stop_reason_to_string(sr: &StopReason) -> String {
    match serde_json::to_value(sr).ok() {
        Some(Value::String(s)) => s,
        _ => format!("{sr:?}").to_lowercase(),
    }
}

/// An all-zero `usage` decodes as "no usage recorded", not `Some(zeros)`.
fn usage_to_token_usage(usage: &Usage) -> Option<TokenUsage> {
    if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
        return None;
    }
    Some(TokenUsage {
        input_tokens: Some(usage.input as u32),
        output_tokens: Some(usage.output as u32),
        cache_read_tokens: (usage.cache_read > 0).then_some(usage.cache_read as u32),
        cache_write_tokens: (usage.cache_write > 0).then_some(usage.cache_write as u32),
        ..Default::default()
    })
}

/// Pull a string field out of a flattened forward-compat `extra` map.
fn extra_str(extra: &std::collections::HashMap<String, Value>, key: &str) -> Option<String> {
    extra.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// The signature riding on the FIRST content block of `block_type` — the
/// same collapsed granularity the IR uses for `text` / `thinking` bodies.
fn first_block_signature(
    content: &[ContentBlock],
    block_type: &str,
    sig_key: &str,
) -> Option<String> {
    content.iter().find_map(|b| {
        let extra = match (block_type, b) {
            ("text", ContentBlock::Text { extra, .. }) => extra,
            ("thinking", ContentBlock::Thinking { extra, .. }) => extra,
            _ => return None,
        };
        extra.get(sig_key).and_then(|v| v.as_str()).map(str::to_string)
    })
}

/// Decode a summary entry's `details` (`{ readFiles, modifiedFiles }`).
fn summary_detail_files(details: Option<&Value>) -> (Vec<String>, Vec<String>) {
    let list = |key: &str| -> Vec<String> {
        details
            .and_then(|d| d.get(key))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    (list("readFiles"), list("modifiedFiles"))
}

fn environment_for(session: &OpenClawSession) -> EnvironmentSnapshot {
    EnvironmentSnapshot {
        working_dir: Some(session.header.cwd.clone()),
        vcs_branch: None,
        vcs_revision: None,
    }
}

/// A channel-aware human actor string for this session, if its routing key is
/// known. DMs become `human:<channel>/<peerId>`; groups become
/// `human:<channel>/group/<peerId>`; a peer without a channel becomes
/// `human:<peerId>`. Returns `None` for main/CLI sessions (the caller then
/// falls back to the default `human:user`).
pub fn user_actor_for(parsed: Option<&ParsedKey>) -> Option<String> {
    let key = parsed?;
    // Sub-agent runs (`agent:<id>:subagent:<uuid>`, observed on disk after a
    // `sessions_spawn`) have no human peer; the "subagent" segment is a key
    // namespace, not a messaging channel.
    if key.channel.as_deref() == Some("subagent") {
        return None;
    }
    let peer = key.peer_id.as_deref()?;
    match (&key.channel, key.peer_kind.as_deref()) {
        (Some(ch), Some("group")) | (Some(ch), Some("channel")) => {
            Some(format!("human:{ch}/group/{peer}"))
        }
        (Some(ch), _) => Some(format!("human:{ch}/{peer}")),
        (None, _) => Some(format!("human:{peer}")),
    }
}

/// The session kind implied by a routing key.
fn session_kind(parsed: Option<&ParsedKey>) -> &'static str {
    match parsed {
        Some(k) if k.channel.as_deref() == Some("subagent") => "spawn-child",
        Some(k) if matches!(k.peer_kind.as_deref(), Some("group") | Some("channel")) => "group",
        Some(k) if k.peer_id.is_some() => "direct",
        _ => "main",
    }
}

/// OpenClaw-specific metadata to stash on `path.meta.extra["openclaw"]`.
pub fn openclaw_meta_extra(session: &OpenClawSession) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    let parsed = session.parsed_key.as_ref();
    if let Some(key) = &session.session_key {
        map.insert("sessionKey".into(), json!(key));
    }
    if let Some(p) = parsed {
        if !p.agent_id.is_empty() {
            map.insert("agentId".into(), json!(p.agent_id));
        }
        if let Some(c) = &p.channel {
            map.insert("channel".into(), json!(c));
        }
        if let Some(k) = &p.peer_kind {
            map.insert("peerKind".into(), json!(k));
        }
        if let Some(id) = &p.peer_id {
            map.insert("peerId".into(), json!(id));
        }
        if let Some(t) = &p.thread_id {
            map.insert("threadId".into(), json!(t));
        }
    }
    map.insert("sessionKind".into(), json!(session_kind(parsed)));
    map
}

/// Convert an [`OpenClawSession`] into a provider-agnostic [`ConversationView`].
pub fn session_to_view(session: &OpenClawSession) -> ConversationView {
    let env = environment_for(session);

    let mut turns: Vec<Turn> = Vec::new();
    let mut tool_call_locs: HashMap<String, (usize, usize)> = HashMap::new();
    let mut delegation_locs: HashMap<String, (usize, usize)> = HashMap::new();
    let mut tool_results: Vec<(String, String, bool)> = Vec::new();
    // (turn index, wire usage, has responseId) per usage-bearing assistant
    // message, for run-cumulative aggregate detection below.
    let mut assistant_usage_rows: Vec<(usize, Usage, bool)> = Vec::new();

    let system_turn = |id: &str, parent: &Option<String>, ts: &str, text: String| Turn {
        id: id.to_string(),
        parent_id: parent.clone(),
        group_id: None,
        role: Role::System,
        timestamp: ts.to_string(),
        text,
        text_signature: None,
        thinking: None,
        thinking_signature: None,
        tool_uses: vec![],
        model: None,
        response_model: None,
        stop_reason: None,
        marker: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: Some(env.clone()),
        delegations: vec![],
        file_mutations: vec![],
    };

    for entry in &session.entries {
        match entry {
            Entry::Session(_)
            | Entry::ModelChange { .. }
            | Entry::ThinkingLevelChange { .. }
            | Entry::Label { .. }
            | Entry::SessionInfo { .. }
            | Entry::Custom { .. }
            | Entry::Leaf { .. } => {
                // Rendering/metadata/pointer entries: no IR turn.
            }

            Entry::Compaction {
                base,
                summary,
                first_kept_entry_id,
                tokens_before,
                details,
                from_hook,
                ..
            } => {
                let (read_files, modified_files) = summary_detail_files(details.as_ref());
                turns.push(Turn {
                    marker: Some(ConversationMarker::Compaction {
                        first_kept_id: (!first_kept_entry_id.is_empty())
                            .then(|| first_kept_entry_id.clone()),
                        tokens_before: Some(*tokens_before),
                        read_files,
                        modified_files,
                        from_hook: *from_hook,
                    }),
                    ..system_turn(
                        &base.id,
                        &base.parent_id,
                        &base.timestamp,
                        format!("Compacted (summary): {summary}"),
                    )
                });
            }
            Entry::BranchSummary {
                base,
                summary,
                from_id,
                details,
                from_hook,
                ..
            } => {
                let (read_files, modified_files) = summary_detail_files(details.as_ref());
                turns.push(Turn {
                    marker: Some(ConversationMarker::BranchSummary {
                        from_id: (!from_id.is_empty()).then(|| from_id.clone()),
                        read_files,
                        modified_files,
                        from_hook: *from_hook,
                    }),
                    ..system_turn(
                        &base.id,
                        &base.parent_id,
                        &base.timestamp,
                        format!("Branch summary: {summary}"),
                    )
                });
            }
            Entry::CustomMessage {
                base,
                custom_type,
                content,
                ..
            } => {
                turns.push(Turn {
                    role: Role::Other(format!("custom:{custom_type}")),
                    text: content.text(),
                    ..system_turn(&base.id, &base.parent_id, &base.timestamp, String::new())
                });
            }

            Entry::Message { base, message, .. } => {
                let text;
                let mut text_signature = None;
                let mut thinking = None;
                let mut thinking_signature = None;
                let mut tool_uses: Vec<ToolInvocation> = Vec::new();
                let mut file_mutations: Vec<FileMutation> = Vec::new();
                let mut model: Option<String> = None;
                let mut response_model: Option<String> = None;
                let mut group_id: Option<String> = None;
                let mut stop_reason_s: Option<String> = None;
                let mut token_usage: Option<TokenUsage> = None;
                let mut delegations: Vec<DelegatedWork> = Vec::new();
                let role: Role;

                match message {
                    AgentMessage::User { .. } => {
                        role = Role::User;
                        text = message.text();
                    }
                    AgentMessage::Assistant {
                        content,
                        model: m,
                        usage,
                        stop_reason,
                        extra,
                        ..
                    } => {
                        role = Role::Assistant;
                        text = message.text();
                        thinking = message.thinking();
                        model = Some(m.clone());
                        stop_reason_s = Some(stop_reason_to_string(stop_reason));
                        token_usage = usage_to_token_usage(usage);
                        // `responseId` is the provider's per-response message id —
                        // the same identifier class Claude carries in `group_id`
                        // (Anthropic message.id). One OpenClaw assistant message =
                        // one API response = one accounting unit.
                        group_id = extra_str(extra, "responseId");
                        response_model = extra_str(extra, "responseModel");
                        text_signature = first_block_signature(content, "text", "textSignature");
                        thinking_signature =
                            first_block_signature(content, "thinking", "thinkingSignature");
                        if token_usage.is_some() {
                            assistant_usage_rows.push((
                                turns.len(),
                                usage.clone(),
                                group_id.is_some(),
                            ));
                        }

                        let turn_idx = turns.len();
                        for block in content {
                            let ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                                extra: block_extra,
                            } = block
                            else {
                                continue;
                            };
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

                            if category == Some(ToolCategory::FileWrite)
                                && let Some(path) = extract_file_path(arguments)
                            {
                                file_mutations.push(FileMutation {
                                    path,
                                    tool_id: Some(id.clone()),
                                    operation: Some(write_operation(name, arguments)),
                                    raw_diff: None,
                                    before: None,
                                    after: None,
                                    rename_to: None,
                                });
                            }

                            tool_uses.push(ToolInvocation {
                                id: id.clone(),
                                name: name.clone(),
                                input: arguments.clone(),
                                result: None,
                                category,
                                thought_signature: block_extra
                                    .get("thoughtSignature")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                execution_mode: block_extra
                                    .get("executionMode")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                            });
                        }
                    }
                    AgentMessage::ToolResult {
                        tool_call_id,
                        is_error,
                        ..
                    } => {
                        tool_results.push((tool_call_id.clone(), message.text(), *is_error));
                        continue;
                    }
                    AgentMessage::BashExecution {
                        command,
                        output,
                        exit_code,
                        ..
                    } => {
                        role = Role::Other("bash".to_string());
                        text = format!("$ {command}\n{output}");
                        tool_uses.push(ToolInvocation {
                            id: base.id.clone(),
                            name: "bash".to_string(),
                            input: json!({ "command": command }),
                            result: Some(ToolResult {
                                content: output.clone(),
                                is_error: !matches!(exit_code, Some(0)),
                            }),
                            category: Some(ToolCategory::Shell),
                            thought_signature: None,
                            execution_mode: None,
                        });
                    }
                }

                turns.push(Turn {
                    id: base.id.clone(),
                    parent_id: base.parent_id.clone(),
                    group_id,
                    role,
                    timestamp: base.timestamp.clone(),
                    text,
                    text_signature,
                    thinking,
                    thinking_signature,
                    tool_uses,
                    model,
                    response_model,
                    stop_reason: stop_reason_s,
                    marker: None,
                    token_usage,
                    attributed_token_usage: None,
                    environment: Some(env.clone()),
                    delegations,
                    file_mutations,
                });
            }
        }
    }

    // Run-cumulative aggregate detection. OpenClaw's final assembled reply
    // of a multi-call run (observed after a `sessions_yield` re-context) is
    // written WITHOUT a `responseId` and with `usage` equal to the
    // field-wise SUM of every prior per-call usage — the run accumulator's
    // totals, not this step's spend. Stamping it would double-count the
    // session (the repo's token-accounting law: never stamp a cumulative
    // counter or repeated total onto a step). Detection is deliberately
    // conservative: only in sessions that stamp `responseId` on real API
    // responses (native modern files), only for rows lacking one, and only
    // on an exact four-field sum match against the accepted per-call rows.
    let has_response_ids = assistant_usage_rows.iter().any(|(_, _, has_rid)| *has_rid);
    if has_response_ids {
        let mut sum = (0u64, 0u64, 0u64, 0u64);
        let mut priors = 0usize;
        for (turn_idx, usage, has_rid) in &assistant_usage_rows {
            let row = (usage.input, usage.output, usage.cache_read, usage.cache_write);
            if !*has_rid && priors > 0 && row == sum {
                if let Some(t) = turns.get_mut(*turn_idx) {
                    t.token_usage = None;
                }
                continue;
            }
            sum = (
                sum.0 + row.0,
                sum.1 + row.1,
                sum.2 + row.2,
                sum.3 + row.3,
            );
            priors += 1;
        }
    }

    // Pass 2: fold tool results onto their invocations.
    for (tool_call_id, content, is_error) in &tool_results {
        if let Some((turn_idx, tool_idx)) = tool_call_locs.get(tool_call_id)
            && let Some(inv) = turns
                .get_mut(*turn_idx)
                .and_then(|t| t.tool_uses.get_mut(*tool_idx))
        {
            inv.result = Some(ToolResult {
                content: content.clone(),
                is_error: *is_error,
            });
        }
        if let Some((turn_idx, deleg_idx)) = delegation_locs.get(tool_call_id)
            && let Some(d) = turns
                .get_mut(*turn_idx)
                .and_then(|t| t.delegations.get_mut(*deleg_idx))
        {
            d.result = Some(content.clone());
        }
    }

    // Aggregate token usage across assistant turns.
    let mut have_usage = false;
    let mut total = TokenUsage::default();
    for turn in &turns {
        if let Some(u) = &turn.token_usage {
            have_usage = true;
            total.input_tokens = Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
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
    let total_usage = have_usage.then_some(total);

    // files_changed: dedup-in-order from FileWrite tool inputs.
    let mut files_changed: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for turn in &turns {
        for inv in &turn.tool_uses {
            if inv.category == Some(ToolCategory::FileWrite)
                && let Some(p) = extract_file_path(&inv.input)
                && seen.insert(p.clone())
            {
                files_changed.push(p);
            }
        }
    }

    let started_at = parse_ts(&session.header.timestamp);
    let last_activity = turns.last().and_then(|t| parse_ts(&t.timestamp));
    let base = (!session.header.cwd.is_empty()).then(|| SessionBase {
        working_dir: Some(session.header.cwd.clone()),
        ..Default::default()
    });

    ConversationView {
        id: session.header.id.clone(),
        started_at,
        last_activity,
        turns,
        total_usage,
        provider_id: Some(PROVIDER_ID.to_string()),
        files_changed,
        session_ids: session.session_id_chain(),
        events: vec![],
        base,
        ..Default::default()
    }
}

// ── ConversationProvider impl ────────────────────────────────────────

fn to_convo_err(e: crate::error::OpenClawError) -> ConvoError {
    ConvoError::Provider(e.to_string())
}

fn meta_to_conversation_meta(meta: crate::io::SessionMeta) -> ConversationMeta {
    let ts = parse_ts(&meta.timestamp);
    ConversationMeta {
        id: meta.id,
        started_at: ts,
        last_activity: ts,
        message_count: meta.entry_count,
        file_path: Some(meta.file_path),
        predecessor: None,
        successor: None,
    }
}

/// The `project` argument is the OpenClaw **agent id** (default
/// [`crate::DEFAULT_AGENT_ID`]).
impl ConversationProvider for OpenClawConvo {
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
            .ok_or_else(|| ConvoError::Provider(format!("session not found: {conversation_id}")))?;
        Ok(meta_to_conversation_meta(meta))
    }

    fn list_metadata(&self, project: &str) -> Result<Vec<ConversationMeta>, ConvoError> {
        let metas = self.list_sessions(project).map_err(to_convo_err)?;
        Ok(metas.into_iter().map(meta_to_conversation_meta).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read_session_from_file;
    use std::path::Path;

    fn fixture_view() -> ConversationView {
        let mut s = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
        s.attach_routing_key();
        session_to_view(&s)
    }

    #[test]
    fn view_has_roles_tools_and_usage() {
        let v = fixture_view();
        assert_eq!(v.provider_id.as_deref(), Some("openclaw"));
        assert!(v.turns.iter().any(|t| t.role == Role::User));

        let asst = v
            .turns
            .iter()
            .find(|t| t.role == Role::Assistant && !t.tool_uses.is_empty())
            .unwrap();
        let read = &asst.tool_uses[0];
        assert_eq!(read.name, "read_file");
        assert_eq!(read.category, Some(ToolCategory::FileRead));
        assert!(read.result.is_some(), "tool result correlated");
        assert_eq!(read.result.as_ref().unwrap().content, "file contents of x.ts");
        assert!(asst.token_usage.is_some());
    }

    #[test]
    fn edit_tool_emits_structural_file_mutation_without_raw() {
        let v = fixture_view();
        let mutation = v
            .turns
            .iter()
            .flat_map(|t| &t.file_mutations)
            .find(|m| m.path == "src/x.ts")
            .expect("file mutation for the edit");
        assert_eq!(mutation.operation.as_deref(), Some("update"));
        assert!(mutation.raw_diff.is_none(), "no raw diff perspective");
        assert_eq!(mutation.tool_id.as_deref(), Some("call_2"));
        assert_eq!(v.files_changed, vec!["src/x.ts"]);
    }

    #[test]
    fn total_usage_sums_assistant_turns() {
        let v = fixture_view();
        let tu = v.total_usage.unwrap();
        // 1200 + 1500 input, 340 + 120 output
        assert_eq!(tu.input_tokens, Some(2700));
        assert_eq!(tu.output_tokens, Some(460));
    }

    #[test]
    fn classify_real_observed_tool_names() {
        // Names observed in real sessions captured from the v2026.6.11 image.
        assert_eq!(classify_tool("exec"), Some(ToolCategory::Shell));
        assert_eq!(classify_tool("read"), Some(ToolCategory::FileRead));
        assert_eq!(classify_tool("write"), Some(ToolCategory::FileWrite));
        assert_eq!(classify_tool("edit"), Some(ToolCategory::FileWrite));
        // Sub-agent spawn is OpenClaw's delegation tool.
        assert_eq!(classify_tool("sessions_spawn"), Some(ToolCategory::Delegation));
        // Control-flow tools are not delegation (and not misclassified).
        assert_eq!(classify_tool("sessions_yield"), None);
    }

    #[test]
    fn subagent_session_key_is_spawn_child_not_a_channel() {
        // Real key shape observed on disk after a sessions_spawn delegation.
        let key = crate::paths::parse_session_key(
            "agent:main:subagent:e1b075db-1f72-4148-a846-bc5b9d4c2ede",
        );
        // A sub-agent run has no human peer: no channel actor…
        assert_eq!(user_actor_for(Some(&key)), None);
        // …and its session kind is spawn-child, not direct.
        let mut s = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
        s.session_key = Some("agent:main:subagent:e1b075db".into());
        s.parsed_key = Some(key);
        let extra = openclaw_meta_extra(&s);
        assert_eq!(
            extra.get("sessionKind").and_then(|v| v.as_str()),
            Some("spawn-child")
        );
    }

    #[test]
    fn user_actor_for_dm_and_group_and_main() {
        assert_eq!(
            user_actor_for(Some(&crate::paths::parse_session_key(
                "agent:main:whatsapp:direct:155"
            )))
            .as_deref(),
            Some("human:whatsapp/155")
        );
        assert_eq!(
            user_actor_for(Some(&crate::paths::parse_session_key(
                "agent:main:slack:group:T42"
            )))
            .as_deref(),
            Some("human:slack/group/T42")
        );
        assert_eq!(
            user_actor_for(Some(&crate::paths::parse_session_key("agent:main:main"))),
            None
        );
    }

    #[test]
    fn meta_extra_carries_channel_and_kind() {
        let mut s = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
        s.attach_routing_key();
        let extra = openclaw_meta_extra(&s);
        assert_eq!(extra.get("channel").and_then(|v| v.as_str()), Some("whatsapp"));
        assert_eq!(extra.get("peerId").and_then(|v| v.as_str()), Some("15555550123"));
        assert_eq!(extra.get("sessionKind").and_then(|v| v.as_str()), Some("direct"));
    }
}
