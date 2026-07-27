//! Implementation of `toolpath-convo` traits for opencode sessions.
//!
//! Unlike Codex's streaming event model, opencode's parts are
//! self-contained: each [`crate::types::ToolPart`] already carries both
//! the tool input and the tool output/error in its [`ToolState`]. So the
//! mapping is mostly a direct translation per part, with minimal
//! cross-part assembly:
//!
//! 1. For each user [`Message`], emit a [`Turn`] with `role: User`
//!    whose `text` is the concatenation of the message's text parts.
//! 2. For each assistant [`Message`], emit a [`Turn`] with
//!    `role: Assistant`:
//!    - `text` ← concatenation of its `text` parts.
//!    - `thinking` ← concatenation of its `reasoning` parts.
//!    - `tool_uses` ← one [`ToolInvocation`] per `tool` part.
//!    - `token_usage` ← summed across all `step-finish` parts (each
//!      is a per-step delta). Falls back to the message-level
//!      `tokens` field if no step-finish parts exist.
//!    - `extra["opencode"]["snapshots"]` ← ordered list of snapshot
//!      SHAs from `step-start`/`step-finish`/`snapshot` parts, used
//!      by the derive layer to fetch file diffs.
//!    - `extra["opencode"]["patches"]` ← any `patch` parts (their
//!      `{hash, files}` records).
//! 3. A `compaction` part (on either a user or assistant message) emits
//!    a generic `part.compaction` event at its position in the item stream,
//!    parented on the last turn emitted before it. `derive_path` carries
//!    it through as a generic `conversation.event` step.
//! 4. Other non-turn parts land in `ConversationView.events`:
//!    `retry`, `file`, `agent`, unknown types.
//! 5. `subtask` parts are captured on the turn's `delegations`
//!    (empty-turn list — the sub-agent's own session lives under
//!    its own id, linked by `session.parent_id`).

use chrono::{TimeZone, Utc};
use serde_json::Value;
use std::collections::HashMap;

use crate::error::Result;
use crate::io::ConvoIO;
use crate::paths::PathResolver;
use crate::types::{
    AssistantMessage, CompactionPart, Message, MessageData, Part, PartData, Session,
    SessionMetadata, Tokens, ToolState, UserMessage,
};
use toolpath_convo::{
    ConversationEvent, ConversationMeta, ConversationProvider, ConversationView,
    ConvoError as ConvoTraitError, DelegatedWork, EnvironmentSnapshot, FileMutation, Item,
    ProducerInfo, Role, SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Provider for opencode sessions.
#[derive(Default)]
pub struct OpencodeConvo {
    io: ConvoIO,
}

impl OpencodeConvo {
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

    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        self.io.read_session(session_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        self.io.list_session_metadata(None)
    }

    pub fn most_recent_session(&self) -> Result<Option<Session>> {
        let metas = self.list_sessions()?;
        match metas.first() {
            Some(m) => Ok(Some(self.read_session(&m.id)?)),
            None => Ok(None),
        }
    }

    /// Read every session. Expensive on large histories.
    pub fn read_all_sessions(&self) -> Result<Vec<Session>> {
        let metas = self.list_sessions()?;
        let mut out = Vec::with_capacity(metas.len());
        for m in metas {
            match self.read_session(&m.id) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("Warning: could not read session {}: {}", m.id, e),
            }
        }
        Ok(out)
    }
}

// ── Tool classification ─────────────────────────────────────────────

/// Map an opencode tool name to toolpath's category ontology.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "read" | "list" | "view" | "ls" => Some(ToolCategory::FileRead),
        "glob" | "grep" | "search" => Some(ToolCategory::FileSearch),
        "write" | "edit" | "multiedit" | "patch" | "delete" => Some(ToolCategory::FileWrite),
        "bash" | "shell" | "exec" | "terminal" => Some(ToolCategory::Shell),
        "webfetch" | "websearch" | "web_fetch" | "web_search" | "fetch" => {
            Some(ToolCategory::Network)
        }
        "task" | "agent" | "subagent" | "spawn_agent" => Some(ToolCategory::Delegation),
        _ => {
            // MCP tools use "mcp__<server>__<tool>" convention. We don't
            // have enough info to categorize those; leave as None.
            None
        }
    }
}

/// Reverse of `tool_category`: pick opencode's native tool name for a
/// given category, disambiguating by `args` shape where needed
/// (e.g. `edit` vs `write`, `glob` vs `grep`).
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("bash"),
        ToolCategory::FileRead => Some("read"),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep"
        } else {
            "glob"
        }),
        ToolCategory::FileWrite => Some(if args.get("old_string").is_some() {
            "edit"
        } else {
            "write"
        }),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "webfetch"
        } else {
            "websearch"
        }),
        ToolCategory::Delegation => Some("task"),
    }
}

// ── Session → ConversationView ─────────────────────────────────────

/// Convert a parsed opencode [`Session`] to the provider-agnostic
/// [`ConversationView`] shape. File mutations from the snapshot git repo
/// are not populated; use [`to_view_with_resolver`] when you have one.
pub fn to_view(session: &Session) -> ConversationView {
    to_view_with_resolver(session, &PathResolver::new())
}

/// Like [`to_view`] but opens opencode's snapshot git repository via the
/// resolver and pre-resolves each turn's file mutations against the
/// snapshot pair. Falls back silently when the repo isn't present.
pub fn to_view_with_resolver(session: &Session, resolver: &PathResolver) -> ConversationView {
    Builder::new(session).build_with_resolver(resolver)
}

struct Builder<'a> {
    session: &'a Session,
    /// The ordered conversation stream — turns, events, and compaction
    /// boundaries interleaved in real order, so a compaction lands at its
    /// true position rather than after all turns.
    items: Vec<Item>,
    /// Id of the most recent turn pushed to `items`. A compaction boundary
    /// parents on this (the last turn before it), a user turn takes it as
    /// its synthesized parent (opencode user rows carry no native
    /// `parentID`), and a turn-less compaction-host message records it as
    /// its redirect target.
    last_turn_id: Option<String>,
    files_changed_order: Vec<String>,
    files_changed_seen: std::collections::HashSet<String>,
    total_usage: TokenUsage,
    total_usage_set: bool,
    /// Snapshot git repo, when one's been opened by `build_with_resolver`.
    /// Used inline by `handle_assistant_message` to compute per-turn
    /// `file_mutations` from snapshot tree↔tree diffs.
    snapshot_repo: Option<git2::Repository>,
    /// The previous assistant turn's ending snapshot SHA. Used as the
    /// `before` of the next turn's snapshot pair so intermediate state
    /// captures correctly.
    prev_snapshot_after: Option<String>,
    /// Message ids that emitted no turn (a user message whose only content
    /// was a compaction part) → the id of the last turn pushed before that
    /// message. opencode writes the boundary on a synthetic user message,
    /// and the next assistant message's `parent_id` names it — resolving
    /// through this map keeps that parent from dangling.
    msg_redirects: HashMap<String, Option<String>>,
}

impl<'a> Builder<'a> {
    fn new(session: &'a Session) -> Self {
        Self {
            session,
            items: Vec::new(),
            last_turn_id: None,
            files_changed_order: Vec::new(),
            files_changed_seen: std::collections::HashSet::new(),
            total_usage: TokenUsage::default(),
            total_usage_set: false,
            snapshot_repo: None,
            prev_snapshot_after: None,
            msg_redirects: HashMap::new(),
        }
    }

    fn push_turn(&mut self, turn: Turn) {
        self.last_turn_id = Some(turn.id.clone());
        self.items.push(Item::Turn(turn));
    }

    /// Resolve a turn's native parent message id through `msg_redirects`,
    /// so parents pointing at messages that emitted no turn land on the
    /// last turn pushed before them.
    fn resolve_parent(&self, parent: Option<String>) -> Option<String> {
        match parent {
            Some(p) => match self.msg_redirects.get(&p) {
                Some(redirect) => redirect.clone(),
                None => Some(p),
            },
            None => None,
        }
    }

    fn push_event(&mut self, event: ConversationEvent) {
        self.items.push(Item::Event(event));
    }

    /// Map an opencode `compaction` part to a generic event,
    /// parented on the last turn emitted so far.
    fn push_compaction(&mut self, part: &Part, c: &CompactionPart) {
        // Context-compaction markers ride as generic events for now; typed
        // boundary support builds on this in the compaction-provenance work.
        self.items.push(Item::Event(ConversationEvent {
            id: format!("compaction-{}", part.id),
            timestamp: millis_to_iso(part.time_created),
            parent_id: self.last_turn_id.clone(),
            event_type: "part.compaction".into(),
            data: to_data_map(&serde_json::to_value(c).unwrap_or(Value::Null)),
        }));
    }

    fn build_with_resolver(mut self, resolver: &PathResolver) -> ConversationView {
        let session_version = self.session.version.clone();
        let session_directory = self.session.directory.to_string_lossy().to_string();
        let session_project_id = self.session.project_id.clone();
        self.snapshot_repo = resolver
            .snapshot_gitdir(&session_project_id, &self.session.directory)
            .ok()
            .and_then(|gd| git2::Repository::open(gd).ok());

        let mut view = self.build();

        // Producer + base.
        view.producer = Some(ProducerInfo {
            name: "opencode".into(),
            version: Some(session_version),
        });
        view.base = Some(SessionBase {
            working_dir: Some(session_directory),
            vcs_revision: Some(session_project_id),
            vcs_branch: None,
            vcs_remote: None,
        });

        // Assistant turns keep opencode's native `parentID` chain; user
        // turns get a synthesized parent in `handle_user_message`. That
        // synthesis is idempotent: re-deriving a projected session walks
        // the same linear log and resynthesizes the same parents.

        // Refresh files_changed so it matches what landed on turns.
        let mut seen = std::collections::HashSet::new();
        let mut ordered = Vec::new();
        for turn in view.turns() {
            for fm in &turn.file_mutations {
                if seen.insert(fm.path.clone()) {
                    ordered.push(fm.path.clone());
                }
            }
        }
        view.files_changed = ordered;
        view
    }

    fn build(mut self) -> ConversationView {
        for msg in &self.session.messages {
            match &msg.data {
                MessageData::User(u) => self.handle_user_message(msg, u),
                MessageData::Assistant(a) => self.handle_assistant_message(msg, a),
                MessageData::Other => {
                    self.push_event(ConversationEvent {
                        id: format!("msg-other-{}", msg.id),
                        timestamp: millis_to_iso(msg.time_created),
                        parent_id: None,
                        event_type: "message.other".into(),
                        data: HashMap::new(),
                    });
                }
            }
        }

        ConversationView {
            id: self.session.id.clone(),
            started_at: Utc.timestamp_millis_opt(self.session.time_created).single(),
            last_activity: Utc.timestamp_millis_opt(self.session.time_updated).single(),
            items: self.items,
            total_usage: if self.total_usage_set {
                Some(self.total_usage)
            } else {
                None
            },
            provider_id: Some("opencode".into()),
            files_changed: self.files_changed_order,
            session_ids: vec![self.session.id.clone()],
            ..Default::default()
        }
    }

    fn handle_user_message(&mut self, msg: &Message, _u: &UserMessage) {
        let text = concat_text_parts(&msg.parts);

        // A compaction marker can ride on a user message (opencode writes a
        // synthetic compaction-bearing user message at the boundary). Emit
        // the boundary in place; it parents on the last turn so far.
        let mut hosted_compaction = false;
        for p in &msg.parts {
            if let PartData::Compaction(c) = &p.data {
                self.push_compaction(p, c);
                hosted_compaction = true;
            }
        }

        // A compaction-host user message with no text is synthetic — the
        // boundary event above stands in for it, so emit no turn and
        // record a redirect: later turns whose native `parent_id` names
        // this message chain onto the last turn before the boundary. Any
        // other empty-text user message (e.g. attachment-only, with parts
        // but no text) still emits a turn.
        if text.is_empty() && hosted_compaction {
            self.msg_redirects
                .insert(msg.id.clone(), self.last_turn_id.clone());
            return;
        }

        let environment = Some(EnvironmentSnapshot {
            working_dir: Some(self.session.directory.to_string_lossy().to_string()),
            vcs_branch: None,
            vcs_revision: None,
        });

        self.push_turn(Turn {
            id: msg.id.clone(),
            // opencode's native user rows carry no parentID, but the log is
            // strictly linear — synthesize the previous turn as parent so
            // the IR turn chain (which derive splicing walks) doesn't
            // break at every user message.
            parent_id: self.last_turn_id.clone(),
            group_id: None,
            role: Role::User,
            timestamp: millis_to_iso(msg.time_created),
            text,
            thinking: None,
            tool_uses: Vec::new(),
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        });
    }

    fn handle_assistant_message(&mut self, msg: &Message, a: &AssistantMessage) {
        let mut text_chunks: Vec<String> = Vec::new();
        let mut thinking_chunks: Vec<String> = Vec::new();
        let mut tool_uses: Vec<ToolInvocation> = Vec::new();
        let mut snapshots: Vec<String> = Vec::new();
        let mut delegations: Vec<DelegatedWork> = Vec::new();
        let mut step_usage = TokenUsage::default();
        let mut step_usage_set = false;
        let mut stop_reason: Option<String> = None;

        for p in &msg.parts {
            match &p.data {
                PartData::Text(t) => {
                    if !t.text.is_empty() {
                        text_chunks.push(t.text.clone());
                    }
                }
                PartData::Reasoning(r) => {
                    if !r.text.is_empty() {
                        thinking_chunks.push(r.text.clone());
                    }
                }
                PartData::Tool(tp) => {
                    tool_uses.push(to_invocation(
                        tp,
                        &mut self.files_changed_order,
                        &mut self.files_changed_seen,
                    ));
                }
                PartData::StepStart(s) => {
                    if let Some(sh) = &s.snapshot
                        && snapshots.last().is_none_or(|l| l != sh)
                    {
                        snapshots.push(sh.clone());
                    }
                }
                PartData::StepFinish(sf) => {
                    if let Some(sh) = &sf.snapshot
                        && snapshots.last().is_none_or(|l| l != sh)
                    {
                        snapshots.push(sh.clone());
                    }
                    accumulate_tokens(&mut step_usage, &sf.tokens);
                    step_usage_set = true;
                    stop_reason = Some(sf.reason.clone());
                }
                PartData::Snapshot(s) => {
                    if snapshots.last().is_none_or(|l| l != &s.snapshot) {
                        snapshots.push(s.snapshot.clone());
                    }
                }
                PartData::Patch(pp) => {
                    for f in &pp.files {
                        if self.files_changed_seen.insert(f.clone()) {
                            self.files_changed_order.push(f.clone());
                        }
                    }
                }
                PartData::Subtask(st) => {
                    delegations.push(DelegatedWork {
                        agent_id: st.agent.clone(),
                        prompt: st.prompt.clone(),
                        turns: Vec::new(),
                        result: None,
                    });
                }
                PartData::File(f) => {
                    self.push_event(ConversationEvent {
                        id: format!("file-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.file".into(),
                        data: to_data_map(&serde_json::to_value(f).unwrap_or(Value::Null)),
                    });
                }
                PartData::Agent(ag) => {
                    self.push_event(ConversationEvent {
                        id: format!("agent-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.agent".into(),
                        data: to_data_map(&serde_json::to_value(ag).unwrap_or(Value::Null)),
                    });
                }
                PartData::Retry(r) => {
                    self.push_event(ConversationEvent {
                        id: format!("retry-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.retry".into(),
                        data: to_data_map(&serde_json::to_value(r).unwrap_or(Value::Null)),
                    });
                }
                PartData::Compaction(c) => {
                    // A compaction marker on an assistant message: emit the
                    // boundary in place, parented on the turn before this
                    // one (this turn hasn't been pushed yet).
                    self.push_compaction(p, c);
                }
                PartData::Unknown => {
                    self.push_event(ConversationEvent {
                        id: format!("unknown-{}", p.id),
                        timestamp: millis_to_iso(p.time_created),
                        parent_id: Some(msg.id.clone()),
                        event_type: "part.unknown".into(),
                        data: HashMap::new(),
                    });
                }
            }
        }

        // Prefer step-summed tokens over the message-level snapshot —
        // the step deltas capture the real per-step work. Absent or
        // all-zero counters mean "spend unknown", not "a zero-cost API
        // call": decode to None, never Some(zeros) (foreign-source
        // projections write zero placeholders into required fields).
        let token_usage = if step_usage_set && !is_usage_zero(&step_usage) {
            Some(step_usage.clone())
        } else {
            let u = tokens_to_convo(&a.tokens);
            if is_usage_zero(&u) { None } else { Some(u) }
        };

        if let Some(u) = token_usage.as_ref() {
            accumulate_total(&mut self.total_usage, u);
            self.total_usage_set = true;
        }

        let environment = Some(EnvironmentSnapshot {
            working_dir: Some(a.path.cwd.to_string_lossy().to_string()),
            vcs_branch: None,
            vcs_revision: None,
        });

        // Compute `file_mutations` for this turn inline:
        //   1. If we have a snapshot repo AND a snapshot pair (prev_after,
        //      this turn's last snapshot), walk the git2 tree↔tree diff
        //      and add a FileMutation per touched file.
        //   2. For any file-write tool whose path wasn't covered by the
        //      snapshot diff, add a tool-input-derived FileMutation
        //      (catches gitignored paths and the no-repo case).
        let file_mutations = self.compute_turn_mutations(&snapshots, &tool_uses);

        self.push_turn(Turn {
            id: msg.id.clone(),
            parent_id: self.resolve_parent(if a.parent_id.is_empty() {
                None
            } else {
                Some(a.parent_id.clone())
            }),
            group_id: None,
            role: Role::Assistant,
            timestamp: millis_to_iso(msg.time_created),
            text: text_chunks.join("\n\n"),
            thinking: if thinking_chunks.is_empty() {
                None
            } else {
                Some(thinking_chunks.join("\n\n"))
            },
            tool_uses,
            model: if a.model_id.is_empty() {
                None
            } else {
                Some(a.model_id.clone())
            },
            stop_reason: stop_reason.or_else(|| a.finish.clone()),
            token_usage,
            attributed_token_usage: None,
            environment,
            delegations,
            file_mutations,
        });
    }

    fn compute_turn_mutations(
        &mut self,
        snapshots: &[String],
        tool_uses: &[ToolInvocation],
    ) -> Vec<FileMutation> {
        let mut out: Vec<FileMutation> = Vec::new();
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Snapshot diff (when repo + pair available).
        if let (Some(repo), Some(first), Some(last)) = (
            self.snapshot_repo.as_ref(),
            snapshots.first(),
            snapshots.last(),
        ) {
            let before = self
                .prev_snapshot_after
                .clone()
                .unwrap_or_else(|| first.clone());
            let after = last.clone();
            self.prev_snapshot_after = Some(after.clone());
            if before != after {
                match diff_trees(repo, &before, &after) {
                    Ok(mutations) => {
                        for fm in mutations {
                            covered.insert(fm.path.clone());
                            out.push(fm);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: snapshot diff {}..{} failed: {}",
                            &before[..before.len().min(8)],
                            &after[..after.len().min(8)],
                            e
                        );
                    }
                }
            }
        } else if let Some(last) = snapshots.last() {
            // Track even when we can't diff, so subsequent turns still
            // chain off the right `before`.
            self.prev_snapshot_after = Some(last.clone());
        }

        // Tool-input fallback for file-write tools whose paths aren't
        // already covered by a snapshot-diff mutation.
        for tu in tool_uses {
            let Some(path) = tool_input_file_path(tu) else {
                continue;
            };
            if covered.contains(&path) {
                continue;
            }
            covered.insert(path.clone());
            out.push(FileMutation {
                path,
                tool_id: Some(tu.id.clone()),
                operation: Some(tool_to_operation(&tu.name).to_string()),
                ..Default::default()
            });
        }

        out
    }
}

fn concat_text_parts(parts: &[Part]) -> String {
    let mut chunks = Vec::new();
    for p in parts {
        if let PartData::Text(t) = &p.data
            && !t.text.is_empty()
            && !t.ignored.unwrap_or(false)
        {
            chunks.push(t.text.clone());
        }
    }
    chunks.join("\n\n")
}

fn to_invocation(
    tp: &crate::types::ToolPart,
    files_changed_order: &mut Vec<String>,
    files_changed_seen: &mut std::collections::HashSet<String>,
) -> ToolInvocation {
    let input = tp.state.input().cloned().unwrap_or(Value::Null);
    let result = match &tp.state {
        ToolState::Completed(c) => Some(ToolResult {
            content: c.output.clone(),
            is_error: false,
        }),
        ToolState::Error(e) => Some(ToolResult {
            content: e.error.clone(),
            is_error: true,
        }),
        _ => None,
    };

    // Opportunistically collect files-changed from tool inputs.
    if matches!(tp.tool.as_str(), "edit" | "write" | "multiedit" | "patch")
        && let Some(path) = input
            .get("filePath")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str())
        && files_changed_seen.insert(path.to_string())
    {
        files_changed_order.push(path.to_string());
    }

    ToolInvocation {
        id: tp.call_id.clone(),
        name: tp.tool.clone(),
        input,
        result,
        category: tool_category(&tp.tool),
    }
}

fn accumulate_tokens(total: &mut TokenUsage, step: &Tokens) {
    add_u32(&mut total.input_tokens, step.input as u32);
    // opencode reports `reasoning` as a SEPARATE additive category
    // (`total == input + output + reasoning + cache`), unlike Claude/OpenAI
    // where reasoning is already inside `output`. Fold it into output_tokens
    // so the IR's `output` means "all generated tokens" consistently and the
    // session total isn't under-counted.
    add_u32(
        &mut total.output_tokens,
        (step.output + step.reasoning) as u32,
    );
    add_u32(&mut total.cache_read_tokens, step.cache.read as u32);
    add_u32(&mut total.cache_write_tokens, step.cache.write as u32);
    // Memoize the reasoning slice we just folded into output. It's
    // INFORMATIONAL (never summed into the total — output already counts
    // it); the invariant is Σ(inner) = reasoning ≤ output. Accumulates
    // across step-finish parts exactly like output does.
    add_reasoning_breakdown(total, step.reasoning as u32);
}

/// Add `reasoning` to `breakdowns["output"]["reasoning"]`, creating the
/// nested maps as needed. No-op when `reasoning` is 0 so a zero-reasoning
/// turn keeps an empty `breakdowns` map (omitted from serialization).
fn add_reasoning_breakdown(usage: &mut TokenUsage, reasoning: u32) {
    if reasoning == 0 {
        return;
    }
    let slot = usage
        .breakdowns
        .entry("output".to_string())
        .or_default()
        .entry("reasoning".to_string())
        .or_insert(0);
    *slot = slot.saturating_add(reasoning);
}

fn add_u32(slot: &mut Option<u32>, delta: u32) {
    if delta == 0 {
        return;
    }
    *slot = Some(slot.unwrap_or(0).saturating_add(delta));
}

fn tokens_to_convo(t: &Tokens) -> TokenUsage {
    let mut usage = TokenUsage {
        input_tokens: if t.input == 0 {
            None
        } else {
            Some(t.input as u32)
        },
        // Fold reasoning into output (additive in opencode — see
        // `accumulate_tokens`).
        output_tokens: if t.output + t.reasoning == 0 {
            None
        } else {
            Some((t.output + t.reasoning) as u32)
        },
        cache_read_tokens: if t.cache.read == 0 {
            None
        } else {
            Some(t.cache.read as u32)
        },
        cache_write_tokens: if t.cache.write == 0 {
            None
        } else {
            Some(t.cache.write as u32)
        },
        ..Default::default()
    };
    // Memoize the reasoning slice folded into output (no-op when 0).
    add_reasoning_breakdown(&mut usage, t.reasoning as u32);
    usage
}

fn is_usage_zero(u: &TokenUsage) -> bool {
    u.input_tokens.is_none()
        && u.output_tokens.is_none()
        && u.cache_read_tokens.is_none()
        && u.cache_write_tokens.is_none()
}

fn accumulate_total(total: &mut TokenUsage, delta: &TokenUsage) {
    if let Some(v) = delta.input_tokens {
        add_u32(&mut total.input_tokens, v);
    }
    if let Some(v) = delta.output_tokens {
        add_u32(&mut total.output_tokens, v);
    }
    if let Some(v) = delta.cache_read_tokens {
        add_u32(&mut total.cache_read_tokens, v);
    }
    if let Some(v) = delta.cache_write_tokens {
        add_u32(&mut total.cache_write_tokens, v);
    }
}

fn millis_to_iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| ms.to_string())
}

fn to_data_map(v: &Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => {
            let mut m = HashMap::new();
            m.insert("value".into(), v.clone());
            m
        }
    }
}

// ── ConversationProvider trait impl ─────────────────────────────────

impl ConversationProvider for OpencodeConvo {
    fn list_conversations(&self, _project: &str) -> toolpath_convo::Result<Vec<String>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas.into_iter().map(|m| m.id).collect())
    }

    fn load_conversation(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let s = self
            .read_session(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(to_view(&s))
    }

    fn load_metadata(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let m = self
            .io
            .read_metadata(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: m.id,
            started_at: m.started_at,
            last_activity: m.last_activity,
            message_count: m.message_count,
            file_path: Some(m.directory),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, _project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: Some(m.directory),
                predecessor: None,
                successor: None,
            })
            .collect())
    }
}

// ── Snapshot diff helpers ──────────────────────────────────────────────

fn tool_input_file_path(tu: &ToolInvocation) -> Option<String> {
    tu.input
        .get("filePath")
        .or_else(|| tu.input.get("file_path"))
        .or_else(|| tu.input.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn tool_to_operation(name: &str) -> &'static str {
    match name {
        "write" => "add",
        "edit" | "multiedit" | "patch" => "update",
        "delete" | "rm" => "delete",
        _ => "touch",
    }
}

fn diff_trees(
    repo: &git2::Repository,
    before: &str,
    after: &str,
) -> std::result::Result<Vec<FileMutation>, git2::Error> {
    let before_obj = repo.revparse_single(before)?;
    let after_obj = repo.revparse_single(after)?;
    let before_tree = before_obj.peel_to_tree()?;
    let after_tree = after_obj.peel_to_tree()?;

    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    opts.include_ignored(false);
    opts.ignore_submodules(true);
    let diff = repo.diff_tree_to_tree(Some(&before_tree), Some(&after_tree), Some(&mut opts))?;

    use std::path::PathBuf;
    let mut by_path: HashMap<PathBuf, (String, &'static str, Option<PathBuf>)> = HashMap::new();

    diff.print(git2::DiffFormat::Patch, |delta, _hunk, line| {
        let Some(new_path) = delta.new_file().path() else {
            if let Some(old) = delta.old_file().path() {
                let buf = by_path
                    .entry(old.to_path_buf())
                    .or_insert_with(|| (String::new(), "delete", None));
                append_diff_line(&mut buf.0, line);
            }
            return true;
        };
        let op = classify_delta(&delta);
        let entry = by_path.entry(new_path.to_path_buf()).or_insert_with(|| {
            (
                String::new(),
                op,
                delta.old_file().path().map(|p| p.to_path_buf()),
            )
        });
        append_diff_line(&mut entry.0, line);
        true
    })?;

    let mut out: Vec<FileMutation> = by_path
        .into_iter()
        .map(|(path, (raw_diff, op, old_path))| FileMutation {
            path: path.to_string_lossy().into_owned(),
            tool_id: None,
            operation: Some(op.to_string()),
            raw_diff: if raw_diff.is_empty() {
                None
            } else {
                Some(raw_diff)
            },
            before: None,
            after: None,
            rename_to: if op == "rename" {
                old_path.map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            },
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn classify_delta(delta: &git2::DiffDelta) -> &'static str {
    use git2::Delta;
    match delta.status() {
        Delta::Added => "add",
        Delta::Deleted => "delete",
        Delta::Modified => "update",
        Delta::Renamed => "rename",
        Delta::Copied => "copy",
        Delta::Typechange => "update",
        _ => "update",
    }
}

fn append_diff_line(buf: &mut String, line: git2::DiffLine<'_>) {
    use git2::DiffLineType;
    let prefix = match line.origin_value() {
        DiffLineType::Context => " ",
        DiffLineType::Addition => "+",
        DiffLineType::Deletion => "-",
        DiffLineType::ContextEOFNL | DiffLineType::AddEOFNL | DiffLineType::DeleteEOFNL => "",
        _ => "",
    };
    buf.push_str(prefix);
    if let Ok(s) = std::str::from_utf8(line.content()) {
        buf.push_str(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    fn setup(body_sql: &str) -> (TempDir, OpencodeConvo) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let conn = Connection::open(data.join("opencode.db")).unwrap();
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
              icon_url text, icon_color text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            {body_sql}
        "#
        ))
        .unwrap();
        drop(conn);
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_data_dir(&data);
        (temp, OpencodeConvo::with_resolver(resolver))
    }

    const BASIC_SQL: &str = r#"
        INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
          VALUES ('proj', '/tmp/proj', 1000, 3000, '[]');
        INSERT INTO session (id, project_id, slug, directory, title, version,
                             time_created, time_updated)
          VALUES ('ses_x', 'proj', 'slug', '/tmp/proj', 'T', '1.3.10', 1000, 3000);
        INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
          ('m1','ses_x',1001,1001,
           '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
          ('m2','ses_x',1002,1100,
           '{"parentID":"m1","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"modelID":"claude","providerID":"anthropic","time":{"created":1002,"completed":1100},"finish":"stop"}');
        INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
          ('p1','m1','ses_x',1001,1001,'{"type":"text","text":"make a pickle"}'),
          ('p2','m2','ses_x',1002,1002,'{"type":"step-start","snapshot":"snap_a"}'),
          ('p3','m2','ses_x',1003,1003,'{"type":"reasoning","text":"I should write main.cpp","time":{"start":1003,"end":1004}}'),
          ('p4','m2','ses_x',1005,1005,'{"type":"tool","tool":"bash","callID":"call_1","state":{"status":"completed","input":{"command":"ls"},"output":"files\n","title":"List","metadata":{"exit":0},"time":{"start":1005,"end":1006}}}'),
          ('p5','m2','ses_x',1007,1007,'{"type":"tool","tool":"write","callID":"call_2","state":{"status":"completed","input":{"filePath":"/tmp/proj/main.cpp","content":"int main(){}\n"},"output":"wrote","title":"Write","metadata":{"bytes":13},"time":{"start":1007,"end":1008}}}'),
          ('p6','m2','ses_x',1009,1009,'{"type":"text","text":"done!"}'),
          ('p7','m2','ses_x',1010,1010,'{"type":"step-finish","reason":"stop","snapshot":"snap_b","tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"cost":0.01}');
    "#;

    #[test]
    fn basic_view_shape() {
        let (_t, mgr) = setup(BASIC_SQL);
        let s = mgr.read_session("ses_x").unwrap();
        let view = to_view(&s);

        assert_eq!(view.id, "ses_x");
        assert_eq!(view.provider_id.as_deref(), Some("opencode"));
        let turns: Vec<_> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].text, "make a pickle");
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[1].text, "done!");
        assert_eq!(
            turns[1].thinking.as_deref(),
            Some("I should write main.cpp")
        );
    }

    #[test]
    fn tool_invocations_paired() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        let assistant = view.turns().nth(1).unwrap();
        assert_eq!(assistant.tool_uses.len(), 2);
        let bash = &assistant.tool_uses[0];
        assert_eq!(bash.name, "bash");
        assert_eq!(bash.category, Some(ToolCategory::Shell));
        assert_eq!(bash.result.as_ref().unwrap().content, "files\n");
        let write = &assistant.tool_uses[1];
        assert_eq!(write.name, "write");
        assert_eq!(write.category, Some(ToolCategory::FileWrite));
    }

    #[test]
    fn files_changed_from_tool_input() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        assert_eq!(view.files_changed, vec!["/tmp/proj/main.cpp".to_string()]);
    }

    #[test]
    fn step_finish_drives_token_usage() {
        let (_t, mgr) = setup(BASIC_SQL);
        let view = to_view(&mgr.read_session("ses_x").unwrap());
        let u = view.turns().nth(1).unwrap().token_usage.as_ref().unwrap();
        assert_eq!(u.input_tokens, Some(100));
        // output (20) + reasoning (5): opencode reports reasoning as a
        // separate additive category, folded into output here.
        assert_eq!(u.output_tokens, Some(25));
        assert_eq!(u.cache_read_tokens, Some(10));

        // The reasoning slice (5) is also memoized under
        // breakdowns["output"]["reasoning"] — it's the SAME number folded
        // into output, so Σ(inner) = 5 ≤ output (25).
        assert_eq!(
            u.breakdowns.get("output").and_then(|m| m.get("reasoning")),
            Some(&5u32)
        );

        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(100));
        assert_eq!(total.output_tokens, Some(25));
    }

    #[test]
    fn zero_reasoning_yields_no_breakdowns() {
        // output present but reasoning 0 → token_usage exists, but no
        // breakdowns entry (empty map, omitted from serialization).
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":10,"output":20,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"step-finish","reason":"stop","tokens":{"input":10,"output":20,"reasoning":0,"cache":{"read":0,"write":0}}}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        let u = view.turns().next().unwrap().token_usage.as_ref().unwrap();
        assert_eq!(u.output_tokens, Some(20));
        assert!(u.breakdowns.is_empty());
    }

    #[test]
    fn reasoning_accumulates_across_step_finishes() {
        // Two step-finish parts in one turn: reasoning 5 then 7 → output
        // total folds 12, and breakdowns["output"]["reasoning"] == 12.
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"step-finish","reason":"tool-calls","tokens":{"input":10,"output":20,"reasoning":5,"cache":{"read":0,"write":0}}}'),
              ('p2','m','s',2,2,'{"type":"step-finish","reason":"stop","tokens":{"input":3,"output":4,"reasoning":7,"cache":{"read":0,"write":0}}}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        let u = view.turns().next().unwrap().token_usage.as_ref().unwrap();
        // output total: (20+5) + (4+7) = 36; reasoning slice: 5+7 = 12.
        assert_eq!(u.output_tokens, Some(36));
        assert_eq!(
            u.breakdowns.get("output").and_then(|m| m.get("reasoning")),
            Some(&12u32)
        );
    }

    #[test]
    fn all_zero_usage_yields_none_and_no_breakdowns() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"step-finish","reason":"stop","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        assert!(view.turns().next().unwrap().token_usage.is_none());
    }

    #[test]
    fn tool_error_becomes_tool_result_error() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p', '/p', 1, 2, '[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"tool","tool":"bash","callID":"c","state":{"status":"error","input":{"command":"false"},"error":"exit 1","time":{"start":1,"end":2}}}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        let tool = &view.turns().next().unwrap().tool_uses[0];
        let r = tool.result.as_ref().unwrap();
        assert!(r.is_error);
        assert_eq!(r.content, "exit 1");
    }

    #[test]
    fn compaction_becomes_event() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','s',1,1,'{"role":"user","time":{"created":1},"agent":"b","model":{"providerID":"o","modelID":"m"}}'),
              ('m2','s',2,2,'{"parentID":"m1","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":2}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m1','s',1,1,'{"type":"text","text":"hi"}'),
              ('p2','m2','s',2,2,'{"type":"compaction","auto":true,"overflow":false}'),
              ('p3','m2','s',3,3,'{"type":"text","text":"done"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());

        assert!(view.events().any(|e| e.event_type == "part.compaction"));

        // In-stream position: the boundary sits between the user turn and
        // the assistant turn that hosted the part.
        assert_eq!(view.items.len(), 3);
        match &view.items[0] {
            Item::Turn(t) => assert_eq!(t.id, "m1"),
            other => panic!("expected user turn first, got {other:?}"),
        }
        match &view.items[1] {
            Item::Event(e) => {
                assert_eq!(e.event_type, "part.compaction");
                assert_eq!(e.parent_id.as_deref(), Some("m1"));
            }
            other => panic!("expected compaction event second, got {other:?}"),
        }
        match &view.items[2] {
            Item::Turn(t) => assert_eq!(t.id, "m2"),
            other => panic!("expected assistant turn third, got {other:?}"),
        }
    }

    #[test]
    fn attachment_only_user_message_still_emits_turn() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','s',1,1,'{"role":"user","time":{"created":1},"agent":"b","model":{"providerID":"o","modelID":"m"}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m1','s',1,1,'{"type":"file","mime":"image/png","url":"file:///tmp/shot.png"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        let turns: Vec<_> = view.turns().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].text, "");
    }

    #[test]
    fn compaction_host_user_message_with_empty_text_emits_no_turn() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','s',1,1,'{"role":"user","time":{"created":1},"agent":"b","model":{"providerID":"o","modelID":"m"}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m1','s',1,1,'{"type":"compaction","auto":true,"overflow":false}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        assert_eq!(view.turns().count(), 0);
        assert!(view.events().any(|e| e.event_type == "part.compaction"));
    }

    #[test]
    fn redirect_resolves_assistant_parent_through_suppressed_compaction_host() {
        // m3 is a synthetic compaction-host user message (no text). m4's
        // native parentID names m3; the redirect resolves it to m2, the
        // last turn before the boundary.
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','s',1,1,'{"role":"user","time":{"created":1},"agent":"b","model":{"providerID":"o","modelID":"m"}}'),
              ('m2','s',2,2,'{"parentID":"m1","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":2}}'),
              ('m3','s',3,3,'{"role":"user","time":{"created":3},"agent":"b","model":{"providerID":"o","modelID":"m"}}'),
              ('m4','s',4,4,'{"parentID":"m3","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":4}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m1','s',1,1,'{"type":"text","text":"hi"}'),
              ('p2','m2','s',2,2,'{"type":"text","text":"ok"}'),
              ('p3','m3','s',3,3,'{"type":"compaction","auto":true,"overflow":true}'),
              ('p4','m4','s',4,4,'{"type":"text","text":"after"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());

        assert!(view.turns().all(|t| t.id != "m3"));
        let boundary = view
            .events()
            .find(|e| e.event_type == "part.compaction")
            .expect("compaction event");
        assert_eq!(boundary.parent_id.as_deref(), Some("m2"));
        let m4 = view.turns().find(|t| t.id == "m4").expect("m4 turn");
        assert_eq!(m4.parent_id.as_deref(), Some("m2"));
    }

    #[test]
    fn user_turn_parent_synthesized_from_last_turn() {
        // User messages carry no parentID on the wire; the provider chains
        // them onto the last turn pushed. The first user turn stays a root.
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','s',1,1,'{"role":"user","time":{"created":1},"agent":"b","model":{"providerID":"o","modelID":"m"}}'),
              ('m2','s',2,2,'{"parentID":"m1","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":2}}'),
              ('m3','s',3,3,'{"role":"user","time":{"created":3},"agent":"b","model":{"providerID":"o","modelID":"m"}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m1','s',1,1,'{"type":"text","text":"hi"}'),
              ('p2','m2','s',2,2,'{"type":"text","text":"ok"}'),
              ('p3','m3','s',3,3,'{"type":"text","text":"next"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());

        let m1 = view.turns().find(|t| t.id == "m1").expect("m1 turn");
        assert_eq!(m1.parent_id, None, "first user turn is a root");
        let m3 = view.turns().find(|t| t.id == "m3").expect("m3 turn");
        assert_eq!(
            m3.parent_id.as_deref(),
            Some("m2"),
            "later user turn chains onto the last turn pushed"
        );
    }

    #[test]
    fn unknown_part_type_becomes_event() {
        let body = r#"
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes) VALUES ('p','/p',1,2,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('s','p','slug','/p','T','1.0.0',1,2);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m','s',1,1,'{"parentID":"","role":"assistant","mode":"b","agent":"b","path":{"cwd":"/p","root":"/p"},"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1}}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1','m','s',1,1,'{"type":"future-thing","foo":"bar"}');
        "#;
        let (_t, mgr) = setup(body);
        let view = to_view(&mgr.read_session("s").unwrap());
        assert!(view.events().any(|e| e.event_type == "part.unknown"));
    }

    #[test]
    fn tool_category_mapping() {
        assert_eq!(tool_category("bash"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("edit"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("write"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("read"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("grep"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("webfetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("task"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category("mcp__x__y"), None);
    }

    #[test]
    fn provider_trait_list_and_load() {
        let (_t, mgr) = setup(BASIC_SQL);
        let ids = ConversationProvider::list_conversations(&mgr, "").unwrap();
        assert_eq!(ids, vec!["ses_x".to_string()]);
        let v = ConversationProvider::load_conversation(&mgr, "", "ses_x").unwrap();
        assert_eq!(v.turns().count(), 2);
    }
}
