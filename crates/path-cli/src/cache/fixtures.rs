//! Session documents for the tests of the cache module and of its
//! readers.

use toolpath::v1::Path;
use toolpath_convo::{ConversationView, Role, Turn};

/// The `n`th turn of a session, after turn `n - 1`.
fn turn(n: usize, role: Role, text: &str, timestamp: &str) -> Turn {
    Turn {
        id: format!("t{n}"),
        parent_id: n.checked_sub(1).map(|p| format!("t{p}")),
        group_id: None,
        role,
        timestamp: timestamp.to_string(),
        text: text.to_string(),
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

/// The document of a session with a prompt and an answer, as a
/// derive writes it, with `title` when the session has one.
pub(crate) fn derive(title: Option<&str>, turns: &[(Role, &str, &str)]) -> Path {
    derive_turns(
        title,
        turns
            .iter()
            .enumerate()
            .map(|(n, (role, text, timestamp))| turn(n, role.clone(), text, timestamp))
            .collect(),
    )
}

/// The document of a session with the turns `turns`, as a derive
/// writes it, with `title` when the session has one.
fn derive_turns(title: Option<&str>, turns: Vec<Turn>) -> Path {
    let conversation = ConversationView {
        id: "1a2b3c4d-0000-0000-0000-000000000000".to_string(),
        provider_id: Some("claude-code".to_string()),
        turns,
        ..Default::default()
    };
    toolpath_convo::derive_path(
        &conversation,
        &toolpath_convo::DeriveConfig {
            base_uri: Some("file:///work/project".to_string()),
            title: title.map(str::to_string),
            ..Default::default()
        },
    )
}

pub(crate) const TURNS: [(Role, &str, &str); 3] = [
    (Role::User, "hello", "2026-09-23T10:00:00Z"),
    (Role::User, "fix the parser", "2026-09-23T10:00:05Z"),
    (Role::Assistant, "Fixed.", "2026-09-23T10:30:00Z"),
];
