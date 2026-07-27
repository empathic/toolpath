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
    Compaction, CompactionTrigger, ConversationMeta, ConversationProvider, ConversationView,
    ConvoError, DelegatedWork, EnvironmentSnapshot, Item, Role, TokenUsage, ToolCategory,
    ToolInvocation, ToolResult, Turn,
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

    // An all-zero usage block is a placeholder, not a measurement —
    // Claude stamps one on synthetic entries (API errors) that consumed
    // nothing. The convention (matching pi/opencode) decodes it as `None`
    // rather than stamping zero-filled counters onto a step.
    let token_usage = msg
        .usage
        .as_ref()
        .map(|u| TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_write_tokens: u.cache_creation_input_tokens,
            ..Default::default()
        })
        .filter(|u| {
            [
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens,
            ]
            .iter()
            .any(|v| v.unwrap_or(0) > 0)
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
        // The API message ID (`msg_…`). Claude Code writes one JSONL line
        // per content block, so several turns can share one group_id —
        // and each repeats the message-level `usage`. Downstream accounting
        // (sum_usage, derive_path) counts a message group once.
        group_id: msg.id.clone(),
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

/// Mutable accessor for the turn inside an [`Item`], if it is one.
fn item_turn_mut(item: &mut Item) -> Option<&mut Turn> {
    match item {
        Item::Turn(t) => Some(t),
        _ => None,
    }
}

/// Merge a tool-result-only message into the turns already pushed onto
/// `items`. Equivalent to [`merge_tool_results`] but operating on the
/// interleaved item stream — non-turn items (events, compaction) are skipped.
fn merge_tool_results_into_items(items: &mut [Item], msg: &Message) -> bool {
    let mut turns: Vec<&mut Turn> = items.iter_mut().filter_map(item_turn_mut).collect();
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

/// Returns true if this entry is Claude's inline compaction boundary marker.
///
/// Claude writes the boundary either as a top-level `type: "compact_boundary"`
/// entry or as `type: "system"` with `subtype: "compact_boundary"`. The
/// `subtype` field isn't in [`ConversationEntry`]'s typed fields, so it lands
/// in `extra`.
fn is_compact_boundary(entry: &ConversationEntry) -> bool {
    entry.entry_type == "compact_boundary"
        || entry
            .extra
            .get("subtype")
            .and_then(|v| v.as_str())
            .map(|s| s == "compact_boundary")
            .unwrap_or(false)
}

/// Returns true if this entry is the synthetic compaction summary that Claude
/// writes immediately after a boundary (`isCompactSummary: true`).
fn is_compact_summary(entry: &ConversationEntry) -> bool {
    entry
        .extra
        .get("isCompactSummary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Build a [`Compaction`] from Claude's boundary marker and (optionally) the
/// synthetic summary that follows it. Returns the boundary plus its native
/// preserved tail (`compactMetadata.preservedMessages.uuids`), which the
/// caller feeds to [`kept_anchor`] once the boundary's parent is final.
///
/// All the boundary's compaction-specific data lives in `entry.extra`
/// (`logicalParentUuid`, `compactMetadata.{trigger,preTokens,preservedMessages}`).
/// `summary` comes from the following `isCompactSummary` entry's message text.
///
/// Only the marked tail counts as kept: the block Claude re-emits just
/// before the boundary is deduped away by [`conversation_to_view`]'s
/// duplicate-uuid stripping and is deliberately not part of the kept
/// provenance (the compaction contract is `kept_from` alone).
fn compaction_from_boundary(
    boundary: &ConversationEntry,
    summary: Option<String>,
) -> (Compaction, Vec<String>) {
    let extra = &boundary.extra;

    // The pre-compaction message the boundary logically continues from.
    // `parentUuid` is always null on the boundary, so use logicalParentUuid.
    let parent_id = extra
        .get("logicalParentUuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let meta = extra.get("compactMetadata");

    let trigger = meta
        .and_then(|m| m.get("trigger"))
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "auto" => Some(CompactionTrigger::Auto),
            "manual" => Some(CompactionTrigger::Manual),
            _ => None,
        });

    let pre_tokens = meta
        .and_then(|m| m.get("preTokens"))
        .and_then(|v| v.as_u64());

    let preserved_uuids: Vec<String> = meta
        .and_then(|m| m.get("preservedMessages"))
        .and_then(|p| p.get("uuids"))
        .and_then(|v| v.as_array())
        .map(|uuids| {
            uuids
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let compaction = Compaction {
        id: boundary.uuid.clone(),
        parent_id,
        timestamp: boundary.timestamp.clone(),
        trigger,
        summary,
        pre_tokens,
        kept_from: None,
    };
    (compaction, preserved_uuids)
}

/// Compute a boundary's [`Compaction::kept_from`] anchor from its native
/// preserved tail: walk the parent chain backward from `parent_id` through
/// the already-emitted turns while each id is in `preserved`; the deepest
/// such turn is the anchor. `None` when the boundary's own parent didn't
/// survive — the preserved set doesn't form a contiguous tail ending at
/// the boundary, so at view granularity the compaction is wholesale.
fn kept_anchor(items: &[Item], parent_id: Option<&str>, preserved: &[String]) -> Option<String> {
    let preserved: std::collections::HashSet<&str> = preserved.iter().map(String::as_str).collect();
    let turns: HashMap<&str, &Turn> = items
        .iter()
        .filter_map(|i| match i {
            Item::Turn(t) => Some((t.id.as_str(), t)),
            _ => None,
        })
        .collect();
    let mut anchor: Option<String> = None;
    let mut visited = std::collections::HashSet::new();
    let mut cur = parent_id;
    while let Some(id) = cur {
        if !preserved.contains(id) || !visited.insert(id) {
            break;
        }
        let Some(turn) = turns.get(id) else { break };
        anchor = Some(id.to_string());
        cur = turn.parent_id.as_deref();
    }
    anchor
}

/// Convert a full conversation to a view with cross-entry tool result assembly.
///
/// Tool-result-only user entries are absorbed into the preceding assistant
/// turn's `ToolInvocation.result` fields rather than emitted as separate turns.
///
/// Compaction boundaries are detected and emitted as [`Item::Compaction`] at
/// their position in the ordered item stream: the boundary's `compactMetadata`
/// becomes the `Compaction`, and the immediately-following synthetic summary
/// entry is folded into `Compaction.summary` rather than surfaced as a turn.
fn conversation_to_view(convo: &Conversation) -> ConversationView {
    // Items are built in source order so a compaction boundary lands at its
    // true position between the turns it separates. Preamble events come
    // first — they precede all entries in the file.
    let mut items: Vec<Item> = Vec::new();

    // Headerless preamble lines (ai-title, last-prompt, queue-operation,
    // permission-mode, file-history-snapshot, etc.) become events so they
    // round-trip back to JSONL.
    for (idx, raw) in convo.preamble.iter().enumerate() {
        items.push(Item::Event(preamble_to_event(idx, raw)));
    }

    // Map from "absorbed-or-skipped entry UUID" → "the previous
    // turn-or-compaction-bearing entry's UUID". Used so that a later turn
    // whose wire parentUuid points at an absorbed entry (a tool-result-only
    // entry, or the folded compaction summary) gets a `parent_id` that still
    // maps onto a real Item — keeping the IR's chain intact for `derive_path`.
    let mut parent_rewrites: HashMap<String, String> = HashMap::new();
    // The UUID of the last turn or compaction emitted into `items`, used to
    // rewrite parents of subsequently absorbed entries.
    let mut last_anchor_uuid: Option<String> = None;

    // Duplicate-uuid stripping: a compacted session can re-emit earlier
    // entries with their original uuids, re-parented into a synthetic
    // linear chain, in the run just before the boundary — observed in long
    // `[1m]`-context 2.1.x sessions (session-chains.md §Re-emitted messages
    // with duplicate UUIDs). We keep only the FIRST occurrence of each
    // uuid: the original carries the true lineage, and a re-emission is a
    // context-window artifact, not provenance. Stripping must happen here,
    // before `derive_path` — its dedup skips byte-identical replays, but
    // the group-total token stamping below (`canonicalize_message_usage`)
    // can make a replayed copy differ from its original and survive as a
    // renamed step.
    let mut seen_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();

    let entries = &convo.entries;
    let mut i = 0;
    while i < entries.len() {
        let entry = &entries[i];

        // Strip re-emitted entries: any non-boundary entry whose uuid already
        // appeared earlier in this conversation. Boundary entries are exempt
        // so every compaction marker survives into the item stream — a
        // continuation file can repeat its parent's boundary verbatim
        // (session-chains.md §Duplicate compact_boundary), and a
        // byte-identical copy collapses later, in `derive_path`.
        if !is_compact_boundary(entry)
            && !entry.uuid.is_empty()
            && !seen_uuids.insert(entry.uuid.clone())
        {
            i += 1;
            continue;
        }

        // Compaction boundary: emit one Item::Compaction at this position,
        // folding the immediately-following synthetic summary entry (if any)
        // into Compaction.summary rather than surfacing it as a turn.
        if is_compact_boundary(entry) {
            let summary = entries.get(i + 1).filter(|next| is_compact_summary(next));
            let summary_text = summary.map(|s| s.text());
            let (compaction, preserved) = compaction_from_boundary(entry, summary_text);
            seen_uuids.insert(entry.uuid.clone());
            if let Some(s) = summary {
                seen_uuids.insert(s.uuid.clone());
            }
            // Rewire the compaction's logical parent through any prior
            // absorption so it lands on a real Item in the derived DAG.
            let mut compaction = compaction;
            if let Some(pid) = compaction.parent_id.as_ref()
                && let Some(real) = parent_rewrites.get(pid)
            {
                compaction.parent_id = Some(real.clone());
            }
            compaction.kept_from = kept_anchor(&items, compaction.parent_id.as_deref(), &preserved);
            let boundary_uuid = compaction.id.clone();
            items.push(Item::Compaction(compaction));
            // Later turns whose wire parentUuid points at the folded summary
            // chain through the compaction (the boundary itself is a real
            // Item, so parents that point at it need no rewrite).
            if let Some(s) = summary {
                parent_rewrites.insert(s.uuid.clone(), boundary_uuid.clone());
                i += 1; // consume the folded summary entry
            }
            last_anchor_uuid = Some(boundary_uuid);
            i += 1;
            continue;
        }

        let Some(msg) = &entry.message else {
            // Message-less entries (attachments, snapshots) survive as
            // events so the projector can re-emit them.
            items.push(Item::Event(entry_to_event(entry)));
            if let Some(prev) = &last_anchor_uuid {
                parent_rewrites.insert(entry.uuid.clone(), prev.clone());
            }
            i += 1;
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
            merge_tool_results_into_items(&mut items, msg);
            if let Some(prev) = &last_anchor_uuid {
                parent_rewrites.insert(entry.uuid.clone(), prev.clone());
            }
            i += 1;
            continue;
        }

        let mut turn = message_to_turn(entry, msg);
        if let Some(pid) = turn.parent_id.as_ref()
            && let Some(real) = parent_rewrites.get(pid)
        {
            turn.parent_id = Some(real.clone());
        }
        last_anchor_uuid = Some(turn.id.clone());
        items.push(Item::Turn(turn));
        i += 1;
    }

    let mut turn_refs: Vec<&mut Turn> = items.iter_mut().filter_map(item_turn_mut).collect();
    canonicalize_message_usage(&mut turn_refs);
    drop(turn_refs);

    // Re-derive delegation results now that tool results are merged
    for turn in items.iter_mut().filter_map(item_turn_mut) {
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

    let total_usage = sum_usage(items.iter().filter_map(Item::as_turn));
    let files_changed = extract_files_changed(items.iter().filter_map(Item::as_turn));

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
        items,
        total_usage,
        provider_id: Some("claude-code".into()),
        files_changed,
        session_ids: vec![],
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
/// For each `group_id` this sets `token_usage` on the group's
/// **last-occurring** turn to the field-wise **maximum** across the group (the
/// message total — never under-counts whatever the stream order) and clears it
/// from the others, so summing `token_usage` over turns yields session totals.
///
/// Grouping is by `group_id` across the whole sequence, not by consecutive run:
/// a single message's turns can be interrupted by an unrelated turn (e.g. a
/// `<subagent_notification>` user message lands between two assistant turns of
/// the same Codex round). Collapsing per run would leave the message total on
/// two turns — once per run — double-counting it. Keying on `group_id` lands it
/// exactly once.
fn canonicalize_message_usage(turns: &mut [&mut Turn]) {
    // First pass: per group_id, the field-wise max usage and the index of the
    // group's last-occurring turn.
    let mut group_total: HashMap<String, TokenUsage> = HashMap::new();
    let mut group_last_idx: HashMap<String, usize> = HashMap::new();
    for (idx, t) in turns.iter().enumerate() {
        let Some(mid) = t.group_id.clone() else {
            continue;
        };
        group_last_idx.insert(mid.clone(), idx);
        if let Some(u) = &t.token_usage {
            group_total
                .entry(mid)
                .and_modify(|acc| *acc = max_usage(acc, u))
                .or_insert_with(|| u.clone());
        }
    }

    // Second pass: clear usage off every grouped turn, then stamp each
    // group's total back onto its last-occurring turn.
    for t in turns.iter_mut() {
        if t.group_id.is_some() {
            t.token_usage = None;
        }
    }
    for (mid, total) in group_total {
        if let Some(&idx) = group_last_idx.get(&mid) {
            turns[idx].token_usage = Some(total);
        }
    }
}

/// Sum token usage across all turns.
fn sum_usage<'a>(turns: impl IntoIterator<Item = &'a Turn>) -> Option<TokenUsage> {
    let turns: Vec<&Turn> = turns.into_iter().collect();

    // A message's usage repeats across every turn split from it; count it
    // once, on the group's last-occurring turn. Key on `group_id` rather than
    // adjacency so an interrupted group (a turn of another group landing in
    // the middle) still counts once.
    let mut group_last_idx: HashMap<&str, usize> = HashMap::new();
    for (idx, turn) in turns.iter().enumerate() {
        if let Some(mid) = &turn.group_id {
            group_last_idx.insert(mid.as_str(), idx);
        }
    }

    let mut total = TokenUsage::default();
    let mut any = false;
    for (idx, turn) in turns.iter().enumerate() {
        if let Some(mid) = &turn.group_id
            && group_last_idx.get(mid.as_str()) != Some(&idx)
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
fn extract_files_changed<'a>(turns: impl IntoIterator<Item = &'a Turn>) -> Vec<String> {
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
        let mut refs: Vec<&mut Turn> = turns.iter_mut().collect();
        canonicalize_message_usage(&mut refs);

        assert!(turns[0].token_usage.is_none(), "total only on final turn");
        assert_eq!(
            turns[1].token_usage.as_ref().unwrap().output_tokens,
            Some(164)
        );
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
        let mut turns = [grp_turn("t1", "msg_A", 164), grp_turn("t2", "msg_A", 55)];
        let mut refs: Vec<&mut Turn> = turns.iter_mut().collect();
        canonicalize_message_usage(&mut refs);

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
        let mut refs: Vec<&mut Turn> = turns.iter_mut().collect();
        canonicalize_message_usage(&mut refs);

        assert!(turns[0].token_usage.is_none());
        assert!(turns[1].token_usage.is_none());
        assert_eq!(
            turns[2].token_usage.as_ref().unwrap().output_tokens,
            Some(997)
        );
        for t in &turns {
            assert!(t.attributed_token_usage.is_none());
        }
    }

    #[test]
    fn canonicalize_groups_across_an_interrupting_turn() {
        // A message group can be interrupted by an unrelated turn (e.g. a
        // `<subagent_notification>` user turn lands between two assistant
        // turns of the same Codex round, both stamped with the group total).
        // Grouping must key on `group_id`, not adjacency: the total lands on
        // the group's LAST-occurring turn ONCE — collapsing per consecutive
        // run would leave it on two turns, double-counting.
        let mut t1 = grp_turn("t1", "msg_A", 997);
        let mut interrupt = message_turn_stub("u1");
        interrupt.role = Role::User;
        interrupt.group_id = None;
        let mut t2 = grp_turn("t2", "msg_A", 997);

        {
            let mut turns = [&mut t1, &mut interrupt, &mut t2];
            canonicalize_message_usage(&mut turns);
        }

        assert!(t1.token_usage.is_none(), "earlier group turn cleared");
        assert!(interrupt.token_usage.is_none(), "ungrouped turn untouched");
        assert_eq!(
            t2.token_usage.as_ref().unwrap().output_tokens,
            Some(997),
            "total lands once on the group's last-occurring turn"
        );

        // And the session sum counts the group exactly once.
        let total = sum_usage([&t1, &interrupt, &t2]).expect("total");
        assert_eq!(total.output_tokens, Some(997));
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 5);
        assert!(turns[0].group_id.is_none(), "user lines carry no ID");
        for turn in &turns[1..=3] {
            assert_eq!(turn.group_id.as_deref(), Some("msg_A"));
        }
        assert_eq!(turns[4].group_id.as_deref(), Some("msg_B"));
    }

    #[test]
    fn test_view_usage_is_canonical_total_on_group_final_turn() {
        // IR contract: `Turn.token_usage` always means "the message's
        // total" and appears only on the message's final turn. The wire
        // repeats the total on every line of a split; the view must not.
        let (_temp, provider) = setup_split_message_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-2")
            .unwrap();

        let turns: Vec<&Turn> = view.turns().collect();
        assert!(turns[1].token_usage.is_none());
        assert!(turns[2].token_usage.is_none());
        assert_eq!(
            turns[3].token_usage.as_ref().unwrap().output_tokens,
            Some(997)
        );
        assert_eq!(
            turns[4].token_usage.as_ref().unwrap().output_tokens,
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
        let turns: Vec<&Turn> = view.turns().collect();
        // 7 entries collapse to 5 turns (2 tool-result-only entries absorbed)
        assert_eq!(turns.len(), 5);

        // Turn 0: user "Fix the bug"
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].text, "Fix the bug");
        assert!(turns[0].parent_id.is_none());

        // Turn 1: assistant with tool use + assembled result
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[1].text, "I'll fix that.");
        assert_eq!(turns[1].thinking.as_deref(), Some("The bug is in auth"));
        assert_eq!(turns[1].tool_uses.len(), 1);
        assert_eq!(turns[1].tool_uses[0].name, "Read");
        assert_eq!(turns[1].tool_uses[0].id, "t1");
        // Key assertion: result is populated from the next entry
        let result = turns[1].tool_uses[0].result.as_ref().unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("fn main()"));
        assert_eq!(turns[1].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(turns[1].stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(turns[1].parent_id.as_deref(), Some("uuid-1"));

        // Token usage
        let usage = turns[1].token_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));

        // Turn 2: second assistant with tool use + assembled result
        assert_eq!(turns[2].role, Role::Assistant);
        assert_eq!(turns[2].text, "I see the issue. Let me fix it.");
        assert_eq!(turns[2].tool_uses[0].name, "Edit");
        let result2 = turns[2].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(result2.content, "File written successfully");

        // Turn 3: final assistant (no tools)
        assert_eq!(turns[3].role, Role::Assistant);
        assert_eq!(turns[3].text, "Done! The bug is fixed.");
        assert!(turns[3].tool_uses.is_empty());

        // Turn 4: user "Thanks!"
        assert_eq!(turns[4].role, Role::User);
        assert_eq!(turns[4].text, "Thanks!");
    }

    #[test]
    fn test_no_phantom_empty_turns() {
        let (_temp, provider) = setup_provider();
        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-1")
            .unwrap();

        // No turns should have empty text with User role (phantom turns)
        for turn in view.turns() {
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2); // user + assistant (tool-result absorbed)
        let result = turns[1].tool_uses[0].result.as_ref().unwrap();
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].tool_uses.len(), 2);

        let r1 = turns[1].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(r1.content, "file a contents");

        let r2 = turns[1].tool_uses[1].result.as_ref().unwrap();
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].text, "Hello");
        assert_eq!(turns[1].text, "Hi there!");
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert!(turns[1].tool_uses[0].result.is_none());
    }

    fn item_ids(view: &ConversationView) -> Vec<&str> {
        view.items
            .iter()
            .map(|item| match item {
                Item::Turn(t) => t.id.as_str(),
                Item::Event(e) => e.id.as_str(),
                Item::Compaction(c) => c.id.as_str(),
            })
            .collect()
    }

    #[test]
    fn test_replayed_duplicate_uuids_are_stripped() {
        // The compaction replay shape: before the boundary, earlier
        // tool_use/tool_result entries are re-emitted with their original
        // uuids. Only the first occurrence of each uuid may reach the item
        // stream — a surviving replay would duplicate a turn id.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let assistant = r#"{"uuid":"u2","type":"assistant","parentUuid":"u1","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading..."},{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a.rs"}}],"stop_reason":"tool_use"}}"#;
        let carrier = r#"{"uuid":"u3","type":"user","parentUuid":"u2","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file a contents","is_error":false}]}}"#;
        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Read a file"}}"#,
            assistant,
            carrier,
            assistant,
            carrier,
            r#"{"uuid":"cb-1","type":"compact_boundary","parentUuid":null,"logicalParentUuid":"u3","timestamp":"2024-01-01T00:00:03Z","compactMetadata":{"trigger":"auto","preTokens":180000}}"#,
            r#"{"uuid":"u4","type":"user","parentUuid":"cb-1","timestamp":"2024-01-01T00:00:04Z","message":{"role":"user","content":"Keep going"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        let turn_ids: Vec<&str> = view.turns().map(|t| t.id.as_str()).collect();
        assert_eq!(
            turn_ids,
            vec!["u1", "u2", "u4"],
            "replayed u2 must be stripped, carriers absorbed"
        );

        let ids = item_ids(&view);
        let mut deduped = ids.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), ids.len(), "duplicate item ids: {ids:?}");

        let result = view
            .turns()
            .find(|t| t.id == "u2")
            .and_then(|t| t.tool_uses[0].result.as_ref())
            .expect("tool result assembled");
        assert_eq!(result.content, "file a contents");
    }

    #[test]
    fn test_compact_boundary_is_exempt_from_uuid_dedup() {
        // A continuation file can repeat its parent's compact_boundary
        // verbatim. Boundary entries bypass the duplicate-uuid strip, so
        // both copies survive to the item stream (a byte-identical copy
        // collapses later, in derive_path).
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let boundary = r#"{"uuid":"cb-1","type":"compact_boundary","parentUuid":null,"logicalParentUuid":"u1","timestamp":"2024-01-01T00:00:01Z","compactMetadata":{"trigger":"auto","preTokens":180000}}"#;
        let entries = [
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            boundary,
            boundary,
            r#"{"uuid":"u2","type":"user","parentUuid":"cb-1","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":"After compaction"}}"#,
        ];
        fs::write(project_dir.join("s1.jsonl"), entries.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let provider = ClaudeConvo::with_resolver(resolver);
        let view =
            ConversationProvider::load_conversation(&provider, "/test/project", "s1").unwrap();

        assert_eq!(item_ids(&view), vec!["u1", "cb-1", "cb-1", "u2"]);
        assert_eq!(
            view.compactions().count(),
            2,
            "both boundary copies survive to_view"
        );
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
        assert_eq!(view.turns().count(), 5);
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

        let turns: Vec<&Turn> = view.turns().collect();
        // Turn 1 (assistant) has a Read tool
        assert_eq!(turns[1].tool_uses[0].category, Some(ToolCategory::FileRead));
        // Turn 2 (assistant) has an Edit tool
        assert_eq!(
            turns[2].tool_uses[0].category,
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

        let turns: Vec<&Turn> = view.turns().collect();
        // User turn has environment (entry has cwd and gitBranch)
        let env = turns[0].environment.as_ref().unwrap();
        assert_eq!(env.working_dir.as_deref(), Some("/project/path"));
        assert_eq!(env.vcs_branch.as_deref(), Some("feat/auth"));
        assert!(env.vcs_revision.is_none());

        // Assistant turn has no environment (entry has no cwd/gitBranch)
        assert!(turns[1].environment.is_none());
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

        let usage = view.turns().nth(1).unwrap().token_usage.as_ref().unwrap();
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
        let turn1 = view.turns().nth(1).unwrap();
        assert_eq!(turn1.delegations.len(), 1);
        let d = &turn1.delegations[0];
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
        for turn in view.turns() {
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
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].text, "Fix the bug");
        assert_eq!(turns[1].text, "I'll fix that.");
        assert_eq!(turns[2].text, "What about the tests?");
        assert_eq!(turns[3].text, "Tests pass now.");

        // Session IDs should be set
        assert_eq!(view.session_ids, vec!["session-a", "session-b"]);
    }

    #[test]
    fn test_load_conversation_skips_bridge_entries() {
        let (_temp, provider) = setup_chained_provider();

        let view = ConversationProvider::load_conversation(&provider, "/test/project", "session-a")
            .unwrap();

        // Bridge entry text "Continue the fix" should NOT appear
        for turn in view.turns() {
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

        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].text, "Hello");
        assert_eq!(turns[1].text, "Hi there!");
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
