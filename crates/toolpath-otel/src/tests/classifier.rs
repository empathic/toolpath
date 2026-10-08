//! The provider crates' tool classifiers, dispatched by harness id the way
//! a caller wires them. Also compiled into `tests/derive.rs`.

use toolpath_convo::ToolCategory;

pub fn provider_tool_category(harness: &str, name: &str) -> Option<ToolCategory> {
    match harness {
        "claude-code" => toolpath_claude::provider::tool_category(name),
        "codex" => toolpath_codex::tool_category(name),
        "opencode" => toolpath_opencode::tool_category(name),
        "pi" => toolpath_pi::provider::classify_tool(name),
        "unknown" => agreed_category(name),
        _ => None,
    }
}

/// A category only when every provider that names `name` agrees on it.
fn agreed_category(name: &str) -> Option<ToolCategory> {
    let mut cats = ["claude-code", "codex", "opencode", "pi"]
        .into_iter()
        .filter_map(|h| provider_tool_category(h, name));
    let first = cats.next()?;
    cats.all(|c| c == first).then_some(first)
}
