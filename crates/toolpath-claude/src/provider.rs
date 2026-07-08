//! Implementation of `toolpath-convo` traits for Claude conversations.
//!
//! Handles cross-entry tool result assembly: Claude's JSONL format writes
//! tool invocations and their results as separate entries. This module
//! pairs them by `tool_use_id` so consumers get complete `Turn` values
//! with `ToolInvocation.result` populated.

use std::collections::HashMap;

use crate::ClaudeConvo;
use crate::types::{Conversation, ConversationEntry, Message, MessageContent, MessageRole};
#[cfg(any(feature = "watcher", test))]
use toolpath_convo::WatcherEvent;
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError, DelegatedWork,
    EnvironmentSnapshot, Role, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

// ── Conversion helpers ───────────────────────────────────────────────

fn claude_role_to_role(role: &MessageRole) -> Role {
    match role {
        MessageRole::User => Role::User,
        MessageRole::Assistant => Role::Assistant,
        MessageRole::System => Role::System,
    }
}

/// Classify a Claude Code tool into toolpath's category ontology.
///
/// Returns `None` for unrecognized tools. When Claude Code adds or
/// renames tools, update this map.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "Read" => Some(ToolCategory::FileRead),
        "Glob" | "Grep" => Some(ToolCategory::FileSearch),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => Some(ToolCategory::FileWrite),
        "Bash" => Some(ToolCategory::Shell),
        "WebFetch" | "WebSearch" => Some(ToolCategory::Network),
        "Task" | "Agent" => Some(ToolCategory::Delegation),
        _ => None,
    }
}

/// Reverse of [`tool_category`]: pick Claude's native tool name for a
/// given [`ToolCategory`], disambiguating by `args` shape where needed
/// (e.g. `Edit` vs `Write`, `Glob` vs `Grep`).
///
/// Returns `None` when no Claude-canonical equivalent exists. Mirrors
/// the `provider::native_name` helpers on opencode / codex / gemini /
/// pi — projectors call it to surface cross-harness tool calls under
/// the names Claude Code's UI knows how to render.
pub fn native_name(category: ToolCategory, args: &serde_json::Value) -> Option<&'static str> {
    let has = |k: &str| args.get(k).is_some();
    match category {
        ToolCategory::Shell => Some("Bash"),
        ToolCategory::FileRead => Some("Read"),
        ToolCategory::FileWrite => Some(if has("old_string") || has("oldString") {
            "Edit"
        } else {
            "Write"
        }),
        ToolCategory::FileSearch => Some(
            // Grep takes a regex `pattern` and often has output_mode/type
            // hints; Glob takes a glob pattern. When ambiguous, default to
            // Glob — its file-list rendering at least shows results.
            if has("output_mode") || has("path_pattern") || has("type") {
                "Grep"
            } else {
                "Glob"
            },
        ),
        ToolCategory::Network => Some(if has("url") { "WebFetch" } else { "WebSearch" }),
        ToolCategory::Delegation => Some("Task"),
    }
}

/// Convert a single entry to a Turn without cross-entry assembly.
/// Tool results within the same message are still matched.
fn message_to_turn(entry: &ConversationEntry, msg: &Message) -> Turn {
    let text = msg.text();

    let thinking = msg.thinking().map(|parts| parts.join("\n"));

    let tool_uses: Vec<ToolInvocation> = msg
        .tool_uses()
        .into_iter()
        .map(|tu| {
            let result = find_tool_result_in_parts(msg, tu.id);
            let category = tool_category(tu.name);
            ToolInvocation {
                id: tu.id.to_string(),
                name: tu.name.to_string(),
                input: tu.input.clone(),
                result,
                category,
            }
        })
        .collect();

    let file_mutations = compute_file_mutations(&tool_uses, entry.cwd.as_deref());

    let token_usage = msg.usage.as_ref().map(|u| TokenUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_input_tokens,
        cache_write_tokens: u.cache_creation_input_tokens,
        ..Default::default()
    });

    let environment = if entry.cwd.is_some() || entry.git_branch.is_some() {
        Some(EnvironmentSnapshot {
            working_dir: entry.cwd.clone(),
            vcs_branch: entry.git_branch.clone(),
            vcs_revision: None,
        })
    } else {
        None
    };

    let delegations = extract_delegations(&tool_uses);

    Turn {
        id: entry.uuid.clone(),
        parent_id: entry.parent_uuid.clone(),
        // Group key: the API message ID (`msg_…`). Claude Code writes one
        // JSONL line per content block, so several turns can share one
        // group_id — and each repeats the message-level `usage`. Downstream
        // accounting (sum_usage, derive_path) counts a message group once.
        //
        // Some captures omit `message.id`. The entry-level `requestId` still
        // identifies the API request for assistant entries — Anthropic's
        // request ID is "useful for deduping streamed messages" (one
        // assistant message per request; see
        // docs/agents/formats/claude-code/jsonl-envelope.md) — so it's the
        // natural fallback: split lines of an id-less message still dedupe
        // their repeated usage. User entries never group.
        group_id: msg.id.clone().or_else(|| {
            (msg.role == MessageRole::Assistant)
                .then(|| entry.request_id.clone())
                .flatten()
        }),
        role: claude_role_to_role(&msg.role),
        timestamp: entry.timestamp.clone(),
        text,
        thinking,
        tool_uses,
        model: msg.model.clone(),
        stop_reason: msg.stop_reason.clone(),
        token_usage,
        attributed_token_usage: None,
        environment,
        delegations,
        file_mutations,
    }
}

/// For each file-write tool invocation in the turn, synthesize a unified
/// diff via [`toolpath_convo::file_write_diff`] and pre-resolve the
/// before-state for `Write` via `git show HEAD:<path>` (best-effort).
/// Each mutation links back to its tool via `tool_id`.
fn compute_file_mutations(
    tool_uses: &[ToolInvocation],
    cwd: Option<&str>,
) -> Vec<toolpath_convo::FileMutation> {
    let mut out = Vec::new();
    for tu in tool_uses {
        if tu.category != Some(ToolCategory::FileWrite) {
            continue;
        }
        let Some(path) = extract_file_path_for_tool(&tu.input) else {
            continue;
        };
        // Only `Write` carries whole-file content; consult git HEAD for
        // its pre-image so the diff isn't addition-only. Other tools
        // (Edit / MultiEdit / NotebookEdit) carry old_string/new_string
        // pairs and don't need a before-state lookup.
        let before_state = if tu.name == "Write" {
            cwd.and_then(|c| git_head_content(c, &path))
        } else {
            None
        };
        let raw_diff =
            toolpath_convo::file_write_diff(&tu.name, &tu.input, &path, before_state.as_deref());
        let operation = match tu.name.as_str() {
            "Write" => Some("add".to_string()),
            "Edit" | "MultiEdit" | "NotebookEdit" => Some("update".to_string()),
            _ => None,
        };
        let after = match tu.name.as_str() {
            "Write" => tu
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        out.push(toolpath_convo::FileMutation {
            path,
            tool_id: Some(tu.id.clone()),
            operation,
            raw_diff,
            before: before_state,
            after,
            rename_to: None,
        });
    }
    out
}

/// Best-effort lookup of a file's contents at `HEAD` in the git repo
/// rooted at `repo_dir` (or one of its ancestors). Shells out to `git
/// show HEAD:<relative-path>`. Returns `None` when any of these hold:
/// `repo_dir` isn't inside a git repo, `path` isn't tracked at `HEAD`,
/// `git` isn't on `PATH`, or the command otherwise fails.
fn git_head_content(repo_dir: &str, path: &str) -> Option<String> {
    use std::path::Path as FsPath;
    use std::process::Command;
    let repo = FsPath::new(repo_dir);
    let file = FsPath::new(path);
    let rel = if file.is_absolute() {
        file.strip_prefix(repo).ok()?.to_path_buf()
    } else {
        file.to_path_buf()
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("HEAD:{rel_str}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn extract_file_path_for_tool(input: &serde_json::Value) -> Option<String> {
    for k in ["file_path", "path", "filename", "file"] {
        if let Some(s) = input.get(k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Extract delegation info from Task tool invocations.
fn extract_delegations(tool_uses: &[ToolInvocation]) -> Vec<DelegatedWork> {
    tool_uses
        .iter()
        .filter(|tu| tu.category == Some(ToolCategory::Delegation))
        .map(|tu| DelegatedWork {
            agent_id: tu.id.clone(),
            prompt: tu
                .input
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            turns: vec![],
            result: tu.result.as_ref().map(|r| r.content.clone()),
        })
        .collect()
}

fn find_tool_result_in_parts(msg: &Message, tool_use_id: &str) -> Option<ToolResult> {
    let parts = match &msg.content {
        Some(MessageContent::Parts(parts)) => parts,
        _ => return None,
    };
    parts.iter().find_map(|p| match p {
        crate::types::ContentPart::ToolResult {
            tool_use_id: id,
            content,
            is_error,
        } if id == tool_use_id => Some(ToolResult {
            content: content.text(),
            is_error: *is_error,
        }),
        _ => None,
    })
}

/// Returns true if this entry is a tool-result-only user message
/// (no human-authored text, only tool_result parts).
fn is_tool_result_only(entry: &ConversationEntry) -> bool {
    let Some(msg) = &entry.message else {
        return false;
    };
    msg.role == MessageRole::User && msg.text().is_empty() && !msg.tool_results().is_empty()
}

/// Merge tool results from a tool-result-only message into existing turns.
///
/// Matches by `tool_use_id` — scans backwards through turns to find the
/// `ToolInvocation` with a matching `id` for each result. This handles
/// cases where a single result entry carries results for tool uses from
/// different assistant turns.
///
/// Returns true if any results were merged.
fn merge_tool_results(turns: &mut [Turn], msg: &Message) -> bool {
    let mut merged = false;
    for tr in msg.tool_results() {
        for turn in turns.iter_mut().rev() {
            if let Some(invocation) = turn
                .tool_uses
                .iter_mut()
                .find(|tu| tu.id == tr.tool_use_id && tu.result.is_none())
            {
                invocation.result = Some(ToolResult {
                    content: tr.content.text(),
                    is_error: tr.is_error,
                });
                merged = true;
                break;
            }
        }
    }
    merged
}

fn entry_to_turn(entry: &ConversationEntry) -> Option<Turn> {
    entry
        .message
        .as_ref()
        .map(|msg| message_to_turn(entry, msg))
}

/// Convert a full conversation to a view with cross-entry tool result assembly.
///
/// Tool-result-only user entries are absorbed into the preceding assistant
/// turn's `ToolInvocation.result` fields rather than emitted as separate turns.
fn conversation_to_view(convo: &Conversation) -> ConversationView {
    let mut turns: Vec<Turn> = Vec::new();
    let mut events: Vec<toolpath_convo::ConversationEvent> = Vec::new();

    // Headerless preamble lines (ai-title, last-prompt, queue-operation,
    // permission-mode, file-history-snapshot, etc.) become events so they
    // round-trip back to JSONL.
    for (idx, raw) in convo.preamble.iter().enumerate() {
        events.push(preamble_to_event(idx, raw));
    }

    // Map from "absorbed-or-skipped entry UUID" → "the previous
    // turn-bearing entry's UUID". Used so that an assistant turn whose
    // wire parentUuid points at a tool-result-only entry (or any other
    // absorbed entry that didn't become a Turn) gets a Turn.parent_id
    // that still maps onto a real Turn — keeping the IR's turn-to-turn
    // chain intact for `derive_path`. The original UUID is preserved
    // via the `tool_result_user` event.
    let mut parent_rewrites: HashMap<String, String> = HashMap::new();
    let mut last_turn_uuid: Option<String> = None;

    for entry in &convo.entries {
        let Some(msg) = &entry.message else {
            // Message-less entries (attachments, snapshots) survive as
            // events so the projector can re-emit them.
            events.push(entry_to_event(entry));
            if let Some(prev) = &last_turn_uuid {
                parent_rewrites.insert(entry.uuid.clone(), prev.clone());
            }
            continue;
        };

        // Tool-result-only user entries get merged into the preceding
        // assistant's tool_uses[i].result and dropped from the turn
        // stream. The next assistant entry's wire parentUuid points at
        // this entry; we record a rewrite so the IR's turn-to-turn chain
        // stays connected. (The projector re-synthesizes the wire-level
        // tool-result entries on the way out from tool_uses[i].result —
        // their original UUIDs aren't preserved across the roundtrip,
        // but the Claude UI walks the chain by parentUuid, not by
        // specific UUIDs, so that's fine.)
        if is_tool_result_only(entry) {
            merge_tool_results(&mut turns, msg);
            if let Some(prev) = &last_turn_uuid {
                parent_rewrites.insert(entry.uuid.clone(), prev.clone());
            }
            continue;
        }

        let mut turn = message_to_turn(entry, msg);
        if let Some(pid) = turn.parent_id.as_ref()
            && let Some(real) = parent_rewrites.get(pid)
        {
            turn.parent_id = Some(real.clone());
        }
        last_turn_uuid = Some(turn.id.clone());
        turns.push(turn);
    }

    canonicalize_message_usage(&mut turns);

    // Re-derive delegation results now that tool results are merged
    for turn in &mut turns {
        for delegation in &mut turn.delegations {
            if delegation.result.is_none()
                && let Some(tu) = turn
                    .tool_uses
                    .iter()
                    .find(|tu| tu.id == delegation.agent_id)
            {
                delegation.result = tu.result.as_ref().map(|r| r.content.clone());
            }
        }
    }

    let total_usage = sum_usage(&turns);
    let files_changed = extract_files_changed(&turns);

    // Pull path-level base/producer from the first entry that carries the
    // metadata (Claude records cwd / git_branch / version on every
    // conversational entry; the first one is the canonical "this is where
    // we started").
    let mut base = toolpath_convo::SessionBase::default();
    let mut producer_version: Option<String> = None;
    for entry in &convo.entries {
        if base.working_dir.is_none()
            && let Some(cwd) = &entry.cwd
        {
            base.working_dir = Some(cwd.clone());
        }
        if base.vcs_branch.is_none()
            && let Some(b) = &entry.git_branch
        {
            base.vcs_branch = Some(b.clone());
        }
        if producer_version.is_none()
            && let Some(v) = &entry.version
        {
            producer_version = Some(v.clone());
        }
        if base.working_dir.is_some() && base.vcs_branch.is_some() && producer_version.is_some() {
            break;
        }
    }
    let view_base = if base.working_dir.is_some()
        || base.vcs_branch.is_some()
        || base.vcs_revision.is_some()
        || base.vcs_remote.is_some()
    {
        Some(base)
    } else {
        None
    };
    let producer = producer_version.map(|v| toolpath_convo::ProducerInfo {
        name: "claude-code".into(),
        version: Some(v),
    });

    ConversationView {
        id: convo.session_id.clone(),
        started_at: convo.started_at,
        last_activity: convo.last_activity,
        turns,
        total_usage,
        provider_id: Some("claude-code".into()),
        files_changed,
        session_ids: vec![],
        events,
        base: view_base,
        producer,
    }
}

/// Build an event from a headerless preamble JSON line (`ai-title`,
/// `last-prompt`, `queue-operation`, `permission-mode`, `file-history-snapshot`,
/// or anything else above `entries` in Claude's JSONL).
///
/// The whole line is preserved verbatim under `data["raw"]`; the projector
/// dumps it straight back onto `convo.preamble`. We don't model the shape —
/// a headerless line is identified by the presence of `data["raw"]`, not by
/// an enumerated `type` list. `event_type` carries the line's `type`, purely
/// informational.
fn preamble_to_event(idx: usize, raw: &serde_json::Value) -> toolpath_convo::ConversationEvent {
    let event_type = raw
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("preamble")
        .to_string();
    let timestamp = raw
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("raw".to_string(), raw.clone());
    toolpath_convo::ConversationEvent {
        id: format!("claude-preamble-{idx}"),
        timestamp,
        parent_id: None,
        event_type,
        data,
    }
}

/// Build an event from a message-less ConversationEntry (attachment, snapshot).
///
/// Captures the entry's typed fields in `event.data` so the projector can
/// reconstruct an equivalent entry. The flatten extras (e.g. an attachment's
/// `attachment` payload) come along for the ride under `entry_extra`.
fn entry_to_event(entry: &ConversationEntry) -> toolpath_convo::ConversationEvent {
    let mut data = HashMap::new();
    if let Some(v) = &entry.cwd {
        data.insert("cwd".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &entry.git_branch {
        data.insert("git_branch".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &entry.version {
        data.insert("version".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &entry.user_type {
        data.insert("user_type".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &entry.message_id {
        data.insert("message_id".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &entry.tool_use_result {
        data.insert("tool_use_result".into(), v.clone());
    }
    if let Some(v) = &entry.snapshot {
        data.insert("snapshot".into(), v.clone());
    }
    if !entry.extra.is_empty()
        && let Ok(value) = serde_json::to_value(&entry.extra)
    {
        data.insert("entry_extra".into(), value);
    }
    toolpath_convo::ConversationEvent {
        id: entry.uuid.clone(),
        timestamp: entry.timestamp.clone(),
        parent_id: entry.parent_uuid.clone(),
        event_type: entry.entry_type.clone(),
        data,
    }
}

/// Field-wise maximum of two usage tuples. `None` is "absent", not 0, so a
/// field present in only one operand survives.
pub(crate) fn max_usage(a: &TokenUsage, b: &TokenUsage) -> TokenUsage {
    fn m(x: Option<u32>, y: Option<u32>) -> Option<u32> {
        match (x, y) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(v), None) | (None, Some(v)) => Some(v),
            (None, None) => None,
        }
    }
    TokenUsage {
        input_tokens: m(a.input_tokens, b.input_tokens),
        output_tokens: m(a.output_tokens, b.output_tokens),
        cache_read_tokens: m(a.cache_read_tokens, b.cache_read_tokens),
        cache_write_tokens: m(a.cache_write_tokens, b.cache_write_tokens),
        ..Default::default()
    }
}

/// Canonicalize message-level accounting for split messages.
///
/// Claude Code writes one JSONL line per content block of an assistant API
/// message, each stamped with `message.usage`. That `usage` is a **streaming
/// snapshot**, not a per-line bill: per the Anthropic streaming API,
/// `message_start` seeds `output_tokens` near zero and each `message_delta`
/// reports the running **cumulative** total, with the final value being the
/// message total. So across a split message's lines, `input`/`cache` are
/// constant and `output_tokens` climbs to the total on the final line —
/// confirmed across every session sampled (~27% of multi-line messages vary;
/// the rest repeat one value stamped after generation). The intermediate
/// values are flush-time snapshots, **not** per-content-block costs (a real
/// prose block routinely shows `output_tokens: 1`), so we do not derive
/// per-step attribution from them, and — the format being undocumented — we
/// do not trust line order.
///
/// Groups by `group_id` **globally**, not by consecutive run — multi-terminal
/// writers can interleave a split message's lines non-contiguously (see
/// docs/agents/formats/claude-code/known-issues.md, "Multi-terminal writes to
/// the same project"). Treating an interleaved group as two contiguous runs
/// would count the message total once per fragment; this sets `token_usage`
/// on the group's **last occurrence** (by turn order) to the field-wise
/// **maximum** across *all* the group's turns and clears it from the rest, so
/// summing `token_usage` over turns yields session totals regardless of
/// interleaving.
fn canonicalize_message_usage(turns: &mut [Turn]) {
    use std::collections::HashMap;

    // gid -> (field-wise max across ALL occurrences, index of last occurrence).
    let mut groups: HashMap<String, (Option<TokenUsage>, usize)> = HashMap::new();
    for (i, t) in turns.iter().enumerate() {
        let Some(gid) = &t.group_id else { continue };
        let entry = groups.entry(gid.clone()).or_insert((None, i));
        if let Some(u) = &t.token_usage {
            entry.0 = Some(match &entry.0 {
                Some(acc) => max_usage(acc, u),
                None => u.clone(),
            });
        }
        entry.1 = i;
    }

    for t in turns.iter_mut() {
        if t.group_id.is_some() {
            t.token_usage = None;
        }
    }
    for (total, last) in groups.into_values() {
        if let Some(total) = total {
            turns[last].token_usage = Some(total);
        }
    }
}

/// Sum token usage across all turns.
///
/// Adjacency-free: a turn's usage counts only when it has no `group_id`, or
/// when it's the **last** turn (by index) carrying its `group_id` — computed
/// by scanning for each gid's max index, not by checking the next turn, so
/// interleaved (non-contiguous) groups still count once. Note the counted
/// value is that last turn's **own** `token_usage`: it equals the message
/// total only when the group's usage is repeated on every line or carried on
/// the last line (or was already canonicalized to the field-wise max).
/// Production always runs [`canonicalize_message_usage`] first, which
/// guarantees that precondition.
fn sum_usage(turns: &[Turn]) -> Option<TokenUsage> {
    use std::collections::HashMap;

    let mut last_occurrence: HashMap<&str, usize> = HashMap::new();
    for (idx, turn) in turns.iter().enumerate() {
        if let Some(gid) = &turn.group_id {
            last_occurrence.insert(gid.as_str(), idx);
        }
    }

    let mut total = TokenUsage::default();
    let mut any = false;
    for (idx, turn) in turns.iter().enumerate() {
        // Turns sharing a group_id all repeat that message's usage; count it
        // once, on the group's last occurrence.
        if let Some(gid) = &turn.group_id
            && last_occurrence.get(gid.as_str()) != Some(&idx)
        {
            continue;
        }
        if let Some(u) = &turn.token_usage {
            any = true;
            total.input_tokens =
                Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
            total.output_tokens =
                Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
            total.cache_read_tokens = match (total.cache_read_tokens, u.cache_read_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            total.cache_write_tokens = match (total.cache_write_tokens, u.cache_write_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
        }
    }
    if any { Some(total) } else { None }
}

/// Extract deduplicated file paths from file-write tool invocations.
fn extract_files_changed(turns: &[Turn]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for turn in turns {
        for tool_use in &turn.tool_uses {
            if tool_use.category == Some(ToolCategory::FileWrite)
                && let Some(path) = tool_use.input.get("file_path").and_then(|v| v.as_str())
                && seen.insert(path.to_string())
            {
                files.push(path.to_string());
            }
        }
    }
    files
}

#[cfg(any(feature = "watcher", test))]
fn entry_to_watcher_event(entry: &ConversationEntry) -> WatcherEvent {
    match entry_to_turn(entry) {
        Some(turn) => WatcherEvent::Turn(Box::new(turn)),
        None => {
            let mut data = serde_json::json!({
                "uuid": entry.uuid,
                "timestamp": entry.timestamp,
            });
            if !entry.extra.is_empty() {
                data["claude"] = serde_json::to_value(&entry.extra).unwrap_or_default();
            }
            WatcherEvent::Progress {
                kind: entry.entry_type.clone(),
                data,
            }
        }
    }
}

// ── ConversationProvider for ClaudeConvo ──────────────────────────────

impl ConversationProvider for ClaudeConvo {
    fn list_conversations(&self, project: &str) -> toolpath_convo::Result<Vec<String>> {
        crate::ClaudeConvo::list_conversations(self, project)
            .map_err(|e| ConvoError::Provider(e.to_string()))
    }

    fn load_conversation(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let convo = self
            .read_conversation(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        let mut view = conversation_to_view(&convo);
        view.session_ids = convo.session_ids.clone();
        Ok(view)
    }

    fn load_metadata(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let meta = self
            .read_conversation_metadata(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;

        Ok(ConversationMeta {
            id: meta.session_id,
            started_at: meta.started_at,
            last_activity: meta.last_activity,
            message_count: meta.message_count,
            file_path: Some(meta.file_path),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_conversation_metadata(project)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;

        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.session_id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: Some(m.file_path),
                predecessor: None,
                successor: None,
            })
            .collect())
    }
}

// ── ConversationWatcher with eager emit + TurnUpdated ────────────────

#[cfg(feature = "watcher")]
impl toolpath_convo::ConversationWatcher for crate::watcher::ConversationWatcher {
    fn poll(&mut self) -> toolpath_convo::Result<Vec<WatcherEvent>> {
        let entries = crate::watcher::ConversationWatcher::poll(self)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;

        let mut events: Vec<WatcherEvent> = Vec::new();

        // Check for session rotations and prepend Progress events
        for (from, to) in self.take_pending_rotations() {
            events.push(WatcherEvent::Progress {
                kind: "session_rotated".into(),
                data: serde_json::json!({
                    "from": from,
                    "to": to,
                }),
            });
        }

        for entry in &entries {
            let Some(msg) = &entry.message else {
                events.push(entry_to_watcher_event(entry));
                continue;
            };

            if is_tool_result_only(entry) {
                // Find matching turns in previously emitted events and in
                // our assembled state, merge results, emit TurnUpdated.
                // Walk events in reverse to find the turn to update.
                let mut updated_turn: Option<Turn> = None;

                // Search backwards through events emitted this poll cycle
                for event in events.iter_mut().rev() {
                    if let WatcherEvent::Turn(turn) | WatcherEvent::TurnUpdated(turn) = event
                        && turn.tool_uses.iter().any(|tu| {
                            tu.result.is_none()
                                && msg.tool_results().iter().any(|tr| tr.tool_use_id == tu.id)
                        })
                    {
                        // Merge results into this turn
                        let mut updated = (**turn).clone();
                        merge_tool_results(std::slice::from_mut(&mut updated), msg);
                        updated_turn = Some(updated.clone());
                        // Also update the existing event in-place so later
                        // result entries can find the right state
                        **turn = updated;
                        break;
                    }
                }

                if let Some(turn) = updated_turn {
                    events.push(WatcherEvent::TurnUpdated(Box::new(turn)));
                }
                // If no matching turn found, the tool-result-only entry
                // is silently dropped (the matching turn was emitted in a
                // prior poll cycle and can't be updated from here).
                continue;
            }

            events.push(entry_to_watcher_event(entry));
        }

        Ok(events)
    }

    fn seen_count(&self) -> usize {
        crate::watcher::ConversationWatcher::seen_count(self)
    }
}

// ── Public re-exports for convenience ────────────────────────────────

/// Convert a Claude [`Conversation`] directly into a [`ConversationView`].
///
/// This performs cross-entry tool result assembly: tool-result-only user
/// entries are merged into the preceding assistant turn rather than emitted
/// as separate turns.
pub fn to_view(convo: &Conversation) -> ConversationView {
    conversation_to_view(convo)
}

/// Convert a single Claude [`ConversationEntry`] into a [`Turn`], if it
/// contains a message.
///
/// Note: this does *not* perform cross-entry assembly. For assembled
/// results, use [`to_view`] instead.
pub fn to_turn(entry: &ConversationEntry) -> Option<Turn> {
    entry_to_turn(entry)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathResolver;
    use std::fs;
    use tempfile::TempDir;

    /// One assistant turn carrying a cumulative usage snapshot (only
    /// output varies across a split, so input/cache are fixed here).
    fn grp_turn(id: &str, mid: &str, output: u32) -> Turn {
        let mut t = message_turn_stub(id);
        t.group_id = Some(mid.into());
        t.token_usage = Some(TokenUsage {
            input_tokens: Some(6),
            output_tokens: Some(output),
            cache_read_tokens: Some(14_842),
            cache_write_tokens: Some(429_831),
            ..Default::default()
        });
        t
    }

    fn message_turn_stub(id: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2024-01-01T00:00:00Z".into(),
            text: String::new(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
        }
    }

    #[test]
    fn canonicalize_streamed_group_keeps_total_only_on_final_turn() {
        // Streaming snapshots climb 55 -> 164 across two lines of one
        // message. The final turn carries the message total (the final
        // snapshot); earlier turns carry nothing. The intermediate snapshot
        // (55) is NOT per-block attribution — it's where generation happened
        // to be when the line was flushed — so we never record it.
        let mut turns = vec![grp_turn("t1", "msg_A", 55), grp_turn("t2", "msg_A", 164)];
        canonicalize_message_usage(&mut turns);

        assert!(turns[0].token_usage.is_none(), "total only on final turn");
        assert_eq!(turns[1].token_usage.as_ref().unwrap().output_tokens, Some(164));
        assert_eq!(turns[1].token_usage.as_ref().unwrap().input_tokens, Some(6));
        for t in &turns {
            assert!(
                t.attributed_token_usage.is_none(),
                "Claude per-line snapshots are not per-step attribution"
            );
        }
    }

    #[test]
    fn canonicalize_does_not_trust_line_order() {
        // Defensive: the complete total arrives FIRST (out of order). We
        // must still report 164 as the message total — the field-wise max,
        // not the last line's snapshot.
        let mut turns = vec![grp_turn("t1", "msg_A", 164), grp_turn("t2", "msg_A", 55)];
        canonicalize_message_usage(&mut turns);

        assert_eq!(
            turns[1].token_usage.as_ref().unwrap().output_tokens,
            Some(164),
            "field-wise max, not the last line"
        );
    }

    #[test]
    fn canonicalize_collapses_repeated_total_to_one_turn() {
        // Byte-identical lines (the ~73% case): the total lands once, on the
        // final turn; no attribution either way.
        let mut turns = vec![
            grp_turn("t1", "msg_A", 997),
            grp_turn("t2", "msg_A", 997),
            grp_turn("t3", "msg_A", 997),
        ];
        canonicalize_message_usage(&mut turns);

        assert!(turns[0].token_usage.is_none());
        assert!(turns[1].token_usage.is_none());
        assert_eq!(turns[2].token_usage.as_ref().unwrap().output_tokens, Some(997));
        for t in &turns {
            assert!(t.attributed_token_usage.is_none());
        }
    }

    #[test]
    fn interleaved_group_ids_still_count_message_usage_once() {
        // Two terminals interleaving writes to the same project (see
        // docs/agents/formats/claude-code/known-issues.md, "Multi-terminal
        // writes to the same project") can split one message's lines
        // non-contiguously: A(gid=m1), B(gid=m2), C(gid=m1). Adjacency-based
        // grouping treats A and C as two independent one-line "runs" and
        // counts msg_A's usage twice.
        let turns_before = vec![
            grp_turn("t1", "msg_A", 100),
            grp_turn("t2", "msg_B", 50),
            grp_turn("t3", "msg_A", 100),
        ];

        // sum_usage must be adjacency-free: correct even before
        // canonicalization runs.
        let total = sum_usage(&turns_before).unwrap();
        assert_eq!(total.output_tokens, Some(150), "X + Y, not 2X + Y");

        let mut turns = turns_before;
        canonicalize_message_usage(&mut turns);

        assert!(
            turns[0].token_usage.is_none(),
            "msg_A's first (non-last) fragment must not carry the total"
        );
        assert_eq!(
            turns[1].token_usage.as_ref().unwrap().output_tokens,
            Some(50),
            "msg_B's only line keeps its usage"
        );
        assert_eq!(
            turns[2].token_usage.as_ref().unwrap().output_tokens,
            Some(100),
            "msg_A's total lands on its LAST occurrence, not its first"
        );

        let total_after = sum_usage(&turns).unwrap();
        assert_eq!(total_after.output_tokens, Some(150), "still X + Y after canonicalization");
    }

    /// An id-less assistant message split across content-block lines: two
    /// entries share `requestId` but `message.id` is absent from both.
    fn setup_idless_message_provider() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Fix the bug"}}"#.to_string(),
            r#"{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","requestId":"req_1","message":{"role":"assistant","content":[{"type":"text","text":"Working on it."}],"model":"claude-opus-4-7","stop_reason":null,"usage":{"input_tokens":5,"output_tokens":10}}}"#.to_string(),
            r#"{"uuid":"uuid-3","type":"assistant","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","requestId":"req_1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}],"model":"claude-opus-4-7","stop_reason":"tool_use","usage":{"input_tokens":5,"output_tokens":10}}}"#.to_string(),
        ];
        fs::write(project_dir.join("session-3.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn idless_assistant_message_groups_by_request_id() {
        let (_temp, provider) = setup_idless_message_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-3")
            .unwrap();

        assert_eq!(view.turns.len(), 3);
        assert!(view.turns[0].group_id.is_none(), "user line carries no group id");
        assert!(
            view.turns[1].group_id.is_some(),
            "id-less assistant lines still get a group id (from requestId)"
        );
        assert_eq!(
            view.turns[1].group_id, view.turns[2].group_id,
            "both lines of the split id-less message share one group id"
        );

        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(
            total.output_tokens,
            Some(10),
            "one message's usage counted once, not once per content-block line"
        );
    }

    #[test]
    fn user_entries_never_group_by_request_id() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // A user entry carrying `requestId` (not something real Claude Code
        // emits per docs/agents/formats/claude-code/jsonl-envelope.md, which
        // scopes `requestId` to assistant entries — but the grouping logic
        // must not rely on that being enforced upstream).
        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","requestId":"req_x","message":{"role":"user","content":"Fix the bug"}}"#,
        ];
        fs::write(project_dir.join("session-4.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-4")
            .unwrap();

        assert_eq!(view.turns.len(), 1);
        assert!(
            view.turns[0].group_id.is_none(),
            "user entries never group by request_id"
        );
    }

    fn setup_provider() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Fix the bug"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll fix that."},{"type":"thinking","thinking":"The bug is in auth"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/main.rs"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            r#"{"uuid":"uuid-3","type":"user","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main() { println!(\"hello\"); }","is_error":false}]}}"#,
            r#"{"uuid":"uuid-4","type":"assistant","parentUuid":"uuid-3","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"I see the issue. Let me fix it."},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/main.rs","old_string":"hello","new_string":"fixed"}}],"model":"claude-opus-4-6","stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":100}}}"#,
            r#"{"uuid":"uuid-5","type":"user","parentUuid":"uuid-4","timestamp":"2024-01-01T00:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"File written successfully","is_error":false}]}}"#,
            r#"{"uuid":"uuid-6","type":"assistant","parentUuid":"uuid-5","timestamp":"2024-01-01T00:00:05Z","message":{"role":"assistant","content":"Done! The bug is fixed.","model":"claude-opus-4-6","stop_reason":"end_turn"}}"#,
            r#"{"uuid":"uuid-7","type":"user","parentUuid":"uuid-6","timestamp":"2024-01-01T00:00:06Z","message":{"role":"user","content":"Thanks!"}}"#,
        ];
        fs::write(project_dir.join("session-1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    /// A session whose first assistant API message is split across three
    /// JSONL lines (text, then one per tool_use) — the on-disk shape Claude
    /// Code writes. Each line repeats the same `message.id` and the full
    /// message-level `usage`, followed by a singleton assistant message.
    fn setup_split_message_provider() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let usage_a = r#"{"input_tokens":6,"output_tokens":997,"cache_read_input_tokens":14842,"cache_creation_input_tokens":429831}"#;
        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Fix the bug"}}"#.to_string(),
            format!(
                r#"{{"uuid":"uuid-2","type":"assistant","parentUuid":"uuid-1","timestamp":"2024-01-01T00:00:01Z","message":{{"id":"msg_A","role":"assistant","content":[{{"type":"text","text":"Working on it."}}],"model":"claude-opus-4-7","stop_reason":null,"usage":{usage_a}}}}}"#
            ),
            format!(
                r#"{{"uuid":"uuid-3","type":"assistant","parentUuid":"uuid-2","timestamp":"2024-01-01T00:00:02Z","message":{{"id":"msg_A","role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"a.rs"}}}}],"model":"claude-opus-4-7","stop_reason":null,"usage":{usage_a}}}}}"#
            ),
            format!(
                r#"{{"uuid":"uuid-4","type":"assistant","parentUuid":"uuid-3","timestamp":"2024-01-01T00:00:03Z","message":{{"id":"msg_A","role":"assistant","content":[{{"type":"tool_use","id":"t2","name":"Read","input":{{"file_path":"b.rs"}}}}],"model":"claude-opus-4-7","stop_reason":"tool_use","usage":{usage_a}}}}}"#
            ),
            r#"{"uuid":"uuid-5","type":"assistant","parentUuid":"uuid-4","timestamp":"2024-01-01T00:00:04Z","message":{"id":"msg_B","role":"assistant","content":[{"type":"text","text":"Done."}],"model":"claude-opus-4-7","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":11}}}"#.to_string(),
        ];
        fs::write(project_dir.join("session-2.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn test_split_message_turns_share_group_id() {
        let (_temp, provider) = setup_split_message_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-2")
            .unwrap();

        assert_eq!(view.turns.len(), 5);
        assert!(view.turns[0].group_id.is_none(), "user lines carry no ID");
        for turn in &view.turns[1..=3] {
            assert_eq!(turn.group_id.as_deref(), Some("msg_A"));
        }
        assert_eq!(view.turns[4].group_id.as_deref(), Some("msg_B"));
    }

    #[test]
    fn test_view_usage_is_canonical_total_on_group_final_turn() {
        // IR contract: `Turn.token_usage` always means "the message's
        // total" and appears only on the message's final turn. The wire
        // repeats the total on every line of a split; the view must not.
        let (_temp, provider) = setup_split_message_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-2")
            .unwrap();

        assert!(view.turns[1].token_usage.is_none());
        assert!(view.turns[2].token_usage.is_none());
        assert_eq!(
            view.turns[3].token_usage.as_ref().unwrap().output_tokens,
            Some(997)
        );
        assert_eq!(
            view.turns[4].token_usage.as_ref().unwrap().output_tokens,
            Some(11)
        );
    }

    #[test]
    fn test_total_usage_counts_each_message_once() {
        let (_temp, provider) = setup_split_message_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-2")
            .unwrap();

        // msg_A's usage appears on three lines but is one API message;
        // totals must be msg_A + msg_B, not 3×msg_A + msg_B.
        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.output_tokens, Some(997 + 11));
        assert_eq!(total.input_tokens, Some(6 + 5));
        assert_eq!(total.cache_read_tokens, Some(14_842));
        assert_eq!(total.cache_write_tokens, Some(429_831));
    }

    #[test]
    fn test_load_conversation_assembles_tool_results() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        assert_eq!(view.id, "session-1");
        // 7 entries collapse to 5 turns (2 tool-result-only entries absorbed)
        assert_eq!(view.turns.len(), 5);

        // Turn 0: user "Fix the bug"
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert!(view.turns[0].parent_id.is_none());

        // Turn 1: assistant with tool use + assembled result
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(
            view.turns[1].thinking.as_deref(),
            Some("The bug is in auth")
        );
        assert_eq!(view.turns[1].tool_uses.len(), 1);
        assert_eq!(view.turns[1].tool_uses[0].name, "Read");
        assert_eq!(view.turns[1].tool_uses[0].id, "t1");
        // Key assertion: result is populated from the next entry
        let result = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("fn main()"));
        assert_eq!(view.turns[1].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(view.turns[1].stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("uuid-1"));

        // Token usage
        let usage = view.turns[1].token_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));

        // Turn 2: second assistant with tool use + assembled result
        assert_eq!(view.turns[2].role, Role::Assistant);
        assert_eq!(view.turns[2].text, "I see the issue. Let me fix it.");
        assert_eq!(view.turns[2].tool_uses[0].name, "Edit");
        let result2 = view.turns[2].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(result2.content, "File written successfully");

        // Turn 3: final assistant (no tools)
        assert_eq!(view.turns[3].role, Role::Assistant);
        assert_eq!(view.turns[3].text, "Done! The bug is fixed.");
        assert!(view.turns[3].tool_uses.is_empty());

        // Turn 4: user "Thanks!"
        assert_eq!(view.turns[4].role, Role::User);
        assert_eq!(view.turns[4].text, "Thanks!");
    }

    #[test]
    fn test_no_phantom_empty_turns() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        // No turns should have empty text with User role (phantom turns)
        for turn in &view.turns {
            if turn.role == Role::User {
                assert!(
                    !turn.text.is_empty(),
                    "Found phantom empty user turn: {:?}",
                    turn.id
                );
            }
        }
    }

    #[test]
    fn test_tool_result_error_flag() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read a file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"/nonexistent"}}],"stop_reason":"tool_use"}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"File not found","is_error":true}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2); // user + assistant (tool-result absorbed)
        let result = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "File not found");
    }

    #[test]
    fn test_multiple_tool_uses_single_result_entry() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Check two files"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading both..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a.rs"}},{"type":"tool_use","id":"t2","name":"Read","input":{"path":"b.rs"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file a contents","is_error":false},{"type":"tool_result","tool_use_id":"t2","content":"file b contents","is_error":false}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[1].tool_uses.len(), 2);

        let r1 = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(r1.content, "file a contents");

        let r2 = view.turns[1].tool_uses[1].result.as_ref().unwrap();
        assert_eq!(r2.content, "file b contents");
    }

    #[test]
    fn test_conversation_without_tool_use_unchanged() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there!"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].text, "Hello");
        assert_eq!(view.turns[1].text, "Hi there!");
    }

    #[test]
    fn test_assistant_turn_without_result_has_none() {
        // Tool use at end of conversation with no result entry
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read a file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert!(view.turns[1].tool_uses[0].result.is_none());
    }

    #[test]
    fn test_list_conversations() {
        let (_temp, provider) = setup_provider();
        let ids = ConversationProvider::list_conversations(&provider, "/test/project").unwrap();
        assert_eq!(ids, vec!["session-1"]);
    }

    #[test]
    fn test_load_metadata() {
        let (_temp, provider) = setup_provider();
        let meta =
            ConversationProvider::load_metadata(&provider, "/test/project", "session-1").unwrap();
        assert_eq!(meta.id, "session-1");
        assert_eq!(meta.message_count, 7);
        assert!(meta.file_path.is_some());
    }

    #[test]
    fn test_list_metadata() {
        let (_temp, provider) = setup_provider();
        let metas = ConversationProvider::list_metadata(&provider, "/test/project").unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "session-1");
    }

    #[test]
    fn test_to_view() {
        let (_temp, manager) = setup_provider();
        let convo = manager
            .read_conversation("/test/project", "session-1")
            .unwrap();
        let view = to_view(&convo);
        assert_eq!(view.turns.len(), 5);
        assert_eq!(view.title(20).unwrap(), "Fix the bug");
    }

    #[test]
    fn test_to_turn_with_message() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        let turn = to_turn(&entry).unwrap();
        assert_eq!(turn.id, "u1");
        assert_eq!(turn.text, "hello");
        assert_eq!(turn.role, Role::User);
    }

    #[test]
    fn test_to_turn_without_message() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(to_turn(&entry).is_none());
    }

    #[test]
    fn test_entry_to_watcher_event_turn() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hi"}}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        assert!(matches!(event, WatcherEvent::Turn(_)));
    }

    #[test]
    fn test_entry_to_watcher_event_progress() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        assert!(matches!(event, WatcherEvent::Progress { .. }));
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait_basic() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#,
        ];
        fs::write(project_dir.join("session-1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // Use the trait explicitly (inherent poll returns ConversationEntry)
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], WatcherEvent::Turn(t) if t.role == Role::User));
        assert!(matches!(&events[1], WatcherEvent::Turn(t) if t.role == Role::Assistant));
        assert_eq!(toolpath_convo::ConversationWatcher::seen_count(&watcher), 2);

        // Second poll returns nothing
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert!(events.is_empty());
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait_assembles_tool_results() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read the file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"fn main() {}","is_error":false}]}}"#,
            r#"{"uuid":"u4","type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":"Done!"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "s1".to_string(),
        );

        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();

        // Should get: Turn(user), Turn(assistant), TurnUpdated(assistant), Turn(assistant)
        assert_eq!(events.len(), 4);

        // First: user turn
        assert!(matches!(&events[0], WatcherEvent::Turn(t) if t.role == Role::User));

        // Second: assistant turn emitted eagerly (result may not be populated yet in the event)
        assert!(matches!(&events[1], WatcherEvent::Turn(t) if t.role == Role::Assistant));

        // Third: TurnUpdated with results merged
        match &events[2] {
            WatcherEvent::TurnUpdated(turn) => {
                assert_eq!(turn.id, "u2");
                assert_eq!(turn.tool_uses.len(), 1);
                let result = turn.tool_uses[0].result.as_ref().unwrap();
                assert_eq!(result.content, "fn main() {}");
                assert!(!result.is_error);
            }
            other => panic!("Expected TurnUpdated, got {:?}", other),
        }

        // Fourth: final assistant turn
        assert!(matches!(&events[3], WatcherEvent::Turn(t) if t.text == "Done!"));
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_trait_incremental_tool_results() {
        // Simulate tool results arriving in a different poll cycle than the tool use
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Start with just the user message and assistant tool use
        let entries_phase1 = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read file"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"test.rs"}}]}}"#,
        ];
        fs::write(
            project_dir.join("s1.jsonl"),
            entries_phase1.join("\n") + "\n",
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "s1".to_string(),
        );

        // First poll: get user + assistant turns
        let events1 = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert_eq!(events1.len(), 2);
        // Assistant turn emitted eagerly with result: None
        if let WatcherEvent::Turn(t) = &events1[1] {
            assert!(t.tool_uses[0].result.is_none());
        } else {
            panic!("Expected Turn");
        }

        // Now append the tool result entry
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(project_dir.join("s1.jsonl"))
            .unwrap();
        writeln!(file, r#"{{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"fn main() {{}}","is_error":false}}]}}}}"#).unwrap();

        // Second poll: tool-result-only entry arrives
        let events2 = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        // The tool-result-only entry can't find its matching turn in this poll
        // cycle (it was emitted in the previous one), so it's silently absorbed.
        // This is a known limitation of the eager-emit approach for cross-poll
        // boundaries — the batch path (to_view) handles this correctly.
        // Consumers needing full fidelity across poll boundaries should
        // periodically do a full load_conversation.
        assert!(events2.is_empty() || events2.iter().all(|e| !matches!(e, WatcherEvent::Turn(_))));
    }

    #[test]
    fn test_merge_tool_results_by_id() {
        // Verify that merge matches by tool_use_id, not position
        let mut turns = vec![Turn {
            id: "t1".into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2024-01-01T00:00:00Z".into(),
            text: "test".into(),
            thinking: None,
            tool_uses: vec![
                ToolInvocation {
                    id: "tool-a".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                    result: None,
                    category: Some(ToolCategory::FileRead),
                },
                ToolInvocation {
                    id: "tool-b".into(),
                    name: "Write".into(),
                    input: serde_json::json!({}),
                    result: None,
                    category: Some(ToolCategory::FileWrite),
                },
            ],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }];

        // Create a message with results in reversed order
        let msg: Message = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-b","content":"write result","is_error":false},{"type":"tool_result","tool_use_id":"tool-a","content":"read result","is_error":true}]}"#,
        )
        .unwrap();

        let merged = merge_tool_results(&mut turns, &msg);
        assert!(merged);

        // Results should match by ID regardless of order
        assert_eq!(
            turns[0].tool_uses[0].result.as_ref().unwrap().content,
            "read result"
        );
        assert!(turns[0].tool_uses[0].result.as_ref().unwrap().is_error);

        assert_eq!(
            turns[0].tool_uses[1].result.as_ref().unwrap().content,
            "write result"
        );
        assert!(!turns[0].tool_uses[1].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_is_tool_result_only() {
        // Tool-result-only entry
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}]}}"#,
        )
        .unwrap();
        assert!(is_tool_result_only(&entry));

        // Regular user entry with text
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u2","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));

        // Entry without message
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u3","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));

        // Assistant entry (never tool-result-only)
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u4","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"hi"}}"#,
        )
        .unwrap();
        assert!(!is_tool_result_only(&entry));
    }

    // ── New enrichment tests ─────────────────────────────────────────

    #[test]
    fn test_tool_category_mapping() {
        assert_eq!(tool_category("Read"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("Glob"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("Grep"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("Write"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("Edit"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("NotebookEdit"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("Bash"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("WebFetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("WebSearch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("Task"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("UnknownTool"), None);
    }

    #[test]
    fn test_turn_has_tool_category() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        // Turn 1 (assistant) has a Read tool
        assert_eq!(
            view.turns[1].tool_uses[0].category,
            Some(ToolCategory::FileRead)
        );
        // Turn 2 (assistant) has an Edit tool
        assert_eq!(
            view.turns[2].tool_uses[0].category,
            Some(ToolCategory::FileWrite)
        );
    }

    #[test]
    fn test_environment_populated_from_entry() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","cwd":"/project/path","gitBranch":"feat/auth","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        // User turn has environment (entry has cwd and gitBranch)
        let env = view.turns[0].environment.as_ref().unwrap();
        assert_eq!(env.working_dir.as_deref(), Some("/project/path"));
        assert_eq!(env.vcs_branch.as_deref(), Some("feat/auth"));
        assert!(env.vcs_revision.is_none());

        // Assistant turn has no environment (entry has no cwd/gitBranch)
        assert!(view.turns[1].environment.is_none());
    }

    #[test]
    fn test_cache_tokens_populated() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":200,"cache_read_input_tokens":500}}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        let usage = view.turns[1].token_usage.as_ref().unwrap();
        assert_eq!(usage.cache_read_tokens, Some(500));
        assert_eq!(usage.cache_write_tokens, Some(200));
    }

    #[test]
    fn test_total_usage_aggregated() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        let total = view.total_usage.as_ref().unwrap();
        // Two assistant turns with usage: (100, 50) and (200, 100)
        assert_eq!(total.input_tokens, Some(300));
        assert_eq!(total.output_tokens, Some(150));
    }

    #[test]
    fn test_provider_id_set() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        assert_eq!(view.provider_id.as_deref(), Some("claude-code"));
    }

    #[test]
    fn test_files_changed_populated() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Edit files"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Editing..."},{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"src/main.rs","content":"fn main() {}"}},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/lib.rs","old_string":"a","new_string":"b"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false},{"type":"tool_result","tool_use_id":"t2","content":"ok","is_error":false}]}}"#,
            r#"{"uuid":"u4","type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"More edits..."},{"type":"tool_use","id":"t3","name":"Write","input":{"file_path":"src/main.rs","content":"updated"}}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        // Deduplicated, first-touch order: src/main.rs first, then src/lib.rs
        assert_eq!(view.files_changed, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_delegations_extracted() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Search for bugs"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Delegating..."},{"type":"tool_use","id":"task-1","name":"Task","input":{"prompt":"Find the authentication bug","subagent_type":"Explore"}}]}}"#,
            r#"{"uuid":"u3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"task-1","content":"Found the bug in auth.rs line 42","is_error":false}]}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        // Assistant turn should have one delegation
        assert_eq!(view.turns[1].delegations.len(), 1);
        let d = &view.turns[1].delegations[0];
        assert_eq!(d.agent_id, "task-1");
        assert_eq!(d.prompt, "Find the authentication bug");
        assert!(d.turns.is_empty()); // Sub-agent turns are in separate files
        // Result gets populated from tool result assembly
        assert_eq!(
            d.result.as_deref(),
            Some("Found the bug in auth.rs line 42")
        );
    }

    #[test]
    fn test_progress_data_enriched_with_extras() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z","data":{"type":"hook_progress","hookName":"pre-commit"}}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        match event {
            WatcherEvent::Progress { kind, data } => {
                assert_eq!(kind, "progress");
                assert_eq!(data["uuid"], "u1");
                assert_eq!(data["timestamp"], "2024-01-01T00:00:00Z");
                let claude = &data["claude"];
                assert_eq!(claude["data"]["type"], "hook_progress");
                assert_eq!(claude["data"]["hookName"], "pre-commit");
            }
            other => panic!(
                "Expected Progress, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_progress_data_no_claude_key_when_no_extras() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"progress","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let event = entry_to_watcher_event(&entry);
        match event {
            WatcherEvent::Progress { data, .. } => {
                assert!(data.get("claude").is_none());
            }
            other => panic!(
                "Expected Progress, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_no_delegations_for_non_task_tools() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        // No turns should have delegations (none use Task tool)
        for turn in &view.turns {
            assert!(turn.delegations.is_empty());
        }
    }

    // ── Session chain tests ─────────────────────────────────────────

    fn setup_chained_provider() -> (TempDir, ClaudeConvo) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Session A: original conversation
        let entries_a = [
            r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Fix the bug"}}"#,
            r#"{"uuid":"a2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","sessionId":"session-a","message":{"role":"assistant","content":"I'll fix that.","model":"claude-opus-4-6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        ];
        fs::write(project_dir.join("session-a.jsonl"), entries_a.join("\n")).unwrap();

        // Session B: continuation with bridge entry
        let entries_b = [
            // Bridge entry: session_id points back to session-a
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Continue the fix"}}"#,
            // Real entries in session-b
            r#"{"uuid":"b1","type":"user","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"What about the tests?"}}"#,
            r#"{"uuid":"b2","type":"assistant","timestamp":"2024-01-01T01:00:02Z","sessionId":"session-b","message":{"role":"assistant","content":"Tests pass now.","model":"claude-opus-4-6","usage":{"input_tokens":200,"output_tokens":100}}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), entries_b.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn test_load_conversation_merges_chain() {
        let (_temp, provider) = setup_chained_provider();

        // Load from session-a — should merge with session-b
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-a")
            .unwrap();

        // Should have turns from both segments (minus the bridge entry)
        // session-a: a1 (user), a2 (assistant)
        // session-b: b1 (user), b2 (assistant) — b0 is bridge, filtered
        assert_eq!(view.turns.len(), 4);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(view.turns[2].text, "What about the tests?");
        assert_eq!(view.turns[3].text, "Tests pass now.");

        // Session IDs should be set
        assert_eq!(view.session_ids, vec!["session-a", "session-b"]);
    }

    #[test]
    fn test_load_conversation_skips_bridge_entries() {
        let (_temp, provider) = setup_chained_provider();

        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-a")
            .unwrap();

        // Bridge entry text "Continue the fix" should NOT appear
        for turn in &view.turns {
            assert_ne!(turn.text, "Continue the fix");
        }
    }

    #[test]
    fn test_load_conversation_single_segment_unchanged() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"solo","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"u2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","sessionId":"solo","message":{"role":"assistant","content":"Hi there!"}}"#,
        ];
        fs::write(project_dir.join("solo.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "solo").unwrap();

        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].text, "Hello");
        assert_eq!(view.turns[1].text, "Hi there!");
        // Single segment — session_ids should be empty
        assert!(view.session_ids.is_empty());
    }

    #[test]
    fn test_list_metadata_chain_transparent() {
        let (_temp, provider) = setup_chained_provider();

        let metas = ConversationProvider::list_metadata(&provider, "/test/project").unwrap();

        // Chain-default: only the chain head is returned
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "session-a");

        // Chains are transparent — no predecessor/successor links
        assert!(metas[0].predecessor.is_none());
        assert!(metas[0].successor.is_none());
    }

    #[cfg(feature = "watcher")]
    #[test]
    fn test_watcher_emits_rotation_progress() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Session A
        let entry_a = r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#;
        fs::write(
            project_dir.join("session-a.jsonl"),
            format!("{}\n", entry_a),
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = crate::watcher::ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-a".to_string(),
        );

        // First poll via trait: consume session-a entries
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WatcherEvent::Turn(_)));

        // Create successor session-b
        let entries_b = [
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"b1","type":"user","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"New"}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), entries_b.join("\n")).unwrap();

        // Second poll via trait: should include rotation Progress event
        let events = toolpath_convo::ConversationWatcher::poll(&mut watcher).unwrap();

        // First event: Progress(session_rotated) with from/to
        assert!(
            events.len() >= 2,
            "Expected Progress + Turn, got {} events",
            events.len()
        );
        match &events[0] {
            WatcherEvent::Progress { kind, data } => {
                assert_eq!(kind, "session_rotated");
                assert_eq!(data["from"], "session-a");
                assert_eq!(data["to"], "session-b");
            }
            other => panic!("Expected Progress, got {:?}", std::mem::discriminant(other)),
        }

        // Second event: Turn for b1 (bridge entry b0 filtered out)
        match &events[1] {
            WatcherEvent::Turn(turn) => {
                assert_eq!(turn.id, "b1");
                assert_eq!(turn.text, "New");
            }
            other => panic!("Expected Turn(b1), got {:?}", std::mem::discriminant(other)),
        }

        // No bridge entry should appear as a Turn
        for event in &events {
            if let WatcherEvent::Turn(t) = event {
                assert_ne!(t.id, "b0", "Bridge entry should not appear as a Turn");
            }
        }
    }

    #[test]
    fn test_load_metadata_chain_transparent() {
        let (_temp, provider) = setup_chained_provider();

        // Load from chain head — aggregated metadata
        let meta_a =
            ConversationProvider::load_metadata(&provider, "/test/project", "session-a").unwrap();
        assert_eq!(meta_a.id, "session-a");
        // Aggregated message count across both segments (2 + 3 = 5)
        assert_eq!(meta_a.message_count, 5);
        // Chains are transparent — no predecessor/successor links
        assert!(meta_a.predecessor.is_none());
        assert!(meta_a.successor.is_none());

        // Load from a successor — still resolves the full chain
        let meta_b =
            ConversationProvider::load_metadata(&provider, "/test/project", "session-b").unwrap();
        assert_eq!(meta_b.id, "session-a"); // head of chain
        assert_eq!(meta_b.message_count, 5);
        assert!(meta_b.predecessor.is_none());
        assert!(meta_b.successor.is_none());
    }
}
