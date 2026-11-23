//! What Claude Code loads when it resumes a session: the uuid chain from the leaf (last message)
//! back through `parentUuid`. A native session's chain runs through its `attachment` lines
//! (environment, skill listing, session context, ... about 3,000 tokens of context in the
//! recorded session); a session projected from a toolpath document must keep them on that chain,
//! not on a side branch the leaf never reaches.
//!
//! Deck v52 item 9: resuming the native session gave the model ~3,371 tokens of history, the same
//! session through toolpath ~326.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{ConversationProjector, DeriveConfig, derive_path, extract_conversation};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../goldens/claude-session/input.jsonl")
}

/// The entry types along the chain from the last message back to the root, leaf first.
fn chain(convo: &toolpath_claude::Conversation) -> Vec<String> {
    let by_uuid: HashMap<&str, &toolpath_claude::ConversationEntry> =
        convo.entries.iter().map(|e| (e.uuid.as_str(), e)).collect();
    let leaf = convo
        .entries
        .iter()
        .rev()
        .find(|e| e.message.is_some())
        .expect("a message entry");
    let mut out = Vec::new();
    let mut at = Some(leaf);
    let mut guard = 0;
    while let Some(e) = at {
        out.push(e.entry_type.clone());
        guard += 1;
        assert!(guard < 1000, "parent chain loops");
        at = e
            .parent_uuid
            .as_deref()
            .and_then(|p| by_uuid.get(p).copied());
    }
    out
}

fn count(chain: &[String], ty: &str) -> usize {
    chain.iter().filter(|t| t.as_str() == ty).count()
}

#[test]
fn the_resumed_chain_reaches_the_same_attachments_as_the_native_one() {
    let native = ConversationReader::read_conversation(golden()).expect("read the golden");
    let view = toolpath_claude::provider::to_view(&native);
    let path = derive_path(&view, &DeriveConfig::default());
    let json = Graph::from_path(path).to_json().expect("serialize");
    let path = Graph::from_json(&json)
        .expect("parse")
        .into_single_path()
        .expect("one path");
    let projected = ClaudeProjector
        .project(&extract_conversation(&path))
        .expect("project");

    let (n, p) = (chain(&native), chain(&projected));
    assert_eq!(count(&n, "attachment"), 12, "native chain: {n:?}");
    assert_eq!(
        count(&p, "attachment"),
        count(&n, "attachment"),
        "the projected chain skips attachments the native chain carries.\n native:    {n:?}\n projected: {p:?}"
    );
    assert_eq!(count(&p, "user"), count(&n, "user"));
    assert_eq!(count(&p, "assistant"), count(&n, "assistant"));
}
