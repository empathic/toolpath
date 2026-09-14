//! Passthrough-chain round-trip: a Claude session whose attachments and
//! message-less `system` lines sit between the messages must project
//! back to one `parentUuid` chain in source order.
//!
//! `claude -r` reads one chain from a leaf to the root. A projected file
//! whose attachments form a side chain with a leaf of its own can resume
//! from that leaf, which drops the tool result and the reply after the
//! last tool call. The fixture is synthetic: it has the line shapes of a
//! captured session (a SessionStart hook line before the first prompt,
//! the pre-turn attachments, a tool call with its hook line and
//! post-result reminders, the Stop hook and turn-duration lines) with
//! placeholder content.

use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use toolpath::v1::Graph;
use toolpath_claude::types::Conversation;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("passthrough_chain.jsonl")
}

fn load_view() -> ConversationView {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read fixture");
    toolpath_claude::provider::to_view(&convo)
}

/// Read JSONL text through `ConversationReader` like the fixture itself.
fn read_jsonl(jsonl: &str) -> Conversation {
    let mut file = NamedTempFile::with_suffix(".jsonl").expect("temp file");
    file.write_all(jsonl.as_bytes()).expect("write temp file");
    ConversationReader::read_conversation(file.path()).expect("read jsonl")
}

/// The fixture without the lines that contain `needle`.
fn load_view_without_line(needle: &str) -> ConversationView {
    let jsonl: String = std::fs::read_to_string(fixture_path())
        .expect("read fixture")
        .lines()
        .filter(|line| !line.contains(needle))
        .map(|line| format!("{line}\n"))
        .collect();
    toolpath_claude::provider::to_view(&read_jsonl(&jsonl))
}

fn ir_roundtrip(view: &ConversationView) -> ConversationView {
    let path = derive_path(view, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let path = back.into_single_path().expect("single path");
    extract_conversation(&path)
}

fn project_fixture() -> Conversation {
    let view = ir_roundtrip(&load_view());
    ClaudeProjector.project(&view).expect("project")
}

/// One chain: the first entry has no parent and every other entry names
/// the entry written before it.
fn assert_one_chain(convo: &Conversation) {
    assert_eq!(convo.entries[0].parent_uuid, None);
    for pair in convo.entries.windows(2) {
        assert_eq!(
            pair[1].parent_uuid.as_deref(),
            Some(pair[0].uuid.as_str()),
            "{} must hang off {}",
            pair[1].uuid,
            pair[0].uuid
        );
    }
}

#[test]
fn fixture_loads_with_every_line() {
    let view = load_view();
    let turn_ids: Vec<&str> = view.turns.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(turn_ids, ["u1", "a1", "a2", "a3"]);
    assert_eq!(
        view.events.len(),
        11,
        "1 preamble line + 10 passthrough lines"
    );
}

#[test]
fn projection_is_one_chain_in_source_order() {
    let convo = project_fixture();
    let ids: Vec<&str> = convo.entries.iter().map(|e| e.uuid.as_str()).collect();
    assert_eq!(
        ids,
        [
            "r1",
            "u1",
            "n1",
            "n2",
            "a1",
            "a2",
            "a2-result-toolu_1",
            "h1",
            "n3",
            "n4",
            "n5",
            "a3",
            "h2",
            "s1",
            "s2"
        ]
    );
    assert_one_chain(&convo);
}

/// Without a hook line between the tool call and its result, the
/// post-result run's source parent is the tool-result line, which the
/// view folds into the assistant turn. The run must still follow the
/// call after the document round-trip.
#[test]
fn roundtrip_without_hook_line_keeps_post_result_run_after_the_call() {
    let view = ir_roundtrip(&load_view_without_line("\"uuid\":\"h1\""));
    let convo = ClaudeProjector.project(&view).expect("project");
    let ids: Vec<&str> = convo.entries.iter().map(|e| e.uuid.as_str()).collect();
    assert_eq!(
        ids,
        [
            "r1",
            "u1",
            "n1",
            "n2",
            "a1",
            "a2",
            "a2-result-toolu_1",
            "n3",
            "n4",
            "n5",
            "a3",
            "h2",
            "s1",
            "s2"
        ]
    );
    assert_one_chain(&convo);
}

/// A file that opens with an attachment and no headerless line: the
/// attachment has no source parent, and `derive_path` gives a parentless
/// event the last step as parent. The projection must still open the
/// chain with it.
#[test]
fn roundtrip_without_headerless_line_opens_the_chain_with_the_root_attachment() {
    let view = ir_roundtrip(&load_view_without_line("\"type\":\"permission-mode\""));
    assert!(
        view.events.iter().all(|e| !e.data.contains_key("raw")),
        "the variant has no headerless line"
    );
    let convo = ClaudeProjector.project(&view).expect("project");
    let ids: Vec<&str> = convo.entries.iter().map(|e| e.uuid.as_str()).collect();
    assert_eq!(
        ids,
        [
            "r1",
            "u1",
            "n1",
            "n2",
            "a1",
            "a2",
            "a2-result-toolu_1",
            "h1",
            "n3",
            "n4",
            "n5",
            "a3",
            "h2",
            "s1",
            "s2"
        ]
    );
    assert_one_chain(&convo);
}

#[test]
fn projection_reparses_as_one_chain() {
    let convo = project_fixture();
    let mut jsonl = String::new();
    for raw in &convo.preamble {
        jsonl.push_str(&serde_json::to_string(raw).unwrap());
        jsonl.push('\n');
    }
    for entry in &convo.entries {
        jsonl.push_str(&serde_json::to_string(entry).unwrap());
        jsonl.push('\n');
    }
    let reread = read_jsonl(&jsonl);

    assert_eq!(reread.entries.len(), convo.entries.len());
    assert_one_chain(&reread);
    let view = toolpath_claude::provider::to_view(&reread);
    assert!(
        view.turns
            .iter()
            .any(|t| t.text.contains("Two files: a.rs and b.rs.")),
        "the reply after the tool call must survive"
    );
}
