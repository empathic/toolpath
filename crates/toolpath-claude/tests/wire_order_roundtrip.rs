//! Wire-level entry-stream fidelity against the real captured session
//! (`test-fixtures/claude/convo.jsonl`).
//!
//! Real Claude interleaves attachments and system entries (turn_duration,
//! compact boundary) with the turns. The projector used to emit all events
//! from a trailing pass, which regrouped them at the end of the file — a
//! resumed session then replayed its entries out of order. These tests pin
//! the projected stream to the source's shape.

use std::path::{Path, PathBuf};

use toolpath_convo::ConversationProjector;
use toolpath_claude::{ClaudeProjector, ConversationReader};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join("convo.jsonl")
}

fn project(entries_of: &toolpath_claude::Conversation) -> toolpath_claude::Conversation {
    let view = toolpath_claude::provider::to_view(entries_of);
    ClaudeProjector.project(&view).expect("project view")
}

fn type_sequence(c: &toolpath_claude::Conversation) -> Vec<String> {
    c.entries.iter().map(|e| e.entry_type.clone()).collect()
}

#[test]
fn projected_entry_type_sequence_matches_source() {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read fixture");
    let projected = project(&convo);
    assert_eq!(
        type_sequence(&convo),
        type_sequence(&projected),
        "entry stream must keep the source interleaving (attachments and \
         system entries in place, not regrouped at the end)"
    );
}

#[test]
fn caveat_entry_keeps_is_meta() {
    use toolpath_convo::{ConversationView, Item, Role, Turn};
    // Claude writes local-command caveat entries with `isMeta: true`; the
    // loader hides them from the transcript sent back to the API. The flag
    // is re-derived from the caveat envelope on projection.
    let caveat = Turn {
        id: "caveat-1".into(),
        parent_id: None,
        group_id: None,
        role: Role::User,
        timestamp: "2026-01-01T00:00:00Z".into(),
        text: "<local-command-caveat>Caveat: locally generated.</local-command-caveat>".into(),
        thinking: None,
        tool_uses: vec![],
        model: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: vec![],
        file_mutations: vec![],
    };
    let view = ConversationView {
        id: "wire-order-caveat".into(),
        items: vec![Item::Turn(caveat)],
        provider_id: Some("claude-code".into()),
        ..Default::default()
    };
    let projected = ClaudeProjector.project(&view).expect("project view");
    let entry = projected
        .entries
        .iter()
        .find(|e| e.entry_type == "user")
        .expect("caveat user entry");
    assert_eq!(
        entry.extra.get("isMeta"),
        Some(&serde_json::json!(true)),
        "caveat entries must stay hidden from the API transcript"
    );
}
