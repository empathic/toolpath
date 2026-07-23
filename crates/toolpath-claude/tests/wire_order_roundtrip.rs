//! Wire-level entry-stream fidelity against the real captured sessions
//! (`test-fixtures/claude/convo.jsonl` and `convo-compacted.jsonl`).
//!
//! Real Claude interleaves attachment and system entries with the turns.
//! The projector used to emit all events from a trailing pass, which
//! regrouped them at the end of the file — a resumed session then replayed
//! its entries out of order.
//!
//! What is pinned: the `entry_type` sequence of the direct
//! `to_view` → `project` pipeline matches the source entry for entry
//! (attachments in place, not regrouped at the end) on both fixtures — for
//! the compacted one that includes the `compact_boundary` system entry and
//! the `isCompactSummary` user entry, which fold into one `Item::Compaction`
//! on read and are re-emitted natively at the same position on projection —
//! and caveat user entries keep `isMeta: true` on projection.
//!
//! What is NOT pinned: `parentUuid` values. 11 of `convo.jsonl`'s 45
//! entries legitimately diverge — the projector re-synthesizes tool-result
//! carrier entries under derived uuids (`<turn-uuid>-result-<tool-id>`):
//! 10 of the diverged entries point at a re-synthesized carrier uuid, and
//! 1 is rewired to the preceding turn. Also not pinned: the
//! derive → extract → project pipeline — only the direct projection is
//! exercised here.

use std::path::{Path, PathBuf};

use toolpath_claude::{ClaudeProjector, ConversationEntry, ConversationReader};
use toolpath_convo::ConversationProjector;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join(name)
}

fn project(entries_of: &toolpath_claude::Conversation) -> toolpath_claude::Conversation {
    let view = toolpath_claude::provider::to_view(entries_of);
    ClaudeProjector.project(&view).expect("project view")
}

fn type_sequence(c: &toolpath_claude::Conversation) -> Vec<String> {
    c.entries.iter().map(|e| e.entry_type.clone()).collect()
}

fn assert_sequence_roundtrips(name: &str) {
    let convo = ConversationReader::read_conversation(fixture_path(name)).expect("read fixture");
    let projected = project(&convo);
    assert_eq!(
        type_sequence(&convo),
        type_sequence(&projected),
        "entry stream of {name} must keep the source interleaving \
         (attachments and system entries in place, not regrouped at the end)"
    );
}

#[test]
fn projected_entry_type_sequence_matches_source() {
    assert_sequence_roundtrips("convo.jsonl");
}

#[test]
fn projected_entry_type_sequence_matches_compacted_source() {
    // The compact boundary (`type: "system"`, subtype `compact_boundary`)
    // and its `isCompactSummary` user entry fold into one `Item::Compaction`
    // on read; projection re-emits both entries at the same position.
    assert_sequence_roundtrips("convo-compacted.jsonl");
}

#[test]
fn caveat_turn_projects_with_is_meta() {
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

#[test]
fn caveat_entry_keeps_is_meta() {
    let convo = ConversationReader::read_conversation(fixture_path("convo-compacted.jsonl"))
        .expect("read fixture");
    let has_caveat = |c: &toolpath_claude::Conversation| -> Option<ConversationEntry> {
        c.entries
            .iter()
            .find(|e| {
                e.extra.get("isMeta") == Some(&serde_json::json!(true)) && e.entry_type == "user"
            })
            .cloned()
    };
    let source = has_caveat(&convo).expect("fixture carries an isMeta caveat entry");
    let projected = project(&convo);
    let out = has_caveat(&projected).expect("projected stream must keep the isMeta caveat");
    assert_eq!(source.entry_type, out.entry_type);
}
