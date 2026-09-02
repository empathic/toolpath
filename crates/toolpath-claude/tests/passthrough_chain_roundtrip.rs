//! Passthrough-chain round-trip: a Claude session whose attachments,
//! message-less `system` lines, and tool-result lines sit between the
//! messages must project back with the source file's lines, order, and
//! `parentUuid` links.
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

fn fixture_text() -> String {
    std::fs::read_to_string(fixture_path()).expect("read fixture")
}

/// A variant of the fixture.
fn view_of(jsonl: &str) -> ConversationView {
    toolpath_claude::provider::to_view(&read_jsonl(jsonl))
}

/// The fixture without the lines that contain `needle`.
fn load_view_without_line(needle: &str) -> ConversationView {
    let jsonl: String = fixture_text()
        .lines()
        .filter(|line| !line.contains(needle))
        .map(|line| format!("{line}\n"))
        .collect();
    view_of(&jsonl)
}

/// The fixture with a PostToolUse hook line between the tool-result line
/// and the reminders that follow it. The hook line hangs off the
/// `tool_use` line, so the first reminder's parent (the tool-result
/// line) is not the line before it.
fn fixture_with_post_tool_use_hook() -> String {
    let mut lines: Vec<String> = fixture_text().lines().map(str::to_string).collect();
    let h1 = lines
        .iter()
        .find(|l| l.contains("\"uuid\":\"h1\""))
        .expect("h1")
        .clone();
    let hook = h1
        .replace("\"uuid\":\"h1\"", "\"uuid\":\"hp\"")
        .replace("PreToolUse:Bash", "PostToolUse:Bash")
        .replace(
            "\"hookEvent\":\"PreToolUse\"",
            "\"hookEvent\":\"PostToolUse\"",
        );
    assert!(hook.contains("\"parentUuid\":\"a2\""));
    let t1 = lines
        .iter()
        .position(|l| l.contains("\"uuid\":\"t1\""))
        .expect("t1");
    lines.insert(t1 + 1, hook);
    lines.join("\n") + "\n"
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

/// `(uuid, parentUuid, type)` of every line that has a UUID, in file
/// order.
fn source_topology() -> Vec<(String, Option<String>, String)> {
    topology_of(&fixture_text())
}

fn topology_of(jsonl: &str) -> Vec<(String, Option<String>, String)> {
    jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let uuid = v.get("uuid")?.as_str()?.to_string();
            let parent = v
                .get("parentUuid")
                .and_then(|p| p.as_str())
                .map(str::to_string);
            let kind = v.get("type")?.as_str()?.to_string();
            Some((uuid, parent, kind))
        })
        .collect()
}

fn topology(convo: &Conversation) -> Vec<(String, Option<String>, String)> {
    convo
        .entries
        .iter()
        .map(|e| (e.uuid.clone(), e.parent_uuid.clone(), e.entry_type.clone()))
        .collect()
}

#[test]
fn fixture_loads_with_every_line() {
    let view = load_view();
    let turn_ids: Vec<&str> = view.turns.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(turn_ids, ["u1", "a1", "a2", "a3"]);
    assert_eq!(
        view.events.len(),
        12,
        "1 preamble line + 10 passthrough lines + 1 tool-result line"
    );
}

#[test]
fn projection_matches_the_source_file() {
    let expected = source_topology();
    assert_eq!(expected.len(), 15);

    let direct = ClaudeProjector.project(&load_view()).expect("project");
    assert_eq!(topology(&direct), expected, "direct view");

    let through_document = project_fixture();
    assert_eq!(
        topology(&through_document),
        expected,
        "after the Path round-trip"
    );

    // The tool-result line replays through the API only with its content
    // parts intact, so its message must equal the source message.
    let source_message = std::fs::read_to_string(fixture_path())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v.get("uuid").and_then(|u| u.as_str()) == Some("t1"))
        .map(|v| v["message"].clone())
        .expect("t1 in fixture");
    let projected_message = through_document
        .entries
        .iter()
        .find(|e| e.uuid == "t1")
        .map(|e| serde_json::to_value(&e.message).unwrap())
        .expect("t1 projected");
    assert_eq!(projected_message, source_message);
}

/// Without a hook line between the tool call and its result, the
/// post-result run's source parent is the tool-result line. The run must
/// still follow that line after the document round-trip.
#[test]
fn roundtrip_without_hook_line_keeps_post_result_run_after_the_call() {
    let view = ir_roundtrip(&load_view_without_line("\"uuid\":\"h1\""));
    let convo = ClaudeProjector.project(&view).expect("project");
    let ids: Vec<&str> = convo.entries.iter().map(|e| e.uuid.as_str()).collect();
    assert_eq!(
        ids,
        [
            "r1", "u1", "n1", "n2", "a1", "a2", "t1", "n3", "n4", "n5", "a3", "h2", "s1", "s2"
        ]
    );
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

/// A file that opens with an attachment and no headerless line: the
/// attachment has no source parent, and `derive_path` gives a parentless
/// event the last step as parent. The projection must still open the
/// chain with it. The headerless line has no UUID, so the source
/// topology is the fixture's.
#[test]
fn roundtrip_without_headerless_line_opens_the_chain_with_the_root_attachment() {
    let view = ir_roundtrip(&load_view_without_line("\"type\":\"permission-mode\""));
    assert!(
        view.events.iter().all(|e| !e.data.contains_key("raw")),
        "the variant has no headerless line"
    );
    let convo = ClaudeProjector.project(&view).expect("project");
    assert_eq!(topology(&convo), source_topology());
}

/// `derive_path` keeps an event parent only when it names a turn or the
/// previous line. A reminder whose parent is the tool-result line, with
/// a hook line between them, must still hang off the tool-result line
/// after the document round-trip, or the tool result becomes a leaf the
/// loader never reaches.
#[test]
fn roundtrip_keeps_a_parent_that_is_not_the_previous_line() {
    let jsonl = fixture_with_post_tool_use_hook();
    let expected = topology_of(&jsonl);
    assert_eq!(expected.len(), 16);
    let hp = expected.iter().position(|(id, _, _)| id == "hp").unwrap();
    assert_eq!(expected[hp - 1].0, "t1");
    assert_eq!(
        expected[hp + 1],
        ("n3".into(), Some("t1".into()), "attachment".into())
    );

    let view = ir_roundtrip(&view_of(&jsonl));
    let convo = ClaudeProjector.project(&view).expect("project");
    assert_eq!(topology(&convo), expected);
}

/// The reply after the tool call hangs off the tool-result line while a
/// hook line, written after that line, is the last line before the
/// reply. The IR keeps turn-to-turn parents only, so the reply must find
/// its way back to the tool-result line, or the tool result becomes a
/// leaf the loader never reaches.
#[test]
fn roundtrip_keeps_a_turn_parent_that_is_not_the_previous_line() {
    let jsonl: String = fixture_with_post_tool_use_hook()
        .lines()
        .filter(|l| {
            !["n3", "n4", "n5"]
                .iter()
                .any(|id| l.contains(&format!("\"uuid\":\"{id}\"")))
        })
        .map(|l| l.replace("\"parentUuid\":\"n5\"", "\"parentUuid\":\"t1\"") + "\n")
        .collect();
    let expected = topology_of(&jsonl);
    assert_eq!(expected.len(), 13);
    let a3 = expected.iter().position(|(id, _, _)| id == "a3").unwrap();
    assert_eq!(expected[a3].1.as_deref(), Some("t1"));
    assert_eq!(expected[a3 - 1].0, "hp");

    let view = ir_roundtrip(&view_of(&jsonl));
    let convo = ClaudeProjector.project(&view).expect("project");
    assert_eq!(topology(&convo), expected);
}

#[test]
fn projection_reparses_with_the_source_topology() {
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

    assert_eq!(topology(&reread), source_topology());
    let view = toolpath_claude::provider::to_view(&reread);
    assert!(
        view.turns
            .iter()
            .any(|t| t.text.contains("Two files: a.rs and b.rs.")),
        "the reply after the tool call must survive"
    );
    let result = reread
        .entries
        .iter()
        .find(|e| e.uuid == "t1")
        .and_then(|e| e.message.as_ref())
        .map(|m| m.tool_results())
        .expect("the tool-result line is a user message");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].tool_use_id, "toolu_1");
}
