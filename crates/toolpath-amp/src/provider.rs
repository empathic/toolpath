//! Build a provider-agnostic [`ConversationView`] from an Amp [`Session`].
//!
//! Mapping reference: `docs/agents/formats/amp/events.md` ("Mapping sketch to
//! the toolpath IR"). One export message becomes one turn — except
//! tool-result-only `user` messages, which are transport plumbing: their
//! results are merged onto the originating invocation and no turn is
//! emitted.
//!
//! Token accounting (kind v1.1.0): Amp reports **clean per-message usage**
//! on every assistant message, so each turn carries its own `token_usage`;
//! `group_id` and `attributed_token_usage` stay `None`, `breakdowns` is
//! omitted, and the derived `totalInputTokens` / capacity `maxInputTokens`
//! are dropped (summing either would fabricate spend). All-zero usage
//! decodes to `None`.

use crate::io::{ConvoIO, ThreadFetcher};
use crate::paths::PathResolver;
use crate::types::{Block, KnownBlock, Message, Session, strip_file_scheme};
use chrono::{DateTime, SecondsFormat};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use toolpath_convo::{
    ConversationEvent, ConversationView, DelegatedWork, FileMutation, ProducerInfo, Role,
    SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Provider identity used for `path-<provider>-…` ids and dispatch.
pub const PROVIDER_ID: &str = "amp";
/// Producer name recorded on the derived `Path`. The version alongside it is
/// the **thread's** creating build (`env.initial.platform.clientVersion`),
/// not the running binary's.
pub const PRODUCER_NAME: &str = "amp";

/// Classify an Amp tool name into toolpath's [`ToolCategory`] ontology.
///
/// The native vocabulary (29 tools at `agentMode: medium`) has **no
/// dedicated read/edit/glob/grep tools** — file work goes through
/// `apply_patch` (writes) and `shell_command` (everything else), a
/// Codex-shaped surface. `skill` is a guidance loader, not an effectful
/// tool: it stays uncategorized deliberately. Exact arms first (verbatim
/// wire names, including capitalized `Task`), then a conservative substring
/// fallback for foreign/renamed tools.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "shell_command" | "shell_command_status" => return Some(ToolCategory::Shell),
        "apply_patch" => return Some(ToolCategory::FileWrite),
        // `finder` and `librarian` are search sub-agents; classified by what
        // they do (search), not how they run.
        "finder" | "librarian" => return Some(ToolCategory::FileSearch),
        "read_web_page" | "web_search" => return Some(ToolCategory::Network),
        "Task" | "oracle" => return Some(ToolCategory::Delegation),
        "skill" => return None,
        _ => {}
    }
    // Substring fallbacks for compound / foreign names.
    let n = name.to_ascii_lowercase();
    if n.contains("shell") || n.contains("terminal") || n.contains("command") || n == "bash" {
        Some(ToolCategory::Shell)
    } else if n.contains("search") || n.contains("grep") || n.contains("glob") {
        Some(ToolCategory::FileSearch)
    // Delegation before write: "dispatch_agent" contains "patch".
    } else if n.contains("task") || n.contains("agent") || n.contains("delegate") {
        Some(ToolCategory::Delegation)
    } else if n.contains("write") || n.contains("edit") || n.contains("patch") {
        Some(ToolCategory::FileWrite)
    } else if n.contains("read") || n.contains("view") || n == "cat" {
        Some(ToolCategory::FileRead)
    } else if n.contains("web") || n.contains("fetch") || n.contains("http") {
        Some(ToolCategory::Network)
    } else {
        None
    }
}

/// The Amp-native tool name for a category — used by the projector (piece
/// 03+) to remap a foreign harness's tools into Amp's vocabulary.
///
/// Amp has no `FileRead` tool at all; the native equivalent of a read is a
/// `shell_command` (`cat`), so that arm intentionally does NOT re-classify
/// to `FileRead`. Totality/disambiguation across all six categories is
/// piece 05's contract; this is the observed-vocabulary baseline.
pub fn native_name(category: ToolCategory, input: &Value) -> &'static str {
    match category {
        ToolCategory::Shell | ToolCategory::FileRead => "shell_command",
        ToolCategory::FileWrite => "apply_patch",
        ToolCategory::FileSearch => "finder",
        ToolCategory::Network => {
            if input.get("query").is_some() {
                "web_search"
            } else {
                "read_web_page"
            }
        }
        ToolCategory::Delegation => "Task",
    }
}

/// Convert an Amp [`Session`] into a [`ConversationView`].
pub fn to_view(session: &Session) -> ConversationView {
    let ex = &session.export;
    let working_dir = ex.working_dir();
    let mut turns: Vec<Turn> = Vec::new();

    for (idx, msg) in ex.messages.iter().enumerate() {
        // Merge this message's tool results onto the originating
        // invocations (they always arrive in the *following* `user`
        // message; pair by `toolUseID`).
        for block in &msg.content {
            if let Block::Known(KnownBlock::ToolResult(tr)) = block {
                attach_tool_result(
                    &mut turns,
                    &tr.tool_use_id,
                    tr.run.result.as_ref(),
                    &working_dir,
                );
            }
        }
        if msg.role == "user" && msg.is_tool_result_only() {
            continue; // plumbing, not a turn
        }
        let turn = build_turn(msg, idx);
        push_linked(&mut turns, turn);
    }

    // Session total = field-wise sum of per-message usage. Safe precisely
    // because Amp's usage is per-message (verified non-cumulative; see
    // RECON.md Q2).
    let mut total_usage: Option<TokenUsage> = None;
    for t in &turns {
        if let Some(u) = &t.token_usage {
            merge_usage(&mut total_usage, u);
        }
    }

    // Files changed, first-touch order, deduped.
    let mut files_changed: Vec<String> = Vec::new();
    for t in &turns {
        for fm in &t.file_mutations {
            if !files_changed.contains(&fm.path) {
                files_changed.push(fm.path.clone());
            }
        }
    }

    // Non-conversational envelope data, preserved as events.
    let mut events: Vec<ConversationEvent> = Vec::new();
    let ts = ex.updated_at.clone().unwrap_or_default();
    if let Some(skills) = ex.activated_skills.as_ref().and_then(Value::as_array) {
        for (i, s) in skills.iter().enumerate() {
            events.push(make_event(
                format!("amp-skill-{i:02}"),
                "skill.activated",
                &ts,
                s.clone(),
            ));
        }
    }
    if let Some(meta) = &ex.meta {
        events.push(make_event(
            "amp-thread-meta".to_string(),
            "thread.meta",
            &ts,
            meta.clone(),
        ));
    }

    let base = working_dir.as_ref().map(|wd| SessionBase {
        working_dir: Some(wd.clone()),
        // Amp records no VCS state in a thread.
        vcs_revision: None,
        vcs_branch: None,
        vcs_remote: None,
    });

    ConversationView {
        id: session.id.clone(),
        started_at: session.started_at(),
        last_activity: session.last_activity(),
        turns,
        total_usage,
        provider_id: Some(PROVIDER_ID.to_string()),
        files_changed,
        session_ids: Vec::new(),
        events,
        base,
        producer: Some(ProducerInfo {
            name: PRODUCER_NAME.to_string(),
            version: session.version(),
        }),
    }
}

// ── Turn construction ────────────────────────────────────────────────

fn build_turn(msg: &Message, idx: usize) -> Turn {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => Role::Other(other.to_string()),
    };

    let mut thinking_parts: Vec<&str> = Vec::new();
    let mut tool_uses: Vec<ToolInvocation> = Vec::new();
    let mut delegations: Vec<DelegatedWork> = Vec::new();
    let mut block_start_ms: Option<i64> = None;

    for block in &msg.content {
        match block {
            Block::Known(KnownBlock::Thinking(t)) => {
                // Empty summaries (sealed reasoning) map to nothing, not "".
                if !t.thinking.trim().is_empty() {
                    thinking_parts.push(&t.thinking);
                }
                note_start(&mut block_start_ms, &t.extra);
            }
            Block::Known(KnownBlock::Text(t)) => note_start(&mut block_start_ms, &t.extra),
            Block::Known(KnownBlock::ToolUse(tu)) => {
                if tool_category(&tu.name) == Some(ToolCategory::Delegation) {
                    delegations.push(DelegatedWork {
                        agent_id: tu.id.clone(),
                        prompt: tu
                            .input
                            .get("prompt")
                            .or_else(|| tu.input.get("description"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        // The sub-agent's turns are not in the parent thread
                        // and no sibling thread is created — always empty.
                        turns: Vec::new(),
                        result: None,
                    });
                }
                tool_uses.push(ToolInvocation {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.input.clone(),
                    result: None,
                    category: tool_category(&tu.name),
                });
            }
            Block::Known(KnownBlock::ToolResult(_)) | Block::Unknown(_) => {}
        }
    }

    let token_usage = msg.usage.as_ref().and_then(|u| {
        // Never stamp zero-filled placeholders onto a step.
        if u.is_usage_zero() {
            return None;
        }
        Some(TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_write_tokens: u.cache_creation_input_tokens,
            // Amp doesn't itemize reasoning tokens; nothing to break down.
            breakdowns: Default::default(),
        })
    });

    // Timestamp: assistant messages carry one on `usage`; user messages on
    // `meta.sentAt` (epoch ms); block start times are the fallback.
    let timestamp = msg
        .usage
        .as_ref()
        .and_then(|u| u.timestamp.clone())
        .or_else(|| msg.sent_at_ms().and_then(ms_to_iso))
        .or_else(|| block_start_ms.and_then(ms_to_iso))
        .unwrap_or_default();

    Turn {
        id: msg
            .protocol_message_id
            .clone()
            .unwrap_or_else(|| format!("m{}", idx + 1)),
        parent_id: None, // set by push_linked
        group_id: None,  // usage is already per-message; no grouping needed
        role,
        timestamp,
        text: msg.text(),
        thinking: (!thinking_parts.is_empty()).then(|| thinking_parts.join("\n\n")),
        tool_uses,
        model: msg.usage.as_ref().and_then(|u| u.model.clone()),
        stop_reason: msg.stop_reason().map(str::to_string),
        token_usage,
        attributed_token_usage: None, // Amp reports per message, not per block
        environment: None,
        delegations,
        file_mutations: Vec::new(),
    }
}

fn push_linked(turns: &mut Vec<Turn>, mut t: Turn) {
    if let Some(prev) = turns.last() {
        t.parent_id = Some(prev.id.clone());
    }
    turns.push(t);
}

fn note_start(slot: &mut Option<i64>, extra: &serde_json::Map<String, Value>) {
    if slot.is_none() {
        *slot = extra.get("startTime").and_then(Value::as_i64);
    }
}

fn ms_to_iso(ms: i64) -> Option<String> {
    DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

// ── Result attachment ────────────────────────────────────────────────

/// Attach one `tool_result` to the invocation that produced it (searching
/// back through emitted turns), backfilling the delegation result for
/// `Task` and materializing [`FileMutation`]s for `apply_patch`.
fn attach_tool_result(
    turns: &mut [Turn],
    tool_use_id: &str,
    result: Option<&Value>,
    working_dir: &Option<String>,
) {
    for turn in turns.iter_mut().rev() {
        let Some(inv) = turn
            .tool_uses
            .iter_mut()
            .rev()
            .find(|i| i.id == tool_use_id)
        else {
            continue;
        };
        inv.result = Some(ToolResult {
            content: result.map(result_content).unwrap_or_default(),
            is_error: result.is_some_and(result_is_error),
        });
        if let Some(d) = turn
            .delegations
            .iter_mut()
            .find(|d| d.agent_id == tool_use_id)
            && d.result.is_none()
        {
            d.result = result.and_then(Value::as_str).map(str::to_string);
        }
        let mutations = result
            .map(|r| mutations_from_result(tool_use_id, r, working_dir))
            .unwrap_or_default();
        turn.file_mutations.extend(mutations);
        return;
    }
}

/// Flatten a polymorphic `run.result` into the text the model saw.
fn result_content(result: &Value) -> String {
    match result {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            // shell_command → {output, exitCode}
            if let Some(out) = o.get("output").and_then(Value::as_str) {
                return out.to_string();
            }
            // apply_patch → {files, summary}
            if let Some(summary) = o.get("summary").and_then(Value::as_str) {
                return summary.to_string();
            }
            // skill → {content: [{type: "text", text}]}
            if let Some(items) = o.get("content").and_then(Value::as_array) {
                let texts: Vec<&str> = items
                    .iter()
                    .filter_map(|i| i.get("text").and_then(Value::as_str))
                    .collect();
                if !texts.is_empty() {
                    return texts.join("\n\n");
                }
            }
            serde_json::to_string(result).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Errors are signalled only inside `result` (`run.status` is `"done"` even
/// for failing commands): a non-zero `exitCode` is the one observed signal.
fn result_is_error(result: &Value) -> bool {
    result
        .get("exitCode")
        .and_then(Value::as_i64)
        .is_some_and(|c| c != 0)
}

/// `apply_patch` results embed real unified diffs per file.
fn mutations_from_result(
    tool_id: &str,
    result: &Value,
    working_dir: &Option<String>,
) -> Vec<FileMutation> {
    let Some(files) = result.get("files").and_then(Value::as_array) else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(|f| {
            let uri = f.get("uri").and_then(Value::as_str)?;
            Some(FileMutation {
                path: relativize(strip_file_scheme(uri), working_dir),
                tool_id: Some(tool_id.to_string()),
                operation: f.get("type").and_then(Value::as_str).map(str::to_string),
                raw_diff: f.get("diff").and_then(Value::as_str).map(str::to_string),
                before: None,
                after: None,
                rename_to: None,
            })
        })
        .collect()
}

/// Make paths under the working dir relative to it (artifact keys in the
/// derived path are relative to `path.base`).
fn relativize(path: &str, working_dir: &Option<String>) -> String {
    if let Some(wd) = working_dir
        && let Some(rel) = path.strip_prefix(&format!("{}/", wd.trim_end_matches('/')))
        && !rel.is_empty()
    {
        return rel.to_string();
    }
    path.to_string()
}

fn merge_usage(slot: &mut Option<TokenUsage>, add: &TokenUsage) {
    let sum = |a: Option<u32>, b: Option<u32>| match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    };
    let s = slot.get_or_insert_with(TokenUsage::default);
    s.input_tokens = sum(s.input_tokens, add.input_tokens);
    s.output_tokens = sum(s.output_tokens, add.output_tokens);
    s.cache_read_tokens = sum(s.cache_read_tokens, add.cache_read_tokens);
    s.cache_write_tokens = sum(s.cache_write_tokens, add.cache_write_tokens);
}

fn make_event(id: String, event_type: &str, ts: &str, payload: Value) -> ConversationEvent {
    let data: HashMap<String, Value> = match payload {
        Value::Object(map) => map.into_iter().collect(),
        Value::Null => HashMap::new(),
        other => {
            let mut m = HashMap::new();
            m.insert("value".to_string(), other);
            m
        }
    };
    ConversationEvent {
        id,
        timestamp: ts.to_string(),
        parent_id: None,
        event_type: event_type.to_string(),
        data,
    }
}

// ── Manager facade ───────────────────────────────────────────────────

/// Reads Amp threads and converts them to [`ConversationView`]s.
#[derive(Debug, Clone, Default)]
pub struct AmpConvo {
    io: ConvoIO,
}

impl AmpConvo {
    /// Infallible: no directory or binary is touched until a read happens.
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::new().with_resolver(resolver),
        }
    }

    pub fn with_fetcher(fetcher: Arc<dyn ThreadFetcher>) -> Self {
        Self {
            io: ConvoIO::new().with_fetcher(fetcher),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn read_session(&self, thread_id: &str) -> crate::Result<Session> {
        self.io.read_session(thread_id)
    }

    pub fn list_sessions(&self) -> crate::Result<Vec<crate::types::SessionMetadata>> {
        let ids = self.io.list_thread_ids()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.io.read_metadata(&id) {
                Ok(m) => out.push(m),
                Err(e) => eprintln!("Warning: failed to read thread {}: {}", id, e),
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        Ok(out)
    }

    pub fn read_all_sessions(&self) -> crate::Result<Vec<Session>> {
        let ids = self.io.list_thread_ids()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.io.read_session(&id) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("Warning: failed to read thread {}: {}", id, e),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::ExportReader;

    const REAL: &str = include_str!("../tests/fixtures/real-session.json");

    fn real_view() -> ConversationView {
        let export = ExportReader::parse_export_with(REAL, true).unwrap();
        to_view(&Session::from_export(export))
    }

    fn synthetic(json: &str) -> ConversationView {
        let export = ExportReader::parse_export(json).unwrap();
        to_view(&Session::from_export(export))
    }

    #[test]
    fn one_turn_per_message_except_tool_result_plumbing() {
        let view = real_view();
        // 24 messages = 1 human prompt + 12 assistant + 11 tool-result-only.
        assert_eq!(view.turns.len(), 13);
        let users = view.turns.iter().filter(|t| t.role == Role::User).count();
        let assistants = view
            .turns
            .iter()
            .filter(|t| t.role == Role::Assistant)
            .count();
        assert_eq!(users, 1, "tool-result messages are not turns");
        assert_eq!(assistants, 12);
    }

    #[test]
    fn turn_ids_are_protocol_message_ids_chained_linearly() {
        let view = real_view();
        assert_eq!(view.turns[0].id, "M-033wt7sbSDccKHLOPDTqjB");
        assert!(view.turns[0].parent_id.is_none());
        for pair in view.turns.windows(2) {
            assert_eq!(
                pair[1].parent_id.as_deref(),
                Some(pair[0].id.as_str()),
                "linear parent chain"
            );
        }
    }

    #[test]
    fn tool_results_merged_onto_originating_turns() {
        let view = real_view();
        let tools: Vec<&ToolInvocation> = view.turns.iter().flat_map(|t| &t.tool_uses).collect();
        assert_eq!(tools.len(), 11);
        assert!(
            tools.iter().all(|t| t.result.is_some()),
            "every invocation carries its result"
        );
        // shell ls output landed on the right invocation.
        let ls = tools
            .iter()
            .find(|t| t.input.get("command").and_then(Value::as_str) == Some("ls -la"))
            .unwrap();
        assert!(ls.result.as_ref().unwrap().content.contains("total 0"));
    }

    #[test]
    fn error_signalled_by_exit_code_not_status() {
        let view = real_view();
        let errored: Vec<&ToolInvocation> = view
            .turns
            .iter()
            .flat_map(|t| &t.tool_uses)
            .filter(|t| t.result.as_ref().is_some_and(|r| r.is_error))
            .collect();
        // Exactly one: the deliberately-failing `cat does-not-exist.txt`
        // (run.status was "done" even for it — status is a lifecycle state).
        assert_eq!(errored.len(), 1);
        assert!(
            errored[0]
                .input
                .get("command")
                .and_then(Value::as_str)
                .unwrap()
                .contains("does-not-exist"),
        );
    }

    #[test]
    fn categories_verbatim_names() {
        let view = real_view();
        let tools: Vec<&ToolInvocation> = view.turns.iter().flat_map(|t| &t.tool_uses).collect();
        let by_cat = |c: ToolCategory| tools.iter().filter(|t| t.category == Some(c)).count();
        assert_eq!(by_cat(ToolCategory::Shell), 6);
        assert_eq!(by_cat(ToolCategory::FileWrite), 3);
        assert_eq!(by_cat(ToolCategory::Delegation), 1);
        // `skill` stays uncategorized but keeps its verbatim name.
        let skill = tools.iter().find(|t| t.name == "skill").unwrap();
        assert!(skill.category.is_none());
        assert!(tools.iter().any(|t| t.name == "Task"), "verbatim casing");
    }

    #[test]
    fn apply_patch_mutations_with_native_diffs() {
        let view = real_view();
        let muts: Vec<&FileMutation> = view.turns.iter().flat_map(|t| &t.file_mutations).collect();
        assert_eq!(muts.len(), 3, "add notes.md, update notes.md, add count.sh");
        assert!(muts.iter().all(|m| m.tool_id.is_some()));
        assert!(muts.iter().all(|m| m.raw_diff.is_some()));
        let update = muts
            .iter()
            .find(|m| m.operation.as_deref() == Some("update"))
            .unwrap();
        assert_eq!(update.path, "notes.md", "relativized against the cwd");
        let diff = update.raw_diff.as_deref().unwrap();
        assert!(diff.contains("-scratch — feature elicitation"));
        assert!(diff.contains("+fixture — feature elicitation"));
        assert_eq!(view.files_changed, vec!["notes.md", "count.sh"]);
    }

    #[test]
    fn task_becomes_delegation_with_backfilled_result() {
        let view = real_view();
        let delegations: Vec<&DelegatedWork> =
            view.turns.iter().flat_map(|t| &t.delegations).collect();
        assert_eq!(delegations.len(), 1);
        let d = delegations[0];
        assert!(d.prompt.contains("Count the words"));
        assert_eq!(
            d.result.as_deref(),
            Some("`notes.md` contains **11 words**.")
        );
        assert!(d.turns.is_empty(), "no child thread exists");
        // The delegation correlates to the Task tool call by TU id.
        let task = view
            .turns
            .iter()
            .flat_map(|t| &t.tool_uses)
            .find(|t| t.name == "Task")
            .unwrap();
        assert_eq!(d.agent_id, task.id);
    }

    #[test]
    fn empty_thinking_maps_to_none() {
        let view = real_view();
        let with_thinking = view.turns.iter().filter(|t| t.thinking.is_some()).count();
        // 5 of 12 thinking blocks carried text; the sealed-empty ones must
        // not become Some("").
        assert_eq!(with_thinking, 5);
        assert!(
            view.turns
                .iter()
                .filter_map(|t| t.thinking.as_deref())
                .all(|s| !s.trim().is_empty())
        );
    }

    #[test]
    fn per_message_token_usage_no_grouping_no_attribution() {
        let view = real_view();
        for t in &view.turns {
            assert!(t.group_id.is_none(), "usage is per-message; no grouping");
            assert!(t.attributed_token_usage.is_none(), "no per-block truth");
            if t.role == Role::Assistant {
                let u = t.token_usage.as_ref().expect("assistant carries usage");
                assert!(u.breakdowns.is_empty(), "no reasoning itemization");
            } else {
                assert!(t.token_usage.is_none(), "user turns carry no usage");
            }
        }
        let m2 = &view.turns[1];
        let u = m2.token_usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, Some(0));
        assert_eq!(u.output_tokens, Some(117));
        assert_eq!(u.cache_read_tokens, Some(0));
        assert_eq!(u.cache_write_tokens, Some(16954));
    }

    #[test]
    fn session_total_is_field_wise_sum() {
        let view = real_view();
        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(0));
        assert_eq!(total.output_tokens, Some(1537));
        assert_eq!(total.cache_read_tokens, Some(210_694));
        assert_eq!(total.cache_write_tokens, Some(20_758));
    }

    #[test]
    fn all_zero_usage_decodes_to_none() {
        let view = synthetic(
            r#"{"id":"T-z","messages":[{"role":"assistant","state":{"type":"complete","stopReason":"end_turn"},"usage":{"model":"m","timestamp":"2026-07-27T18:00:00.000Z","inputTokens":0,"outputTokens":0,"maxInputTokens":272000,"totalInputTokens":0,"cacheReadInputTokens":0,"cacheCreationInputTokens":0},"content":[{"type":"text","text":"hi"}]}]}"#,
        );
        assert!(
            view.turns[0].token_usage.is_none(),
            "placeholder, not spend"
        );
        assert!(view.total_usage.is_none());
    }

    #[test]
    fn max_and_total_input_tokens_never_summed() {
        let view = real_view();
        let total = view.total_usage.as_ref().unwrap();
        // 12 × maxInputTokens (272k) or Σ totalInputTokens would be orders of
        // magnitude beyond any real counter here.
        for v in [
            total.input_tokens,
            total.output_tokens,
            total.cache_read_tokens,
            total.cache_write_tokens,
        ] {
            assert!(v.unwrap_or(0) < 272_000, "capacity leaked into spend");
        }
    }

    #[test]
    fn timestamps_model_and_stop_reason() {
        let view = real_view();
        assert_eq!(view.turns[0].timestamp, "2026-07-27T18:34:15.723Z"); // meta.sentAt
        let m2 = &view.turns[1];
        assert_eq!(m2.timestamp, "2026-07-27T18:34:20.295Z"); // usage.timestamp
        assert_eq!(m2.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(m2.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            view.turns.last().unwrap().stop_reason.as_deref(),
            Some("end_turn")
        );
    }

    #[test]
    fn view_metadata_populated() {
        let view = real_view();
        assert_eq!(view.id, "T-019fa4db-29cf-70c9-8d9b-81524df70e52");
        assert_eq!(view.provider_id.as_deref(), Some("amp"));
        let base = view.base.as_ref().unwrap();
        assert_eq!(base.working_dir.as_deref(), Some("/tmp/amp-elicit"));
        assert!(base.vcs_branch.is_none(), "Amp records no git state");
        assert!(base.vcs_revision.is_none());
        let p = view.producer.as_ref().unwrap();
        assert_eq!(p.name, "amp");
        assert_eq!(p.version.as_deref(), Some("0.0.1785170481-ga5b614"));
        assert!(view.started_at.is_some());
        assert!(view.last_activity.is_some());
    }

    #[test]
    fn envelope_extras_become_events() {
        let view = real_view();
        let skill = view
            .events
            .iter()
            .find(|e| e.event_type == "skill.activated")
            .unwrap();
        assert_eq!(skill.data["name"], "using-superpowers");
        let meta = view
            .events
            .iter()
            .find(|e| e.event_type == "thread.meta")
            .unwrap();
        assert_eq!(meta.data["visibility"], "private");
    }

    #[test]
    fn category_exact_arms() {
        assert_eq!(tool_category("shell_command"), Some(ToolCategory::Shell));
        assert_eq!(
            tool_category("shell_command_status"),
            Some(ToolCategory::Shell)
        );
        assert_eq!(tool_category("apply_patch"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("finder"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("librarian"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("web_search"), Some(ToolCategory::Network));
        assert_eq!(tool_category("read_web_page"), Some(ToolCategory::Network));
        assert_eq!(tool_category("Task"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("oracle"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("skill"), None);
        assert_eq!(tool_category("painter"), None);
    }

    #[test]
    fn category_substring_fallback_for_foreign_names() {
        assert_eq!(tool_category("Bash"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("grep_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("edit_file"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("read_file"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("web_fetch"), Some(ToolCategory::Network));
        assert_eq!(
            tool_category("dispatch_agent"),
            Some(ToolCategory::Delegation)
        );
    }

    #[test]
    fn native_name_baseline() {
        use serde_json::json;
        assert_eq!(
            native_name(ToolCategory::Shell, &json!({})),
            "shell_command"
        );
        assert_eq!(
            native_name(ToolCategory::FileRead, &json!({})),
            "shell_command",
            "amp has no read tool; a shell cat is the native equivalent"
        );
        assert_eq!(
            native_name(ToolCategory::FileWrite, &json!({})),
            "apply_patch"
        );
        assert_eq!(native_name(ToolCategory::FileSearch, &json!({})), "finder");
        assert_eq!(
            native_name(ToolCategory::Network, &json!({"query": "x"})),
            "web_search"
        );
        assert_eq!(
            native_name(ToolCategory::Network, &json!({"url": "x"})),
            "read_web_page"
        );
        assert_eq!(native_name(ToolCategory::Delegation, &json!({})), "Task");
        // Every native name maps back into a category (FileRead's shell
        // fallback lands on Shell — Amp genuinely has no read tool).
        for c in [
            ToolCategory::Shell,
            ToolCategory::FileWrite,
            ToolCategory::FileSearch,
            ToolCategory::Delegation,
        ] {
            assert_eq!(tool_category(native_name(c, &json!({}))), Some(c));
        }
    }

    #[test]
    fn skill_result_content_flattened() {
        let view = real_view();
        let skill = view
            .turns
            .iter()
            .flat_map(|t| &t.tool_uses)
            .find(|t| t.name == "skill")
            .unwrap();
        let content = &skill.result.as_ref().unwrap().content;
        assert!(content.contains("using-superpowers Skill"));
        assert!(!content.starts_with('{'), "text extracted, not JSON-dumped");
    }

    #[test]
    fn apply_patch_result_content_is_summary() {
        let view = real_view();
        let patch = view
            .turns
            .iter()
            .flat_map(|t| &t.tool_uses)
            .find(|t| t.name == "apply_patch")
            .unwrap();
        assert!(
            patch
                .result
                .as_ref()
                .unwrap()
                .content
                .starts_with("add: /tmp/amp-elicit/notes.md")
        );
    }

    #[test]
    fn result_content_shapes() {
        use serde_json::json;
        assert_eq!(result_content(&json!("plain")), "plain");
        assert_eq!(result_content(&json!({"output": "x", "exitCode": 0})), "x");
        assert_eq!(result_content(&json!({"files": [], "summary": "s"})), "s");
        assert_eq!(
            result_content(
                &json!({"content": [{"type":"text","text":"a"},{"type":"text","text":"b"}]})
            ),
            "a\n\nb"
        );
        // Unrecognized object shapes degrade to JSON.
        assert_eq!(result_content(&json!({"weird": 1})), r#"{"weird":1}"#);
    }

    #[test]
    fn unmatched_tool_result_is_ignored_not_fatal() {
        let view = synthetic(
            r#"{"id":"T-y","messages":[{"role":"user","content":[{"type":"tool_result","toolUseID":"TU-orphan","run":{"result":{"output":"x","exitCode":0},"status":"done"}}]}]}"#,
        );
        assert!(view.turns.is_empty());
    }
}
