//! The tool classifier otel documents derive with. `toolpath-otel` knows no
//! harness's tool names, so each harness it infers is classified by that
//! harness's own provider crate.

use toolpath_convo::ToolCategory;

/// The classifier for [`toolpath_otel::DeriveConfig::tool_category`].
pub(crate) fn classifier() -> toolpath_otel::ToolClassifier {
    toolpath_otel::ToolClassifier::new(tool_category)
}

/// The category of tool `name` in a session of `harness`, a harness id as
/// `toolpath-otel` records it in `meta.otel.harness`.
pub(crate) fn tool_category(harness: &str, name: &str) -> Option<ToolCategory> {
    match harness {
        "unknown" => agreed_category(name),
        known => provider(known)?(name),
    }
}

fn provider(harness: &str) -> Option<fn(&str) -> Option<ToolCategory>> {
    Some(match harness {
        "claude-code" => toolpath_claude::provider::tool_category,
        "codex" => toolpath_codex::tool_category,
        #[cfg(not(target_os = "emscripten"))]
        "opencode" => toolpath_opencode::tool_category,
        "pi" => toolpath_pi::provider::classify_tool,
        _ => return None,
    })
}

/// Every provider crate's classifier, including the harnesses otel does not
/// infer (a Gemini CLI, Copilot CLI or Cursor session infers as `unknown`).
/// The wasm build has no opencode or cursor crate.
const PROVIDERS: &[fn(&str) -> Option<ToolCategory>] = &[
    toolpath_claude::provider::tool_category,
    toolpath_codex::tool_category,
    toolpath_gemini::provider::tool_category,
    toolpath_copilot::tool_category,
    #[cfg(not(target_os = "emscripten"))]
    toolpath_opencode::tool_category,
    #[cfg(not(target_os = "emscripten"))]
    cursor_tool_category,
    toolpath_pi::provider::classify_tool,
];

#[cfg(not(target_os = "emscripten"))]
fn cursor_tool_category(name: &str) -> Option<ToolCategory> {
    toolpath_cursor::tool_category(0, name)
}

/// For an unknown harness: a category only when every provider that names
/// `name` agrees on it.
fn agreed_category(name: &str) -> Option<ToolCategory> {
    let mut cats = PROVIDERS.iter().filter_map(|f| f(name));
    let first = cats.next()?;
    cats.all(|c| c == first).then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_harness_uses_its_provider_crate() {
        assert_eq!(
            tool_category("claude-code", "Bash"),
            Some(ToolCategory::Shell)
        );
        assert_eq!(
            tool_category("claude-code", "Agent"),
            Some(ToolCategory::Delegation)
        );
        assert_eq!(
            tool_category("codex", "apply_patch"),
            Some(ToolCategory::FileWrite)
        );
        assert_eq!(tool_category("codex", "Bash"), None);
        assert_eq!(tool_category("pi", "bash"), Some(ToolCategory::Shell));
        assert_eq!(tool_category("gemini-cli", "Bash"), None);
    }

    #[cfg(not(target_os = "emscripten"))]
    #[test]
    fn opencode_uses_its_provider_crate() {
        assert_eq!(
            tool_category("opencode", "delete"),
            Some(ToolCategory::FileWrite)
        );
    }

    #[test]
    fn an_unknown_harness_takes_only_an_agreed_category() {
        assert_eq!(
            tool_category("unknown", "Agent"),
            Some(ToolCategory::Delegation)
        );
        for name in ["edit", "read", "write_file", "Bash", "ls", "frobnicate"] {
            let named: Vec<ToolCategory> = PROVIDERS.iter().filter_map(|f| f(name)).collect();
            let agreed = named.first().filter(|c| named.iter().all(|x| x == *c));
            assert_eq!(tool_category("unknown", name), agreed.copied(), "{name}");
        }
    }

    #[test]
    fn an_unknown_harness_asks_the_providers_otel_does_not_infer() {
        assert_eq!(
            tool_category("unknown", "run_shell_command"),
            Some(ToolCategory::Shell)
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    #[test]
    fn providers_that_disagree_give_an_unknown_harness_no_category() {
        assert_eq!(
            toolpath_gemini::provider::tool_category("list_directory"),
            Some(ToolCategory::FileRead)
        );
        assert_eq!(
            toolpath_copilot::tool_category("list_directory"),
            Some(ToolCategory::FileSearch)
        );
        assert_eq!(tool_category("unknown", "list_directory"), None);
        assert_eq!(cursor_tool_category("ls"), Some(ToolCategory::FileRead));
        assert_eq!(
            toolpath_copilot::tool_category("ls"),
            Some(ToolCategory::FileSearch)
        );
        assert_eq!(tool_category("unknown", "ls"), None);
    }
}
