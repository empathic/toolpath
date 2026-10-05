//! Per-harness tool-name → [`ToolCategory`] tables.
//!
//! Each agent harness names its tools differently, and the same name can
//! mean different things in different harnesses (opencode's `ls` reads a
//! directory listing; pi's `ls` is a file search). Classification is
//! therefore dispatched per harness: [`tool_category`] takes a
//! [`KnownHarness`] and consults only that harness's table.
//!
//! When the producing harness is not known, [`fallback_tool_category`]
//! classifies a name only if every harness that lists it exactly agrees
//! on its category.

use crate::ToolCategory;
use ToolCategory::{Delegation, FileRead, FileSearch, FileWrite, Network, Shell};

/// An agent harness with a tool-name table in this module.
///
/// [`KnownHarness::id`] is the `provider_id` the harness's provider crate
/// stamps on its [`ConversationView`](crate::ConversationView).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KnownHarness {
    /// Claude Code (`claude-code`).
    ClaudeCode,
    /// Gemini CLI (`gemini-cli`).
    GeminiCli,
    /// Codex CLI (`codex`).
    Codex,
    /// opencode (`opencode`).
    Opencode,
    /// Pi (`pi`).
    Pi,
    /// GitHub Copilot CLI (`copilot-cli`).
    CopilotCli,
    /// Cursor (`cursor`).
    Cursor,
}

impl KnownHarness {
    /// Every known harness, in a stable order.
    pub const ALL: [KnownHarness; 7] = [
        KnownHarness::ClaudeCode,
        KnownHarness::GeminiCli,
        KnownHarness::Codex,
        KnownHarness::Opencode,
        KnownHarness::Pi,
        KnownHarness::CopilotCli,
        KnownHarness::Cursor,
    ];

    /// The harness's provider id.
    pub fn id(self) -> &'static str {
        match self {
            KnownHarness::ClaudeCode => "claude-code",
            KnownHarness::GeminiCli => "gemini-cli",
            KnownHarness::Codex => "codex",
            KnownHarness::Opencode => "opencode",
            KnownHarness::Pi => "pi",
            KnownHarness::CopilotCli => "copilot-cli",
            KnownHarness::Cursor => "cursor",
        }
    }

    /// Look up a harness by its exact, case-sensitive provider id.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    /// The harness's exact-name table.
    ///
    /// [`tool_category`] may normalise the name or apply heuristics before
    /// or after consulting it: pi lowercases and treats any name containing
    /// `task` or `agent` as delegation; Copilot lowercases (ASCII) and falls
    /// back to substring matches. The other harnesses match exactly.
    pub fn tool_table(self) -> &'static [(&'static str, ToolCategory)] {
        match self {
            KnownHarness::ClaudeCode => CLAUDE_CODE,
            KnownHarness::GeminiCli => GEMINI_CLI,
            KnownHarness::Codex => CODEX,
            KnownHarness::Opencode => OPENCODE,
            KnownHarness::Pi => PI,
            KnownHarness::CopilotCli => COPILOT_CLI,
            KnownHarness::Cursor => CURSOR,
        }
    }
}

/// Classify `name` as the given harness names its tools.
///
/// Returns `None` for names the harness's table does not recognise.
pub fn tool_category(harness: KnownHarness, name: &str) -> Option<ToolCategory> {
    match harness {
        KnownHarness::Pi => {
            let lower = name.to_lowercase();
            if lower.contains("task") || lower.contains("agent") {
                return Some(Delegation);
            }
            lookup(PI, &lower)
        }
        KnownHarness::CopilotCli => {
            let n = name.to_ascii_lowercase();
            lookup(COPILOT_CLI, &n).or_else(|| copilot_substring(&n))
        }
        _ => lookup(harness.tool_table(), name),
    }
}

/// Classify `name` for the harness with provider id `harness_id`, or with
/// [`fallback_tool_category`] when the id is absent or not a known harness.
pub fn tool_category_for(harness_id: Option<&str>, name: &str) -> Option<ToolCategory> {
    match harness_id.and_then(KnownHarness::from_id) {
        Some(h) => tool_category(h, name),
        None => fallback_tool_category(name),
    }
}

/// Classify `name` without knowing which harness produced it.
///
/// Looks `name` up exactly (case-sensitive, no heuristics) in every
/// harness's [`KnownHarness::tool_table`]. Returns the category when at
/// least one table lists the name and every table that lists it agrees;
/// returns `None` when no table lists it or the tables disagree.
pub fn fallback_tool_category(name: &str) -> Option<ToolCategory> {
    let mut found = None;
    for h in KnownHarness::ALL {
        if let Some(cat) = lookup(h.tool_table(), name) {
            match found {
                None => found = Some(cat),
                Some(prev) if prev != cat => return None,
                Some(_) => {}
            }
        }
    }
    found
}

fn lookup(table: &[(&str, ToolCategory)], name: &str) -> Option<ToolCategory> {
    table.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

fn copilot_substring(n: &str) -> Option<ToolCategory> {
    if n.contains("shell") || n.contains("terminal") || n.contains("command") {
        Some(Shell)
    } else if n.contains("search") || n.contains("grep") || n.contains("glob") {
        Some(FileSearch)
    } else if n.contains("write")
        || n.contains("edit")
        || n.contains("patch")
        || n.contains("replace")
    {
        Some(FileWrite)
    } else if n.contains("read") || n.contains("view") || n.contains("file") {
        Some(FileRead)
    } else if n.contains("web") || n.contains("fetch") || n.contains("http") {
        Some(Network)
    } else {
        None
    }
}

const CLAUDE_CODE: &[(&str, ToolCategory)] = &[
    ("Read", FileRead),
    ("Glob", FileSearch),
    ("Grep", FileSearch),
    ("Write", FileWrite),
    ("Edit", FileWrite),
    ("MultiEdit", FileWrite),
    ("NotebookEdit", FileWrite),
    ("Bash", Shell),
    ("WebFetch", Network),
    ("WebSearch", Network),
    ("Task", Delegation),
    ("Agent", Delegation),
];

/// Kept in sync with <https://geminicli.com/docs/reference/tools>.
const GEMINI_CLI: &[(&str, ToolCategory)] = &[
    ("read_file", FileRead),
    ("read_many_files", FileRead),
    ("list_directory", FileRead),
    ("get_internal_docs", FileRead),
    ("read_mcp_resource", FileRead),
    ("glob", FileSearch),
    ("grep_search", FileSearch),
    ("search_file_content", FileSearch),
    ("write_file", FileWrite),
    ("replace", FileWrite),
    ("edit", FileWrite),
    ("run_shell_command", Shell),
    ("web_fetch", Network),
    ("google_web_search", Network),
    ("task", Delegation),
    ("activate_skill", Delegation),
];

const CODEX: &[(&str, ToolCategory)] = &[
    ("read_file", FileRead),
    ("read_many_files", FileRead),
    ("list_dir", FileRead),
    ("view_image", FileRead),
    ("mcp_resource", FileRead),
    ("glob", FileSearch),
    ("grep_search", FileSearch),
    ("search_file_content", FileSearch),
    ("tool_search", FileSearch),
    ("tool_suggest", FileSearch),
    ("write_file", FileWrite),
    ("apply_patch", FileWrite),
    ("replace", FileWrite),
    ("edit", FileWrite),
    ("shell", Shell),
    ("shell_command", Shell),
    ("exec_command", Shell),
    ("unified_exec", Shell),
    ("write_stdin", Shell),
    ("js_repl", Shell),
    ("web_fetch", Network),
    ("web_search", Network),
    ("google_web_search", Network),
    ("spawn_agent", Delegation),
    ("close_agent", Delegation),
    ("wait_agent", Delegation),
    ("resume_agent", Delegation),
    ("send_message", Delegation),
    ("followup_task", Delegation),
    ("list_agents", Delegation),
    ("agent_jobs", Delegation),
    ("task", Delegation),
    ("activate_skill", Delegation),
];

/// MCP tools (`mcp__<server>__<tool>`) are deliberately absent.
const OPENCODE: &[(&str, ToolCategory)] = &[
    ("read", FileRead),
    ("list", FileRead),
    ("view", FileRead),
    ("ls", FileRead),
    ("glob", FileSearch),
    ("grep", FileSearch),
    ("search", FileSearch),
    ("write", FileWrite),
    ("edit", FileWrite),
    ("multiedit", FileWrite),
    ("patch", FileWrite),
    ("delete", FileWrite),
    ("bash", Shell),
    ("shell", Shell),
    ("exec", Shell),
    ("terminal", Shell),
    ("webfetch", Network),
    ("websearch", Network),
    ("web_fetch", Network),
    ("web_search", Network),
    ("fetch", Network),
    ("task", Delegation),
    ("agent", Delegation),
    ("subagent", Delegation),
    ("spawn_agent", Delegation),
];

/// Matched against the lowercased name, after the `task`/`agent`
/// substring rule in [`tool_category`].
const PI: &[(&str, ToolCategory)] = &[
    ("read", FileRead),
    ("write", FileWrite),
    ("edit", FileWrite),
    ("bash", Shell),
    ("shell", Shell),
    ("run", Shell),
    ("exec", Shell),
    ("grep", FileSearch),
    ("glob", FileSearch),
    ("find", FileSearch),
    ("ls", FileSearch),
    ("webfetch", Network),
    ("websearch", Network),
    ("fetch", Network),
];

/// Not yet verified against a real Copilot CLI session. Matched against
/// the ASCII-lowercased name, before the substring fallbacks in
/// [`tool_category`].
const COPILOT_CLI: &[(&str, ToolCategory)] = &[
    ("shell", Shell),
    ("bash", Shell),
    ("sh", Shell),
    ("run", Shell),
    ("exec", Shell),
    ("execute", Shell),
    ("terminal", Shell),
    ("run_in_terminal", Shell),
    ("run_command", Shell),
    ("run_shell", Shell),
    ("command", Shell),
    ("read", FileRead),
    ("read_file", FileRead),
    ("readfile", FileRead),
    ("view", FileRead),
    ("view_file", FileRead),
    ("cat", FileRead),
    ("open", FileRead),
    ("get_file", FileRead),
    ("write", FileWrite),
    ("write_file", FileWrite),
    ("writefile", FileWrite),
    ("create", FileWrite),
    ("create_file", FileWrite),
    ("edit", FileWrite),
    ("edit_file", FileWrite),
    ("apply_patch", FileWrite),
    ("patch", FileWrite),
    ("str_replace", FileWrite),
    ("str_replace_editor", FileWrite),
    ("replace", FileWrite),
    ("replace_string_in_file", FileWrite),
    ("insert", FileWrite),
    ("delete_file", FileWrite),
    ("glob", FileSearch),
    ("list", FileSearch),
    ("list_dir", FileSearch),
    ("list_directory", FileSearch),
    ("ls", FileSearch),
    ("find", FileSearch),
    ("find_files", FileSearch),
    ("grep", FileSearch),
    ("search", FileSearch),
    ("ripgrep", FileSearch),
    ("rg", FileSearch),
    ("file_search", FileSearch),
    ("grep_search", FileSearch),
    ("semantic_search", FileSearch),
    ("codebase_search", FileSearch),
    ("fetch", Network),
    ("web_fetch", Network),
    ("fetch_url", Network),
    ("web_search", Network),
    ("search_web", Network),
    ("browser", Network),
    ("open_url", Network),
    ("http", Network),
    ("subagent", Delegation),
    ("delegate", Delegation),
    ("spawn_agent", Delegation),
    ("task", Delegation),
    ("agent", Delegation),
    ("dispatch_agent", Delegation),
];

/// Cursor tool names: the `aiserver.v1` protobuf tool names, the
/// workbench `*ToolCall` discriminators, and the agent-side JSONL
/// vocabulary. Numeric `toolFormerData.tool` ids are resolved to names by
/// `toolpath-cursor` before this table is consulted. Planning, MCP control
/// plane, UI control flow, reporting, VCS writes and media tools are
/// deliberately absent.
const CURSOR: &[(&str, ToolCategory)] = &[
    ("run_terminal_command_v2", Shell),
    ("run_terminal_commands", Shell),
    ("run_terminal_cmd", Shell),
    ("run_test", Shell),
    ("write_shell_stdin", Shell),
    ("Shell", Shell),
    ("shell", Shell),
    ("edit_file_v2", FileWrite),
    ("edit_file", FileWrite),
    ("edit", FileWrite),
    ("Edit", FileWrite),
    ("Write", FileWrite),
    ("StrReplace", FileWrite),
    ("delete_file", FileWrite),
    ("delete", FileWrite),
    ("new_edit", FileWrite),
    ("new_file", FileWrite),
    ("save_file", FileWrite),
    ("reapply", FileWrite),
    ("undo_edit", FileWrite),
    ("apply_agent_diff", FileWrite),
    ("create_rm_files", FileWrite),
    ("add_test", FileWrite),
    ("delete_test", FileWrite),
    ("fix_lints", FileWrite),
    ("fix_lints_subagent", FileWrite),
    ("read_file_v2", FileRead),
    ("read_file", FileRead),
    ("read", FileRead),
    ("Read", FileRead),
    ("read_chunk", FileRead),
    ("list_dir", FileRead),
    ("list_dir_v2", FileRead),
    ("ls", FileRead),
    ("read_project", FileRead),
    ("get_project_structure", FileRead),
    ("get_symbols", FileRead),
    ("get_tests", FileRead),
    ("gotodef", FileRead),
    ("summarize_code", FileRead),
    ("read_lints", FileRead),
    ("read_with_linter", FileRead),
    ("read_semsearch_files", FileRead),
    ("blame_by_file_path", FileRead),
    ("glob_file_search", FileSearch),
    ("Glob", FileSearch),
    ("glob", FileSearch),
    ("ripgrep_raw_search", FileSearch),
    ("ripgrep_search", FileSearch),
    ("grep_search", FileSearch),
    ("grep", FileSearch),
    ("Grep", FileSearch),
    ("search", FileSearch),
    ("search_symbols", FileSearch),
    ("semantic_search", FileSearch),
    ("semantic_search_full", FileSearch),
    ("sem_search", FileSearch),
    ("deep_search", FileSearch),
    ("deep_search_subagent", FileSearch),
    ("tool_call_file_search", FileSearch),
    ("web_search", Network),
    ("web_fetch", Network),
    ("fetch_pull_request", Network),
    ("fetch", Network),
    ("call_mcp_tool", Network),
    ("task_v2", Delegation),
    ("task", Delegation),
    ("Task", Delegation),
    ("task_subagent", Delegation),
    ("spec_subagent", Delegation),
    ("background_composer_followup", Delegation),
    ("start_grind_execution", Delegation),
    ("start_grind_planning", Delegation),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn ids_round_trip() {
        for h in KnownHarness::ALL {
            assert_eq!(KnownHarness::from_id(h.id()), Some(h));
        }
        assert_eq!(KnownHarness::from_id("Claude-Code"), None);
        assert_eq!(KnownHarness::from_id("unknown"), None);
    }

    #[test]
    fn tables_have_no_duplicate_names() {
        for h in KnownHarness::ALL {
            let mut seen = BTreeSet::new();
            for (n, _) in h.tool_table() {
                assert!(seen.insert(*n), "{} lists {n} twice", h.id());
            }
        }
    }

    #[test]
    fn normalised_tables_are_lowercase() {
        for h in [KnownHarness::Pi, KnownHarness::CopilotCli] {
            for (n, _) in h.tool_table() {
                assert_eq!(*n, n.to_lowercase(), "{} entry {n}", h.id());
            }
        }
    }

    #[test]
    fn dispatch_is_per_harness() {
        assert_eq!(tool_category(KnownHarness::Opencode, "ls"), Some(FileRead));
        assert_eq!(tool_category(KnownHarness::Pi, "ls"), Some(FileSearch));
        assert_eq!(tool_category(KnownHarness::ClaudeCode, "read"), None);
        assert_eq!(tool_category(KnownHarness::Pi, "READ"), Some(FileRead));
        assert_eq!(
            tool_category(KnownHarness::Pi, "MyAgentThing"),
            Some(Delegation)
        );
        assert_eq!(
            tool_category(KnownHarness::CopilotCli, "Run_In_Terminal"),
            Some(Shell)
        );
        assert_eq!(
            tool_category(KnownHarness::CopilotCli, "mcp__x__open_file"),
            Some(FileRead)
        );
        assert_eq!(
            tool_category(KnownHarness::Codex, "unified_exec"),
            Some(Shell)
        );
        assert_eq!(
            tool_category(KnownHarness::Codex, "shell_command"),
            Some(Shell)
        );
        assert_eq!(
            tool_category(KnownHarness::Cursor, "StrReplace"),
            Some(FileWrite)
        );
    }

    #[test]
    fn tool_category_for_dispatches_known_ids_and_falls_back() {
        assert_eq!(tool_category_for(Some("pi"), "ls"), Some(FileSearch));
        assert_eq!(tool_category_for(Some("opencode"), "ls"), Some(FileRead));
        assert_eq!(tool_category_for(None, "ls"), None);
        assert_eq!(tool_category_for(Some("aider"), "Bash"), Some(Shell));
        assert_eq!(tool_category_for(None, "mystery"), None);
    }

    #[test]
    fn fallback_requires_agreement() {
        assert_eq!(fallback_tool_category("Bash"), Some(Shell));
        assert_eq!(fallback_tool_category("apply_patch"), Some(FileWrite));
        assert_eq!(fallback_tool_category("unified_exec"), Some(Shell));
        assert_eq!(fallback_tool_category("shell_command"), Some(Shell));
        assert_eq!(fallback_tool_category("edit"), Some(FileWrite));
        assert_eq!(fallback_tool_category("ls"), None);
        assert_eq!(fallback_tool_category("list_dir"), None);
        assert_eq!(fallback_tool_category("LS"), None);
        assert_eq!(fallback_tool_category("todowrite"), None);
    }

    /// Names whose category differs between harness tables. Each harness
    /// keeps its own mapping; [`fallback_tool_category`] returns `None`
    /// for these.
    #[test]
    fn cross_harness_disagreements_are_known() {
        let mut by_name: BTreeMap<&str, Vec<ToolCategory>> = BTreeMap::new();
        for h in KnownHarness::ALL {
            for (n, c) in h.tool_table() {
                let cats = by_name.entry(n).or_default();
                if !cats.contains(c) {
                    cats.push(*c);
                }
            }
        }
        let disagreements: Vec<&str> = by_name
            .into_iter()
            .filter(|(_, cats)| cats.len() > 1)
            .map(|(n, _)| n)
            .collect();
        assert_eq!(disagreements, ["list", "list_dir", "list_directory", "ls"]);
    }
}
