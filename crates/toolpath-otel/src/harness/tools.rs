//! Tool-name → category tables, one per harness, matching each provider
//! crate's own classifier. `toolpath_convo::tools` will replace this module.

use super::SourceHarness;
use ToolCategory::{Delegation, FileRead, FileSearch, FileWrite, Network, Shell};
use toolpath_convo::ToolCategory;

/// Classify `name` as `harness` names its tools; an unknown harness gets
/// [`fallback_tool_category`].
pub fn tool_category(harness: SourceHarness, name: &str) -> Option<ToolCategory> {
    match harness {
        SourceHarness::ClaudeCode => lookup(CLAUDE_CODE, name),
        // `shell_command` (codex-rs core/src/tools/spec.rs) is otel-only:
        // toolpath-codex does not list it.
        SourceHarness::Codex if name == "shell_command" => Some(Shell),
        SourceHarness::Codex => lookup(CODEX, name),
        SourceHarness::Opencode => lookup(OPENCODE, name),
        SourceHarness::Pi => {
            let lower = name.to_lowercase();
            if lower.contains("task") || lower.contains("agent") {
                return Some(Delegation);
            }
            lookup(PI, &lower)
        }
        SourceHarness::Unknown => fallback_tool_category(name),
    }
}

/// Exact lookup in every table; a category only when every table that lists
/// `name` agrees.
pub fn fallback_tool_category(name: &str) -> Option<ToolCategory> {
    let mut found = None;
    for table in ALL_TABLES {
        if let Some(cat) = lookup(table, name) {
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

const ALL_TABLES: [&[(&str, ToolCategory)]; 7] = [
    CLAUDE_CODE,
    GEMINI_CLI,
    CODEX,
    OPENCODE,
    PI,
    COPILOT_CLI,
    CURSOR,
];

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

/// toolpath-copilot also lowercases and applies substring fallbacks; the
/// fallback here uses only the exact names.
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
    use std::collections::BTreeSet;

    fn names(table: &[(&'static str, ToolCategory)]) -> BTreeSet<&'static str> {
        table.iter().map(|(n, _)| *n).collect()
    }

    /// Each harness's mapping exactly as its provider crate's classifier on
    /// main 77dc16a5 lists it (`toolpath-claude`/`-codex`/`-opencode`
    /// `tool_category`, `toolpath-pi` `classify_tool`).
    #[test]
    fn per_harness_tables_match_the_provider_crates() {
        let claude: &[(&str, ToolCategory)] = &[
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
        let codex: &[(&str, ToolCategory)] = &[
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
        let opencode: &[(&str, ToolCategory)] = &[
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
        let pi: &[(&str, ToolCategory)] = &[
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
        for (harness, table, want) in [
            (SourceHarness::ClaudeCode, CLAUDE_CODE, claude),
            (SourceHarness::Codex, CODEX, codex),
            (SourceHarness::Opencode, OPENCODE, opencode),
            (SourceHarness::Pi, PI, pi),
        ] {
            assert_eq!(names(table), names(want), "{harness:?}");
            for (name, cat) in want {
                assert_eq!(
                    tool_category(harness, name),
                    Some(*cat),
                    "{harness:?} {name}"
                );
            }
            assert_eq!(tool_category(harness, "mystery"), None, "{harness:?}");
        }
    }

    #[test]
    fn harness_specific_rules() {
        assert_eq!(
            tool_category(SourceHarness::Codex, "shell_command"),
            Some(Shell)
        );
        assert_eq!(tool_category(SourceHarness::Pi, "Bash"), Some(Shell));
        assert_eq!(
            tool_category(SourceHarness::Pi, "SubAgent"),
            Some(Delegation)
        );
        assert_eq!(tool_category(SourceHarness::Opencode, "ls"), Some(FileRead));
        assert_eq!(tool_category(SourceHarness::Pi, "ls"), Some(FileSearch));
        assert_eq!(tool_category(SourceHarness::ClaudeCode, "bash"), None);
        assert_eq!(tool_category(SourceHarness::Opencode, "todowrite"), None);
    }

    #[test]
    fn unknown_harness_falls_back_only_on_agreement() {
        for (name, want) in [
            ("Bash", Some(Shell)),
            ("exec_command", Some(Shell)),
            ("run_shell_command", Some(Shell)),
            ("bash", Some(Shell)),
            ("apply_patch", Some(FileWrite)),
            ("edit", Some(FileWrite)),
            ("read", Some(FileRead)),
            ("grep", Some(FileSearch)),
            ("webfetch", Some(Network)),
            ("task", Some(Delegation)),
            ("Agent", Some(Delegation)),
            ("ls", None),
            ("list", None),
            ("list_dir", None),
            ("list_directory", None),
            ("shell_command", None),
            ("LS", None),
            ("todowrite", None),
            ("mystery", None),
        ] {
            assert_eq!(tool_category(SourceHarness::Unknown, name), want, "{name}");
        }
    }
}
