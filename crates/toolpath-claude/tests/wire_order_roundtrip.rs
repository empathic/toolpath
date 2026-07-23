//! Wire-level entry-stream fidelity against the real captured compacted
//! session (`test-fixtures/claude/convo-compacted.jsonl`).
//!
//! Real Claude interleaves attachments and system entries (turn_duration,
//! compact boundary) with the turns. The projector used to emit all events
//! from a trailing pass, which regrouped them at the end of the file — a
//! resumed session then replayed its entries out of order. These tests pin
//! the projected stream to the source's shape.

use std::path::{Path, PathBuf};

use toolpath_convo::ConversationProjector;
use toolpath_claude::{ClaudeProjector, ConversationEntry, ConversationReader};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join("convo-compacted.jsonl")
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
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read fixture");
    let has_caveat = |c: &toolpath_claude::Conversation| -> Option<ConversationEntry> {
        c.entries
            .iter()
            .find(|e| e.extra.get("isMeta") == Some(&serde_json::json!(true)) && e.entry_type == "user")
            .cloned()
    };
    let source = has_caveat(&convo).expect("fixture carries an isMeta caveat entry");
    let projected = project(&convo);
    let out = has_caveat(&projected).expect("projected stream must keep the isMeta caveat");
    assert_eq!(source.entry_type, out.entry_type);
}
