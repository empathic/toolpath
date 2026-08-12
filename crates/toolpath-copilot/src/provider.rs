//! Build a provider-agnostic [`ConversationView`] from a Copilot [`Session`].
//!
//! The mapping follows the `events.jsonl` semantics in
//! `docs/agents/formats/copilot-cli/events.md`, verified against first-hand
//! captures at `copilotVersion` 1.0.67–1.0.68 (turns, tools, sub-agents,
//! shutdown totals, and context compaction). Tool-name classification stays
//! deliberately broad for MCP/custom pass-through names.

use crate::io::ConvoIO;
use crate::paths::PathResolver;
use crate::types::{CopilotEvent, Session};
use serde_json::Value;
use std::collections::HashMap;
use toolpath_convo::{
    Compaction, ConversationEvent, ConversationView, DelegatedWork, FileMutation, Item,
    ProducerInfo, Role, SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Provider identity used for `path-<provider>-…` ids and dispatch.
pub const PROVIDER_ID: &str = "copilot";
/// Producer name recorded on the derived `Path`.
pub const PRODUCER_NAME: &str = "copilot-cli";

/// Classify a Copilot tool name into toolpath's [`ToolCategory`] ontology.
///
/// ⚠️ The Copilot CLI's tool vocabulary is **not yet verified against a real
/// session**; this errs toward broad, case-insensitive matching and returns
/// `None` for anything unrecognized (the raw `name`/`input` are still carried
/// on the [`ToolInvocation`]). Tighten with observed names later.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    let n = name.to_ascii_lowercase();
    // Exact-ish matches first.
    match n.as_str() {
        "shell" | "bash" | "sh" | "run" | "exec" | "execute" | "terminal" | "run_in_terminal"
        | "run_command" | "run_shell" | "command" => return Some(ToolCategory::Shell),
        "read" | "read_file" | "readfile" | "view" | "view_file" | "cat" | "open" | "get_file" => {
            return Some(ToolCategory::FileRead);
        }
        "write"
        | "write_file"
        | "writefile"
        | "create"
        | "create_file"
        | "edit"
        | "edit_file"
        | "apply_patch"
        | "patch"
        | "str_replace"
        | "str_replace_editor"
        | "replace"
        | "replace_string_in_file"
        | "insert"
        | "delete_file" => {
            return Some(ToolCategory::FileWrite);
        }
        "glob" | "list" | "list_dir" | "list_directory" | "ls" | "find" | "find_files" | "grep"
        | "search" | "ripgrep" | "rg" | "file_search" | "grep_search" | "semantic_search"
        | "codebase_search" => return Some(ToolCategory::FileSearch),
        "fetch" | "web_fetch" | "fetch_url" | "web_search" | "search_web" | "browser"
        | "open_url" | "http" => return Some(ToolCategory::Network),
        "subagent" | "delegate" | "spawn_agent" | "task" | "agent" | "dispatch_agent" => {
            return Some(ToolCategory::Delegation);
        }
        _ => {}
    }
    // Substring fallbacks for compound / namespaced names.
    if n.contains("shell") || n.contains("terminal") || n.contains("command") {
        Some(ToolCategory::Shell)
    } else if n.contains("search") || n.contains("grep") || n.contains("glob") {
        Some(ToolCategory::FileSearch)
    } else if n.contains("write")
        || n.contains("edit")
        || n.contains("patch")
        || n.contains("replace")
    {
        Some(ToolCategory::FileWrite)
    } else if n.contains("read") || n.contains("view") || n.contains("file") {
        Some(ToolCategory::FileRead)
    } else if n.contains("web") || n.contains("fetch") || n.contains("http") {
        Some(ToolCategory::Network)
    } else {
        None
    }
}

/// Reverse of [`tool_category`]: the Copilot-native tool name for a category.
///
/// Used by the projector to remap a foreign harness's tool name (e.g. Claude's
/// `Bash`, codex's `shell`) into Copilot's vocabulary. `FileWrite`/`FileSearch`
/// are too coarse alone, so the arg shape disambiguates. Every returned name
/// re-classifies back to the same category via [`tool_category`], so a
/// Copilot→Copilot round-trip is stable.
pub fn native_name(category: ToolCategory, input: &serde_json::Value) -> &'static str {
    match category {
        ToolCategory::Shell => "bash",
        ToolCategory::FileRead => "view",
        ToolCategory::FileWrite => {
            // Copilot's real file tools are `edit` (partial replace) and
            // `create` (new file), disambiguated by the presence of an
            // old-string arg.
            if input.get("old_string").is_some() || input.get("old_str").is_some() {
                "edit"
            } else {
                "create"
            }
        }
        ToolCategory::FileSearch => {
            if input.get("query").is_some()
                || input.get("pattern").is_some()
                || input.get("regex").is_some()
            {
                "grep"
            } else {
                "glob"
            }
        }
        ToolCategory::Network => "fetch",
        ToolCategory::Delegation => "task",
    }
}

/// Fold `add` into `slot` field-wise (summing counts). Used to merge several
/// assistant messages into one turn's usage, and to sum per-turn usage into the
/// session total.
pub(crate) fn merge_turn_usage(slot: &mut Option<TokenUsage>, add: Option<TokenUsage>) {
    let Some(add) = add else { return };
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

/// A non-turn stream entry captured during the walk: a generic event or a
/// typed compaction boundary. Kept in ONE arrival-ordered vec (not one per
/// kind) so same-watermark entries preserve wire order — copilot 1.0.68
/// writes a `system.message` re-prime after `session.compaction_complete`
/// and before the next user turn, and draining events and boundaries from
/// separate vecs would put that re-prime ahead of the boundary.
enum Pending {
    Event(ConversationEvent),
    Compaction(Compaction),
}

/// Convert a parsed Copilot [`Session`] into a [`ConversationView`].
pub fn to_view(session: &Session) -> ConversationView {
    let start = session.start();
    let default_model = start.as_ref().and_then(|s| s.model.clone());

    let mut turns: Vec<Turn> = Vec::new();
    let mut current: Option<Turn> = None;
    // Non-turn entries (events and compaction boundaries) in arrival order,
    // each tagged with how many turns precede it in the stream (flushed
    // turns plus a content-bearing in-progress turn), so the final `items`
    // assembly can restore the real interleaving.
    let mut pending: Vec<(usize, Pending)> = Vec::new();
    // Copilot reports per-message tokens (`outputTokens`, and — on a projected
    // session — `inputTokens`/cache). We set them per-turn and sum for the
    // session total; `session.shutdown` (when present) is the fallback total.
    let mut shutdown_usage: Option<TokenUsage> = None;
    // `seq` numbers turns (stable across re-derivation); `aux_seq` numbers
    // fallback tool/sub-agent ids so a turn's id never depends on how many
    // tools preceded it (which would break parent-graph idempotency).
    let mut seq: usize = 0;
    let mut aux_seq: usize = 0;

    for (i, line) in session.lines.iter().enumerate() {
        let ts = line.timestamp.clone().unwrap_or_default();
        match line.event() {
            CopilotEvent::SessionStart(_) => {}
            CopilotEvent::SessionShutdown(s) => {
                if s.input_tokens.is_some() || s.output_tokens.is_some() {
                    shutdown_usage = Some(TokenUsage {
                        input_tokens: s.input_tokens,
                        output_tokens: s.output_tokens,
                        cache_read_tokens: s.cache_read_tokens,
                        cache_write_tokens: s.cache_write_tokens,
                        breakdowns: Default::default(),
                    });
                }
            }
            CopilotEvent::UserMessage(m) => {
                flush(&mut turns, &mut current);
                seq += 1;
                let mut t = empty_turn(format!("u{seq}"), Role::User, ts);
                t.text = m.text;
                push_linked(&mut turns, t);
            }
            CopilotEvent::AssistantTurnStart => {
                flush(&mut turns, &mut current);
                seq += 1;
                let mut t = empty_turn(format!("a{seq}"), Role::Assistant, ts);
                t.model = default_model.clone();
                current = Some(t);
            }
            CopilotEvent::AssistantMessage(m) => {
                let usage = m.token_usage();
                let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                merge_turn_usage(&mut cur.token_usage, usage);
                append_text(&mut cur.text, &m.text);
                if let Some(r) = &m.reasoning {
                    match &mut cur.thinking {
                        Some(t) => {
                            t.push_str("\n\n");
                            t.push_str(r);
                        }
                        None => cur.thinking = Some(r.clone()),
                    }
                }
                if cur.model.is_none() {
                    cur.model = m.model.clone().or_else(|| default_model.clone());
                }
            }
            CopilotEvent::AssistantTurnEnd => {
                flush(&mut turns, &mut current);
            }
            CopilotEvent::ToolStart(te) => {
                let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                aux_seq += 1;
                let tool_id = te.id.clone().unwrap_or_else(|| format!("tool{aux_seq}"));
                if let Some(fm) = file_mutation_for(&tool_id, &te.name, &te.args) {
                    cur.file_mutations.push(fm);
                }
                cur.tool_uses.push(ToolInvocation {
                    id: tool_id,
                    name: te.name.clone(),
                    input: te.args,
                    result: None,
                    category: tool_category(&te.name),
                });
            }
            CopilotEvent::ToolComplete(te) => {
                // Native edit/create completes carry the REAL file-state diff
                // inline (`result.detailedContent`, git-style) — better
                // fidelity than the arg-derived reconstruction from ToolStart;
                // upgrade the matching mutation's raw perspective with it.
                if let (Some(id), Some(det)) = (te.id.as_deref(), te.detailed.as_deref())
                    && det.contains("@@")
                {
                    upgrade_mutation_diff(&mut current, &mut turns, id, det);
                }
                let result = ToolResult {
                    content: te.output.clone().unwrap_or_default(),
                    is_error: te.success == Some(false),
                };
                if !attach_tool_result(
                    &mut current,
                    &mut turns,
                    te.id.as_deref(),
                    &te.name,
                    result.clone(),
                ) {
                    // No matching start seen — synthesize a carrier invocation.
                    let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                    aux_seq += 1;
                    let tool_id = te.id.clone().unwrap_or_else(|| format!("tool{aux_seq}"));
                    if let Some(fm) = file_mutation_for(&tool_id, &te.name, &te.args) {
                        cur.file_mutations.push(fm);
                    }
                    cur.tool_uses.push(ToolInvocation {
                        id: tool_id,
                        name: te.name.clone(),
                        input: te.args,
                        result: Some(result),
                        category: tool_category(&te.name),
                    });
                }
            }
            CopilotEvent::SubagentStarted(s) => {
                let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                aux_seq += 1;
                cur.delegations.push(DelegatedWork {
                    agent_id: s.id.clone().unwrap_or_else(|| format!("sub{aux_seq}")),
                    prompt: s.prompt.unwrap_or_default(),
                    turns: Vec::new(),
                    result: s.result,
                });
            }
            CopilotEvent::SubagentCompleted(s) => {
                backfill_delegation_result(&mut current, &mut turns, s.id.as_deref(), s.result);
            }
            CopilotEvent::SkillInvoked(p) => {
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, "skill.invoked", &ts, p)),
                ));
            }
            CopilotEvent::Hook { kind, payload } => {
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, &kind, &ts, payload)),
                ));
            }
            CopilotEvent::Abort(p) => {
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, "abort", &ts, p)),
                ));
            }
            // Observed at 1.0.68: `session.compaction_start` carries only
            // pre-compaction token bookkeeping ({system,conversation,
            // toolDefinitions}Tokens); the boundary's substance rides
            // `session.compaction_complete` ({success, preCompactionTokens,
            // postCompactionTokens, preCompactionMessagesLength,
            // messagesRemoved, tokensRemoved, summaryContent, checkpoint*}).
            // The start stays a generic event; a successful complete becomes
            // the typed `Item::Compaction`. Copilot reports removed-message
            // *counts*, not surviving ids, so there is no kept anchor —
            // wholesale (`kept_from: None`); the counts and checkpoint path
            // are copilot bookkeeping outside the closed typed set.
            CopilotEvent::CompactionStart(p) => {
                flush(&mut turns, &mut current);
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, "session.compaction_start", &ts, p)),
                ));
            }
            CopilotEvent::CompactionComplete(p) => {
                flush(&mut turns, &mut current);
                if p.get("success").and_then(Value::as_bool) == Some(true) {
                    pending.push((
                        turn_watermark(&turns, &current),
                        Pending::Compaction(Compaction {
                            // Renumbered by final position during assembly,
                            // alongside turn ids.
                            id: String::new(),
                            parent_id: None,
                            timestamp: ts.clone(),
                            trigger: None,
                            summary: p
                                .get("summaryContent")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            pre_tokens: p.get("preCompactionTokens").and_then(Value::as_u64),
                            kept_from: None,
                        }),
                    ));
                } else {
                    pending.push((
                        turn_watermark(&turns, &current),
                        Pending::Event(make_event(i, "session.compaction_complete", &ts, p)),
                    ));
                }
            }
            CopilotEvent::SessionOther { kind, payload } => {
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, &kind, &ts, payload)),
                ));
            }
            CopilotEvent::Unknown { kind, payload } => {
                pending.push((
                    turn_watermark(&turns, &current),
                    Pending::Event(make_event(i, &kind, &ts, payload)),
                ));
            }
        }
    }
    flush(&mut turns, &mut current);

    // Renumber turns by final position so ids don't depend on how many turns
    // were created-then-dropped as empty (which differs across re-derivation
    // and would break parent-graph idempotency). Turn ids are only used for the
    // step DAG; tool/delegation pairing keys off tool/agent ids, not turn ids.
    // Parents are stitched during `items` assembly below, where compaction
    // boundaries join the chain.
    for (i, t) in turns.iter_mut().enumerate() {
        t.id = format!("t{i}");
    }

    // Session total = field-wise sum of per-turn usage (Σ turns = session
    // total). Per-turn usage only carries `output` (Copilot's
    // `assistant.message` reports only `outputTokens`), so the session's real
    // input/cache totals live on `session.shutdown`. When both are present,
    // take `output` from the per-message sum and fill input/cache from the
    // shutdown — otherwise ~all of the session's input + cache tokens (222k in
    // the real fixture) get dropped. Fall back to whichever exists alone.
    let mut summed: Option<TokenUsage> = None;
    for t in &turns {
        if let Some(u) = &t.token_usage {
            merge_turn_usage(&mut summed, Some(u.clone()));
        }
    }
    let total_usage = match (summed, shutdown_usage) {
        (Some(mut s), Some(sd)) => {
            s.input_tokens = s.input_tokens.or(sd.input_tokens);
            s.cache_read_tokens = s.cache_read_tokens.or(sd.cache_read_tokens);
            s.cache_write_tokens = s.cache_write_tokens.or(sd.cache_write_tokens);
            s.output_tokens = s.output_tokens.or(sd.output_tokens);
            Some(s)
        }
        (s, sd) => s.or(sd),
    };

    // Files changed, in first-touch order, deduped.
    let mut files_changed: Vec<String> = Vec::new();
    for t in &turns {
        for fm in &t.file_mutations {
            if !files_changed.contains(&fm.path) {
                files_changed.push(fm.path.clone());
            }
        }
    }

    // Base: `session.start`'s `context` is primary; `workspace.yaml` is the
    // fallback for anything it lacks.
    let ws = session.workspace.as_ref();
    let s = start.as_ref();
    let working_dir = s
        .and_then(|s| s.cwd.clone())
        .or_else(|| s.and_then(|s| s.git_root.clone()))
        .or_else(|| ws.and_then(|w| w.git_root.clone()));
    let vcs_branch = s
        .and_then(|s| s.branch.clone())
        .or_else(|| ws.and_then(|w| w.branch.clone()));
    let vcs_revision = s
        .and_then(|s| s.revision.clone())
        .or_else(|| ws.and_then(|w| w.revision.clone()));
    let vcs_remote = s
        .and_then(|s| s.repository.clone())
        .or_else(|| ws.and_then(|w| w.repository.clone()));
    let base = if working_dir.is_some()
        || vcs_branch.is_some()
        || vcs_revision.is_some()
        || vcs_remote.is_some()
    {
        Some(SessionBase {
            working_dir,
            vcs_revision,
            vcs_branch,
            vcs_remote,
        })
    } else {
        None
    };

    // Merge turns and pending entries into the ordered `items` stream,
    // restoring the real interleaving from each entry's turn watermark.
    // Watermarks are nondecreasing (the vec is append-only), so a single
    // forward pass suffices, and same-watermark entries drain in arrival
    // order — e.g. the `system.message` re-prime that follows a compaction
    // lands after the boundary, matching the wire.
    let to_item = |p: Pending| match p {
        Pending::Event(e) => Item::Event(e),
        Pending::Compaction(c) => Item::Compaction(c),
    };
    let mut items: Vec<Item> = Vec::new();
    let mut pending = pending.into_iter().peekable();
    for (i, t) in turns.into_iter().enumerate() {
        while pending.peek().is_some_and(|(w, _)| *w <= i) {
            items.push(to_item(pending.next().unwrap().1));
        }
        items.push(Item::Turn(t));
    }
    items.extend(pending.map(|(_, p)| to_item(p)));

    // Stitch the linear parent chain through compaction boundaries: a turn
    // after a boundary parents on the boundary, not on the previous turn,
    // so the head-ancestry walk crosses the compaction in order. Compaction
    // ids are renumbered by final position like turn ids — the envelope UUID
    // is re-minted by every projection, so keeping it would break
    // project → re-read idempotency (the id, and with it the post-boundary
    // turn's parent, would change on each hop).
    let mut prev: Option<String> = None;
    let mut compact_n = 0usize;
    for item in &mut items {
        match item {
            Item::Turn(t) => {
                t.parent_id = prev.clone();
                prev = Some(t.id.clone());
            }
            Item::Compaction(c) => {
                c.id = format!("compact{compact_n}");
                compact_n += 1;
                c.parent_id = prev.clone();
                prev = Some(c.id.clone());
            }
            Item::Event(_) => {}
        }
    }

    ConversationView {
        id: session.id.clone(),
        started_at: session.started_at(),
        last_activity: session.last_activity(),
        items,
        total_usage,
        provider_id: Some(PROVIDER_ID.to_string()),
        files_changed,
        session_ids: Vec::new(),
        base,
        producer: Some(ProducerInfo {
            name: PRODUCER_NAME.to_string(),
            version: session.version(),
        }),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn empty_turn(id: String, role: Role, timestamp: String) -> Turn {
    Turn {
        id,
        parent_id: None,
        group_id: None,
        role,
        timestamp,
        text: String::new(),
        thinking: None,
        tool_uses: Vec::new(),
        model: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    }
}

fn ensure_assistant<'a>(
    current: &'a mut Option<Turn>,
    seq: &mut usize,
    model: &Option<String>,
    ts: &str,
) -> &'a mut Turn {
    if current.is_none() {
        *seq += 1;
        let mut t = empty_turn(format!("a{seq}"), Role::Assistant, ts.to_string());
        t.model = model.clone();
        *current = Some(t);
    }
    current.as_mut().unwrap()
}

fn append_text(buf: &mut String, more: &str) {
    if more.is_empty() {
        return;
    }
    if !buf.is_empty() {
        buf.push_str("\n\n");
    }
    buf.push_str(more);
}

fn push_linked(turns: &mut Vec<Turn>, mut t: Turn) {
    if let Some(prev) = turns.last() {
        t.parent_id = Some(prev.id.clone());
    }
    turns.push(t);
}

fn turn_has_content(t: &Turn) -> bool {
    // Token usage counts as content: an aborted response is an empty-text,
    // tool-less assistant message that still consumed real tokens, and
    // dropping it breaks session-total conservation across round-trips.
    !t.text.trim().is_empty()
        || !t.tool_uses.is_empty()
        || !t.delegations.is_empty()
        || !t.file_mutations.is_empty()
        || t.thinking.is_some()
        || t.token_usage.is_some()
        || t.attributed_token_usage.is_some()
}

fn flush(turns: &mut Vec<Turn>, current: &mut Option<Turn>) {
    if let Some(t) = current.take()
        && turn_has_content(&t)
    {
        push_linked(turns, t);
    }
}

/// How many turns precede an event created now: the flushed turns, plus the
/// in-progress turn when it already has content (it started before the event,
/// so it sorts ahead of it in the `items` stream).
fn turn_watermark(turns: &[Turn], current: &Option<Turn>) -> usize {
    turns.len() + current.as_ref().is_some_and(turn_has_content) as usize
}

/// Attach a `tool.execution_complete` result to its matching invocation.
///
/// The Copilot `events.jsonl` schema is unverified, and the reverse-engineering
/// sources never confirmed that tool events carry a correlation id. So:
///
/// - **If an `id` is present** it is treated as authoritative: match it in the
///   open turn, then in earlier turns. A present-but-unmatched id returns
///   `false` (the caller makes a standalone carrier) rather than guessing.
/// - **If there is no `id`** (the likely real case), pair with the most-recent
///   result-less invocation in the *current open turn*, preferring the same
///   tool `name`. `start`/`complete` bracket within a turn, so this matches the
///   sequential common case without double-counting.
///
/// Returns whether a match was found.
fn attach_tool_result(
    current: &mut Option<Turn>,
    turns: &mut [Turn],
    id: Option<&str>,
    name: &str,
    result: ToolResult,
) -> bool {
    if let Some(id) = id {
        if let Some(cur) = current.as_mut()
            && let Some(inv) = cur.tool_uses.iter_mut().rev().find(|t| t.id == id)
        {
            inv.result = Some(result);
            return true;
        }
        for t in turns.iter_mut().rev() {
            if let Some(inv) = t.tool_uses.iter_mut().rev().find(|t| t.id == id) {
                inv.result = Some(result);
                return true;
            }
        }
        return false;
    }
    // No id: positional pairing within the current open turn.
    if let Some(cur) = current.as_mut() {
        if let Some(inv) = cur
            .tool_uses
            .iter_mut()
            .rev()
            .find(|t| t.result.is_none() && t.name == name)
        {
            inv.result = Some(result);
            return true;
        }
        if let Some(inv) = cur.tool_uses.iter_mut().rev().find(|t| t.result.is_none()) {
            inv.result = Some(result);
            return true;
        }
    }
    false
}

/// Replace the arg-derived `raw_diff` on the mutation created by tool call
/// `id` with the native file-state diff from `result.detailedContent`.
fn upgrade_mutation_diff(current: &mut Option<Turn>, turns: &mut [Turn], id: &str, diff: &str) {
    let hit = |t: &mut Turn| {
        t.file_mutations
            .iter_mut()
            .rev()
            .find(|m| m.tool_id.as_deref() == Some(id))
            .map(|m| m.raw_diff = Some(diff.to_string()))
            .is_some()
    };
    if let Some(cur) = current.as_mut()
        && hit(cur)
    {
        return;
    }
    for t in turns.iter_mut().rev() {
        if hit(t) {
            return;
        }
    }
}

fn backfill_delegation_result(
    current: &mut Option<Turn>,
    turns: &mut [Turn],
    id: Option<&str>,
    result: Option<String>,
) {
    let Some(id) = id else { return };
    if let Some(cur) = current.as_mut()
        && let Some(d) = cur.delegations.iter_mut().rev().find(|d| d.agent_id == id)
    {
        if d.result.is_none() {
            d.result = result;
        }
        return;
    }
    for t in turns.iter_mut().rev() {
        if let Some(d) = t.delegations.iter_mut().rev().find(|d| d.agent_id == id) {
            if d.result.is_none() {
                d.result = result;
            }
            return;
        }
    }
}

fn make_event(idx: usize, event_type: &str, ts: &str, payload: Value) -> ConversationEvent {
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
        id: format!("evt-{idx:04}"),
        timestamp: ts.to_string(),
        parent_id: None,
        event_type: event_type.to_string(),
        data,
    }
}

/// Best-effort file-mutation extraction for a `FileWrite`-category tool call.
///
/// Returns `None` for non-file-write tools or when no path is present. When
/// the args carry full content (a create/write), a `raw` unified-diff
/// perspective is synthesized; edit-shaped calls (old/new string) without the
/// full file yield a structural-only mutation (no `raw_diff`). See
/// `docs/agents/formats/copilot-cli/file-fidelity.md`.
fn file_mutation_for(tool_id: &str, name: &str, args: &Value) -> Option<FileMutation> {
    if tool_category(name) != Some(ToolCategory::FileWrite) {
        return None;
    }
    let path = str_arg(
        args,
        &[
            "path",
            "file_path",
            "filePath",
            "filename",
            "file",
            "target_file",
        ],
    )?;

    let n = name.to_ascii_lowercase();
    let is_delete = n.contains("delete");
    let looks_add = n.contains("create") || n.contains("add") || n.contains("new");
    let content = str_arg(
        args,
        &[
            "content",
            "contents",
            "new_content",
            "text",
            "file_text",
            "new_str",
            "newstring",
            "newString",
        ],
    );
    let old = str_arg(args, &["old_string", "old_str", "oldstring", "oldString"]);

    let operation = if is_delete {
        "delete"
    } else if looks_add || (content.is_some() && old.is_none()) {
        "add"
    } else {
        "update"
    };

    // A `raw` perspective requires a full after-content (and a known before).
    // We only have that for full-write/add shapes.
    let (raw_diff, after) = match (&content, old.is_some()) {
        (Some(c), false) if !is_delete => (
            Some(toolpath_convo::unified_diff(&path, "", c)),
            Some(c.clone()),
        ),
        _ => (None, content.clone()),
    };

    Some(FileMutation {
        path,
        tool_id: Some(tool_id.to_string()),
        operation: Some(operation.to_string()),
        raw_diff,
        before: None,
        after: if is_delete { None } else { after },
        rename_to: None,
    })
}

fn str_arg(args: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

// ── Manager facade ───────────────────────────────────────────────────

/// Reads Copilot CLI sessions and converts them to [`ConversationView`]s.
#[derive(Debug, Clone, Default)]
pub struct CopilotConvo {
    io: ConvoIO,
}

impl CopilotConvo {
    pub fn new() -> Self {
        Self { io: ConvoIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: ConvoIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &ConvoIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn read_session(&self, session_id: &str) -> crate::Result<Session> {
        self.io.read_session(session_id)
    }

    pub fn list_sessions(&self) -> crate::Result<Vec<crate::types::SessionMetadata>> {
        self.io.list_sessions()
    }

    pub fn most_recent_session(&self) -> crate::Result<Option<Session>> {
        let dirs = self.io.list_session_dirs()?;
        match dirs.first() {
            Some(dir) => Ok(Some(self.io.read_session_dir(dir)?)),
            None => Ok(None),
        }
    }

    pub fn read_all_sessions(&self) -> crate::Result<Vec<Session>> {
        let dirs = self.io.list_session_dirs()?;
        let mut out = Vec::with_capacity(dirs.len());
        for dir in dirs {
            match self.io.read_session_dir(&dir) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("Warning: failed to read {}: {}", dir.display(), e),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EventLine;

    fn parse(body: &str) -> Session {
        let lines: Vec<EventLine> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Session {
            id: "sess-test".to_string(),
            dir_path: std::path::PathBuf::from("/tmp/sess-test"),
            lines,
            workspace: None,
        }
    }

    // Shaped after a real session (copilotVersion 1.0.67): nested `context`,
    // `toolName`/`toolCallId`, `result.content`, `reasoningText`, `outputTokens`.
    fn body() -> String {
        [
            r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"copilotVersion":"1.0.66","producer":"copilot-agent","context":{"cwd":"/tmp/proj"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-06-30T10:00:01.000Z","data":{"content":"build a thing"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-06-30T10:00:02.000Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"content":"Listing files.","model":"claude-haiku-4.5","reasoningText":"Let me look at the files."}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-06-30T10:00:04.000Z","data":{"toolCallId":"c1","toolName":"bash","arguments":{"command":"ls"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-06-30T10:00:05.000Z","data":{"toolCallId":"c1","success":true,"result":{"content":"a.rs","detailedContent":"a.rs"}}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-06-30T10:00:06.000Z","data":{"toolCallId":"c2","toolName":"create_file","arguments":{"path":"a.rs","content":"fn main() {}\n"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-06-30T10:00:07.000Z","data":{"toolCallId":"c2","success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:08.000Z","data":{"content":"Done."}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-06-30T10:00:09.000Z","data":{}}"#,
            r#"{"type":"session.shutdown","timestamp":"2026-06-30T10:00:10.000Z","data":{"modelMetrics":{"model":"claude-haiku-4.5"},"usage":{"inputTokens":1200,"outputTokens":340}}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn builds_user_and_assistant_turns() {
        let view = to_view(&parse(&body()));
        assert_eq!(view.turns().count(), 2);
        assert_eq!(view.turns().next().unwrap().role, Role::User);
        assert_eq!(view.turns().next().unwrap().text, "build a thing");
        assert_eq!(view.turns().nth(1).unwrap().role, Role::Assistant);
        // Two assistant messages collapsed into one turn.
        assert!(view.turns().nth(1).unwrap().text.contains("Listing files."));
        assert!(view.turns().nth(1).unwrap().text.contains("Done."));
    }

    #[test]
    fn assistant_turn_chains_to_user() {
        let view = to_view(&parse(&body()));
        assert_eq!(
            view.turns().nth(1).unwrap().parent_id.as_deref(),
            Some(view.turns().next().unwrap().id.as_str())
        );
    }

    #[test]
    fn tool_calls_paired_with_results() {
        let view = to_view(&parse(&body()));
        let tools = &view.turns().nth(1).unwrap().tool_uses;
        assert_eq!(tools.len(), 2);
        let shell = tools.iter().find(|t| t.name == "bash").unwrap();
        assert_eq!(shell.category, Some(ToolCategory::Shell));
        // Result content comes from the nested `result.content` object.
        assert_eq!(shell.result.as_ref().unwrap().content, "a.rs");
        assert!(!shell.result.as_ref().unwrap().is_error);
    }

    #[test]
    fn assistant_reasoning_becomes_thinking() {
        let view = to_view(&parse(&body()));
        assert_eq!(
            view.turns().nth(1).unwrap().thinking.as_deref(),
            Some("Let me look at the files.")
        );
    }

    #[test]
    fn output_tokens_summed_when_no_shutdown() {
        // Same as body() but without session.shutdown: total falls back to the
        // sum of per-message outputTokens (50 + 20).
        let body = [
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"content":"a","outputTokens":50}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:08.000Z","data":{"content":"b","outputTokens":20}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
        ]
        .join("\n");
        let u = to_view(&parse(&body)).total_usage.unwrap();
        assert_eq!(u.output_tokens, Some(70));
        assert_eq!(u.input_tokens, None);
    }

    #[test]
    fn session_start_context_git_is_primary_over_workspace() {
        let body = [
            r#"{"type":"session.start","data":{"copilotVersion":"1.0.67","context":{"cwd":"/ctx/dir","gitRoot":"/ctx/dir","repository":"acme/demo","branch":"ctx-branch","headCommit":"ctxsha"}}}"#,
            r#"{"type":"user.message","data":{"content":"hi"}}"#,
        ]
        .join("\n");
        let mut session = parse(&body);
        // Conflicting workspace.yaml — session.start context must win.
        session.workspace = Some(crate::types::Workspace {
            git_root: Some("/ws/dir".into()),
            repository: Some("other/repo".into()),
            branch: Some("ws-branch".into()),
            revision: Some("wssha".into()),
        });
        let base = to_view(&session).base.unwrap();
        assert_eq!(base.working_dir.as_deref(), Some("/ctx/dir"));
        assert_eq!(base.vcs_branch.as_deref(), Some("ctx-branch"));
        assert_eq!(base.vcs_revision.as_deref(), Some("ctxsha"));
        assert_eq!(base.vcs_remote.as_deref(), Some("acme/demo"));
    }

    #[test]
    fn file_write_produces_mutation_with_raw_diff() {
        let view = to_view(&parse(&body()));
        let fm = view
            .turns()
            .nth(1)
            .unwrap()
            .file_mutations
            .iter()
            .find(|f| f.path == "a.rs")
            .expect("file mutation for a.rs");
        assert_eq!(fm.operation.as_deref(), Some("add"));
        assert_eq!(fm.tool_id.as_deref(), Some("c2"));
        assert!(fm.raw_diff.as_ref().unwrap().contains("+fn main() {}"));
        assert_eq!(view.files_changed, vec!["a.rs".to_string()]);
    }

    #[test]
    fn total_usage_from_shutdown() {
        let view = to_view(&parse(&body()));
        let u = view.total_usage.unwrap();
        assert_eq!(u.input_tokens, Some(1200));
        assert_eq!(u.output_tokens, Some(340));
    }

    #[test]
    fn shutdown_fills_input_cache_when_per_message_output_present() {
        // Per-message `outputTokens` sum to a session output, but the real
        // input/cache totals only exist on `session.shutdown.tokenDetails`.
        // The total must keep the summed output AND pick up input/cache from
        // the shutdown (not drop them because `summed` is Some).
        let body = [
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"content":"a","outputTokens":50}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:08.000Z","data":{"content":"b","outputTokens":20}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
            r#"{"type":"session.shutdown","timestamp":"2026-06-30T10:00:10.000Z","data":{"tokenDetails":{"input":{"tokenCount":800},"cache_read":{"tokenCount":9000},"cache_write":{"tokenCount":1500},"output":{"tokenCount":70}}}}"#,
        ]
        .join("\n");
        let u = to_view(&parse(&body)).total_usage.unwrap();
        assert_eq!(u.output_tokens, Some(70), "output from per-message sum");
        assert_eq!(u.input_tokens, Some(800), "input from shutdown");
        assert_eq!(u.cache_read_tokens, Some(9000), "cache_read from shutdown");
        assert_eq!(
            u.cache_write_tokens,
            Some(1500),
            "cache_write from shutdown"
        );
    }

    #[test]
    fn view_metadata_populated() {
        let view = to_view(&parse(&body()));
        assert_eq!(view.provider_id.as_deref(), Some("copilot"));
        assert_eq!(
            view.base.as_ref().unwrap().working_dir.as_deref(),
            Some("/tmp/proj")
        );
        let p = view.producer.as_ref().unwrap();
        assert_eq!(p.name, "copilot-cli");
        assert_eq!(p.version.as_deref(), Some("1.0.66"));
    }

    #[test]
    fn workspace_yaml_populates_base_git_context() {
        let mut session = parse(&body());
        session.workspace = Some(crate::types::Workspace {
            git_root: Some("/tmp/proj".into()),
            repository: Some("git@github.com:o/r.git".into()),
            branch: Some("feature/x".into()),
            revision: Some("abc123".into()),
        });
        let view = to_view(&session);
        let base = view.base.as_ref().unwrap();
        // cwd from session.start still wins for working_dir.
        assert_eq!(base.working_dir.as_deref(), Some("/tmp/proj"));
        assert_eq!(base.vcs_branch.as_deref(), Some("feature/x"));
        assert_eq!(base.vcs_remote.as_deref(), Some("git@github.com:o/r.git"));
        assert_eq!(base.vcs_revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn base_uses_git_root_when_no_cwd() {
        // A session with no session.start (no cwd) but a workspace git root.
        let mut session = parse("{\"type\":\"user.message\",\"data\":{\"text\":\"hi\"}}");
        session.workspace = Some(crate::types::Workspace {
            git_root: Some("/repo/root".into()),
            branch: Some("main".into()),
            ..Default::default()
        });
        let view = to_view(&session);
        let base = view.base.as_ref().unwrap();
        assert_eq!(base.working_dir.as_deref(), Some("/repo/root"));
        assert_eq!(base.vcs_branch.as_deref(), Some("main"));
    }

    #[test]
    fn subagent_becomes_delegation() {
        let body = [
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"subagent.started","timestamp":"2026-06-30T10:00:02.000Z","data":{"id":"sub-1","prompt":"do research"}}"#,
            r#"{"type":"subagent.completed","timestamp":"2026-06-30T10:00:09.000Z","data":{"id":"sub-1","result":"found it"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        let d = &view.turns().next().unwrap().delegations[0];
        assert_eq!(d.agent_id, "sub-1");
        assert_eq!(d.prompt, "do research");
        assert_eq!(d.result.as_deref(), Some("found it"));
    }

    #[test]
    fn hooks_and_skills_become_events() {
        let body = [
            r#"{"type":"hook.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"name":"fmt"}}"#,
            r#"{"type":"skill.invoked","timestamp":"2026-06-30T10:00:01.000Z","data":{"skill":"x"}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(view.events().count(), 2);
        assert_eq!(view.events().next().unwrap().event_type, "hook.start");
        assert_eq!(view.events().nth(1).unwrap().event_type, "skill.invoked");
    }

    #[test]
    fn category_classifies_common_names() {
        assert_eq!(tool_category("run_in_terminal"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("read_file"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("create_file"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("grep_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("web_fetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("totally_unknown_xyz"), None);
    }

    #[test]
    fn tool_pairing_without_ids_does_not_double_count() {
        // The reverse-engineered schema may omit a correlation id on tool
        // events; start/complete must still collapse to ONE invocation.
        let body = [
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"tool.execution_start","data":{"name":"shell","args":{"command":"ls"}}}"#,
            r#"{"type":"tool.execution_complete","data":{"name":"shell","args":{"command":"ls"},"success":true,"output":"a.rs"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        let tools = &view.turns().next().unwrap().tool_uses;
        assert_eq!(
            tools.len(),
            1,
            "id-less start/complete must not double-count"
        );
        assert_eq!(tools[0].result.as_ref().unwrap().content, "a.rs");
    }

    #[test]
    fn file_write_without_id_has_single_mutation() {
        let body = [
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"tool.execution_start","data":{"name":"create_file","args":{"path":"a.rs","content":"fn main() {}\n"}}}"#,
            r#"{"type":"tool.execution_complete","data":{"name":"create_file","args":{"path":"a.rs","content":"fn main() {}\n"},"success":true}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(view.turns().next().unwrap().tool_uses.len(), 1);
        assert_eq!(
            view.turns().next().unwrap().file_mutations.len(),
            1,
            "id-less file write must not duplicate the mutation"
        );
        assert_eq!(view.files_changed, vec!["a.rs".to_string()]);
    }

    #[test]
    fn tool_pairing_with_ids_still_works() {
        // Regression guard: explicit ids remain authoritative.
        let view = to_view(&parse(&body()));
        let shell = view
            .turns()
            .nth(1)
            .unwrap()
            .tool_uses
            .iter()
            .find(|t| t.name == "bash")
            .unwrap();
        assert_eq!(shell.result.as_ref().unwrap().content, "a.rs");
        // body() has two id-bearing tool calls: bash + create_file.
        assert_eq!(view.turns().nth(1).unwrap().tool_uses.len(), 2);
    }

    #[test]
    fn empty_message_with_usage_survives_as_a_turn() {
        // An aborted response: assistant.message with no content, no tools,
        // no reasoning — but real outputTokens. Dropping it would break
        // session-total conservation (seen crossing real sessions into
        // copilot in the cross-harness matrix).
        let body = [
            r#"{"type":"session.start","timestamp":"2026-07-01T00:00:00Z","data":{"copilotVersion":"1.0.67","context":{"cwd":"/p"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-07-01T00:00:01Z","data":{"content":"go"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-07-01T00:00:02Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-07-01T00:00:03Z","data":{"content":"","outputTokens":7}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-07-01T00:00:04Z","data":{}}"#,
        ]
        .join("\n");
        let session = crate::Session {
            id: "s-abort".into(),
            dir_path: "/tmp/s-abort".into(),
            lines: body
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            workspace: None,
        };
        let view = to_view(&session);
        let aborted = view
            .turns()
            .find(|t| matches!(t.role, Role::Assistant))
            .expect("empty assistant turn with usage must survive");
        assert_eq!(
            aborted.token_usage.as_ref().and_then(|u| u.output_tokens),
            Some(7)
        );
        assert_eq!(
            view.total_usage.as_ref().and_then(|u| u.output_tokens),
            Some(7),
            "session total must include the aborted response's tokens"
        );
    }

    /// Compact item-stream fingerprint: `turn:<text>` / `event:<type>` /
    /// `compaction:<id>`.
    fn item_shapes(view: &ConversationView) -> Vec<String> {
        view.items
            .iter()
            .map(|i| match i {
                Item::Turn(t) => format!("turn:{}", t.text),
                Item::Event(e) => format!("event:{}", e.event_type),
                Item::Compaction(c) => format!("compaction:{}", c.id),
            })
            .collect()
    }

    #[test]
    fn event_after_open_turn_content_lands_after_that_turn() {
        // The hook fires after the in-progress assistant turn already has
        // content, so it must sort after that turn and before the next one.
        let body = [
            r#"{"type":"user.message","data":{"content":"go"}}"#,
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"assistant.message","data":{"content":"working"}}"#,
            r#"{"type":"hook.start","data":{"name":"fmt"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
            r#"{"type":"user.message","data":{"content":"next"}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(
            item_shapes(&view),
            ["turn:go", "turn:working", "event:hook.start", "turn:next"]
        );
    }

    #[test]
    fn event_before_open_turn_content_lands_before_that_turn() {
        // The hook fires after turn_start but before the turn has any
        // content, so it must sort ahead of that turn.
        let body = [
            r#"{"type":"user.message","data":{"content":"go"}}"#,
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"hook.start","data":{"name":"fmt"}}"#,
            r#"{"type":"assistant.message","data":{"content":"reply"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(
            item_shapes(&view),
            ["turn:go", "event:hook.start", "turn:reply"]
        );
    }

    #[test]
    fn leading_and_trailing_events_stay_at_the_ends() {
        let body = [
            r#"{"type":"hook.start","data":{"name":"boot"}}"#,
            r#"{"type":"user.message","data":{"content":"go"}}"#,
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"assistant.message","data":{"content":"reply"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
            r#"{"type":"skill.invoked","data":{"skill":"x"}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(
            item_shapes(&view),
            [
                "event:hook.start",
                "turn:go",
                "turn:reply",
                "event:skill.invoked"
            ]
        );
    }

    #[test]
    fn event_inside_dropped_empty_turn_lands_between_neighbor_turns() {
        // The assistant turn bracketing the hook never gains content and is
        // dropped; the event must still land between the surviving turns.
        let body = [
            r#"{"type":"user.message","data":{"content":"one"}}"#,
            r#"{"type":"assistant.turn_start","data":{}}"#,
            r#"{"type":"hook.start","data":{"name":"fmt"}}"#,
            r#"{"type":"assistant.turn_end","data":{}}"#,
            r#"{"type":"user.message","data":{"content":"two"}}"#,
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(
            item_shapes(&view),
            ["turn:one", "event:hook.start", "turn:two"]
        );
    }

    fn compaction_body(success: bool) -> String {
        [
            r#"{"type":"session.start","timestamp":"2026-07-21T22:00:00.000Z","data":{"copilotVersion":"1.0.68","context":{"cwd":"/p"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-07-21T22:00:01.000Z","data":{"content":"make a file"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-07-21T22:00:02.000Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-07-21T22:00:03.000Z","data":{"content":"Done."}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-07-21T22:00:04.000Z","data":{}}"#,
            r#"{"type":"session.compaction_start","timestamp":"2026-07-21T22:00:05.000Z","data":{"systemTokens":6075,"conversationTokens":424,"toolDefinitionsTokens":7681}}"#,
            &format!(
                r#"{{"type":"session.compaction_complete","id":"c0ffee00-0000-4000-8000-000000000001","timestamp":"2026-07-21T22:00:06.000Z","data":{{"success":{success},"preCompactionTokens":427,"postCompactionTokens":663,"preCompactionMessagesLength":6,"messagesRemoved":5,"tokensRemoved":-236,"summaryContent":"<overview>made a file</overview>","checkpointNumber":1}}}}"#
            ),
            r#"{"type":"user.message","timestamp":"2026-07-21T22:00:07.000Z","data":{"content":"what did you make?"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-07-21T22:00:08.000Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-07-21T22:00:09.000Z","data":{"content":"A file."}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-07-21T22:00:10.000Z","data":{}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn successful_compaction_complete_becomes_a_boundary() {
        let view = to_view(&parse(&compaction_body(true)));
        let c = view.compactions().next().expect("one compaction");
        assert_eq!(view.compactions().count(), 1);
        assert_eq!(c.id, "compact0", "position-stable id, like turn ids");
        assert_eq!(
            c.summary.as_deref(),
            Some("<overview>made a file</overview>")
        );
        assert_eq!(c.pre_tokens, Some(427));
        assert_eq!(c.kept_from, None, "copilot compaction is wholesale");

        // Chain: pre-turns → boundary → post-turns, in item order.
        let pre_assistant = view.turns().nth(1).unwrap();
        assert_eq!(c.parent_id.as_deref(), Some(pre_assistant.id.as_str()));
        let post_user = view.turns().nth(2).unwrap();
        assert_eq!(post_user.parent_id.as_deref(), Some(c.id.as_str()));

        // The start marker stays observable as a generic event.
        let start_events = view
            .items
            .iter()
            .filter(|i| matches!(i, Item::Event(e) if e.event_type == "session.compaction_start"))
            .count();
        assert_eq!(start_events, 1);
    }

    #[test]
    fn failed_compaction_complete_stays_a_generic_event() {
        let view = to_view(&parse(&compaction_body(false)));
        assert_eq!(view.compactions().count(), 0);
        let complete_events = view
            .items
            .iter()
            .filter(
                |i| matches!(i, Item::Event(e) if e.event_type == "session.compaction_complete"),
            )
            .count();
        assert_eq!(complete_events, 1);
        // With no boundary the post-compaction user turn chains straight to
        // the previous assistant turn.
        let post_user = view.turns().nth(2).unwrap();
        let pre_assistant = view.turns().nth(1).unwrap();
        assert_eq!(
            post_user.parent_id.as_deref(),
            Some(pre_assistant.id.as_str())
        );
    }

    #[test]
    fn real_fixture_post_compaction_reprime_lands_after_the_boundary() {
        // copilot 1.0.68 writes a `system.message` re-prime AFTER
        // `session.compaction_complete` and before the next user turn. All
        // three share a turn watermark, so the item stream must keep their
        // arrival order: Turn, Event(compaction_start), Compaction,
        // Event(system.message), Turn.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-fixtures/copilot/compacted-real.jsonl");
        let lines = crate::reader::EventReader::read_lines(&path).expect("read compacted fixture");
        let session = Session {
            id: "compacted-real".to_string(),
            dir_path: path,
            lines,
            workspace: None,
        };
        let view = to_view(&session);
        let idx = view
            .items
            .iter()
            .position(|i| matches!(i, Item::Compaction(_)))
            .expect("one compaction");
        assert!(
            matches!(&view.items[idx - 2], Item::Turn(t) if t.text.contains("File contents")),
            "last pre-compaction assistant turn precedes the pair"
        );
        assert!(
            matches!(&view.items[idx - 1],
                Item::Event(e) if e.event_type == "session.compaction_start"),
            "start marker sits immediately before the boundary"
        );
        assert!(
            matches!(&view.items[idx + 1], Item::Event(e) if e.event_type == "system.message"),
            "the system.message re-prime must land AFTER the boundary, matching wire order"
        );
        assert!(
            matches!(&view.items[idx + 2],
                Item::Turn(t) if t.role == Role::User && t.text.contains("what is in notes.md")),
            "next user turn follows the re-prime"
        );
    }

    #[test]
    fn adjacent_compactions_keep_start_boundary_interleaving() {
        // Two back-to-back compactions share a watermark; the stream must
        // read E,C,E,C (each start marker with its boundary), not E,E,C,C.
        let complete = |n: u32| {
            format!(
                r#"{{"type":"session.compaction_complete","timestamp":"2026-07-21T22:00:0{n}.000Z","data":{{"success":true,"preCompactionTokens":{n},"summaryContent":"s{n}"}}}}"#
            )
        };
        let body = [
            r#"{"type":"user.message","data":{"content":"go"}}"#.to_string(),
            r#"{"type":"session.compaction_start","data":{"conversationTokens":1}}"#.to_string(),
            complete(1),
            r#"{"type":"session.compaction_start","data":{"conversationTokens":2}}"#.to_string(),
            complete(2),
            r#"{"type":"user.message","data":{"content":"next"}}"#.to_string(),
        ]
        .join("\n");
        let view = to_view(&parse(&body));
        assert_eq!(
            item_shapes(&view),
            [
                "turn:go",
                "event:session.compaction_start",
                "compaction:compact0",
                "event:session.compaction_start",
                "compaction:compact1",
                "turn:next"
            ]
        );
        // The parent chain crosses both boundaries in order.
        let second = view.compactions().nth(1).unwrap();
        assert_eq!(second.parent_id.as_deref(), Some("compact0"));
        assert_eq!(
            view.turns().nth(1).unwrap().parent_id.as_deref(),
            Some("compact1")
        );
    }
}
