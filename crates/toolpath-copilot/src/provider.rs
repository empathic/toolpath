//! Build a provider-agnostic [`ConversationView`] from a Copilot [`Session`].
//!
//! ⚠️ The mapping below follows the *inferred* `events.jsonl` semantics in
//! `docs/agents/formats/copilot-cli/events.md`. Tool-name classification and
//! file-mutation extraction are best-effort and should be tightened once a
//! real session is captured.

use crate::io::ConvoIO;
use crate::paths::PathResolver;
use crate::types::{CopilotEvent, Session};
use serde_json::Value;
use std::collections::HashMap;
use toolpath_convo::{
    ConversationEvent, ConversationView, DelegatedWork, FileMutation, ProducerInfo, Role,
    SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
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
        "write" | "write_file" | "writefile" | "create" | "create_file" | "edit" | "edit_file"
        | "apply_patch" | "patch" | "str_replace" | "str_replace_editor" | "replace"
        | "replace_string_in_file" | "insert" | "delete_file" => {
            return Some(ToolCategory::FileWrite);
        }
        "glob" | "list" | "list_dir" | "list_directory" | "ls" | "find" | "find_files"
        | "grep" | "search" | "ripgrep" | "rg" | "file_search" | "grep_search"
        | "semantic_search" | "codebase_search" => return Some(ToolCategory::FileSearch),
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
    } else if n.contains("write") || n.contains("edit") || n.contains("patch") || n.contains("replace")
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

/// Convert a parsed Copilot [`Session`] into a [`ConversationView`].
pub fn to_view(session: &Session) -> ConversationView {
    let default_model = session.start_model();
    let cwd = session.cwd();

    let mut turns: Vec<Turn> = Vec::new();
    let mut current: Option<Turn> = None;
    let mut events: Vec<ConversationEvent> = Vec::new();
    let mut total_usage: Option<TokenUsage> = None;
    let mut seq: usize = 0;

    for (i, line) in session.lines.iter().enumerate() {
        let ts = line.timestamp.clone().unwrap_or_default();
        match line.event() {
            CopilotEvent::SessionStart(_) => {}
            CopilotEvent::SessionShutdown(s) => {
                if s.input_tokens.is_some() || s.output_tokens.is_some() {
                    total_usage = Some(TokenUsage {
                        input_tokens: s.input_tokens,
                        output_tokens: s.output_tokens,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
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
                let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                append_text(&mut cur.text, &m.text);
                if cur.model.is_none() {
                    cur.model = m.model.or_else(|| default_model.clone());
                }
            }
            CopilotEvent::AssistantTurnEnd => {
                flush(&mut turns, &mut current);
            }
            CopilotEvent::ToolStart(te) => {
                let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                seq += 1;
                let tool_id = te.id.clone().unwrap_or_else(|| format!("tool{seq}"));
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
                let result = ToolResult {
                    content: te.output.clone().unwrap_or_default(),
                    is_error: te.success == Some(false),
                };
                if !backfill_tool_result(&mut current, &mut turns, te.id.as_deref(), result.clone())
                {
                    // No matching start seen — synthesize a carrier invocation.
                    let cur = ensure_assistant(&mut current, &mut seq, &default_model, &ts);
                    seq += 1;
                    let tool_id = te.id.clone().unwrap_or_else(|| format!("tool{seq}"));
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
                seq += 1;
                cur.delegations.push(DelegatedWork {
                    agent_id: s.id.clone().unwrap_or_else(|| format!("sub{seq}")),
                    prompt: s.prompt.unwrap_or_default(),
                    turns: Vec::new(),
                    result: s.result,
                });
            }
            CopilotEvent::SubagentCompleted(s) => {
                backfill_delegation_result(&mut current, &mut turns, s.id.as_deref(), s.result);
            }
            CopilotEvent::SkillInvoked(p) => {
                events.push(make_event(i, "skill.invoked", &ts, p));
            }
            CopilotEvent::Hook { kind, payload } => {
                events.push(make_event(i, &kind, &ts, payload));
            }
            CopilotEvent::Abort(p) => events.push(make_event(i, "abort", &ts, p)),
            CopilotEvent::CompactionComplete(p) => {
                events.push(make_event(i, "session.compaction_complete", &ts, p));
            }
            CopilotEvent::SessionOther { kind, payload } => {
                events.push(make_event(i, &kind, &ts, payload));
            }
            CopilotEvent::Unknown { kind, payload } => {
                events.push(make_event(i, &kind, &ts, payload));
            }
        }
    }
    flush(&mut turns, &mut current);

    // Files changed, in first-touch order, deduped.
    let mut files_changed: Vec<String> = Vec::new();
    for t in &turns {
        for fm in &t.file_mutations {
            if !files_changed.contains(&fm.path) {
                files_changed.push(fm.path.clone());
            }
        }
    }

    // Base: working dir from `session.start` cwd (falling back to the
    // workspace's git root), with git branch/revision/remote from
    // `workspace.yaml` when available.
    let ws = session.workspace.as_ref();
    let working_dir = cwd
        .clone()
        .or_else(|| ws.and_then(|w| w.git_root.clone()));
    let has_vcs = ws.map(|w| !w.is_empty()).unwrap_or(false);
    let base = if working_dir.is_some() || has_vcs {
        Some(SessionBase {
            working_dir,
            vcs_revision: ws.and_then(|w| w.revision.clone()),
            vcs_branch: ws.and_then(|w| w.branch.clone()),
            vcs_remote: ws.and_then(|w| w.repository.clone()),
        })
    } else {
        None
    };

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
    !t.text.trim().is_empty()
        || !t.tool_uses.is_empty()
        || !t.delegations.is_empty()
        || !t.file_mutations.is_empty()
        || t.thinking.is_some()
}

fn flush(turns: &mut Vec<Turn>, current: &mut Option<Turn>) {
    if let Some(t) = current.take()
        && turn_has_content(&t)
    {
        push_linked(turns, t);
    }
}

/// Set the result on the tool invocation with `id`. Searches the open turn
/// first, then previously-flushed turns (last to first). Returns whether a
/// match was found.
fn backfill_tool_result(
    current: &mut Option<Turn>,
    turns: &mut [Turn],
    id: Option<&str>,
    result: ToolResult,
) -> bool {
    let Some(id) = id else { return false };
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
    false
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
    let path = str_arg(args, &["path", "file_path", "filePath", "filename", "file", "target_file"])?;

    let n = name.to_ascii_lowercase();
    let is_delete = n.contains("delete");
    let looks_add = n.contains("create") || n.contains("add") || n.contains("new");
    let content = str_arg(args, &["content", "contents", "new_content", "text", "file_text", "new_str", "newstring", "newString"]);
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

    fn body() -> String {
        [
            r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"version":"1.0.66","cwd":"/tmp/proj","model":"gpt-5-copilot"}}"#,
            r#"{"type":"user.message","timestamp":"2026-06-30T10:00:01.000Z","data":{"text":"build a thing"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-06-30T10:00:02.000Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"text":"Listing files."}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-06-30T10:00:04.000Z","data":{"id":"c1","name":"shell","args":{"command":"ls"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-06-30T10:00:05.000Z","data":{"id":"c1","name":"shell","args":{"command":"ls"},"success":true,"output":"a.rs"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-06-30T10:00:06.000Z","data":{"id":"c2","name":"create_file","args":{"path":"a.rs","content":"fn main() {}\n"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-06-30T10:00:07.000Z","data":{"id":"c2","name":"create_file","args":{"path":"a.rs","content":"fn main() {}\n"},"success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:08.000Z","data":{"text":"Done."}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-06-30T10:00:09.000Z","data":{}}"#,
            r#"{"type":"session.shutdown","timestamp":"2026-06-30T10:00:10.000Z","data":{"modelMetrics":{"model":"gpt-5-copilot"},"usage":{"inputTokens":1200,"outputTokens":340}}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn builds_user_and_assistant_turns() {
        let view = to_view(&parse(&body()));
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "build a thing");
        assert_eq!(view.turns[1].role, Role::Assistant);
        // Two assistant messages collapsed into one turn.
        assert!(view.turns[1].text.contains("Listing files."));
        assert!(view.turns[1].text.contains("Done."));
    }

    #[test]
    fn assistant_turn_chains_to_user() {
        let view = to_view(&parse(&body()));
        assert_eq!(view.turns[1].parent_id.as_deref(), Some(view.turns[0].id.as_str()));
    }

    #[test]
    fn tool_calls_paired_with_results() {
        let view = to_view(&parse(&body()));
        let tools = &view.turns[1].tool_uses;
        assert_eq!(tools.len(), 2);
        let shell = tools.iter().find(|t| t.name == "shell").unwrap();
        assert_eq!(shell.category, Some(ToolCategory::Shell));
        assert_eq!(shell.result.as_ref().unwrap().content, "a.rs");
        assert!(!shell.result.as_ref().unwrap().is_error);
    }

    #[test]
    fn file_write_produces_mutation_with_raw_diff() {
        let view = to_view(&parse(&body()));
        let fm = view.turns[1]
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
    fn view_metadata_populated() {
        let view = to_view(&parse(&body()));
        assert_eq!(view.provider_id.as_deref(), Some("copilot"));
        assert_eq!(view.base.as_ref().unwrap().working_dir.as_deref(), Some("/tmp/proj"));
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
        let d = &view.turns[0].delegations[0];
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
        assert_eq!(view.events.len(), 2);
        assert_eq!(view.events[0].event_type, "hook.start");
        assert_eq!(view.events[1].event_type, "skill.invoked");
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
}
