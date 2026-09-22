//! Wire parent-chain fidelity through the full round-trip.
//!
//! Claude's headerless lines (`ai-title`, `last-prompt`,
//! `file-history-snapshot`, …) carry no `uuid`, so nothing on the wire can
//! ever chain through them. They survive the trip as parentless IR events
//! (`claude-preamble-N`), and the turns around them keep their recorded
//! `parentUuid`, so a re-projected session's chain only ever names real
//! entry uuids.
//!
//! Entries that do carry a uuid (attachments, system entries) are chained
//! through as recorded; a parent naming an absorbed tool-result carrier is
//! redirected to the assistant turn on the way in and to the synthesized
//! carrier on the way out.

use std::io::Write;

use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{ConversationProjector, DeriveConfig, derive_path, extract_conversation};

const SESSION: &str = "11111111-2222-3333-4444-555555555555";

fn roundtrip_jsonl(jsonl: &str) -> toolpath_claude::Conversation {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join(format!("{SESSION}.jsonl"));
    let mut f = std::fs::File::create(&file_path).expect("create fixture");
    f.write_all(jsonl.as_bytes()).expect("write fixture");

    let convo = ConversationReader::read_conversation(&file_path).expect("read fixture");
    let view = toolpath_claude::provider::to_view(&convo);
    let path = derive_path(&view, &DeriveConfig::default());
    let extracted = extract_conversation(&path);
    ClaudeProjector
        .project(&extracted)
        .expect("project back to Claude JSONL")
}

#[test]
fn parent_uuid_chain_survives_headerless_lines() {
    // A snapshot line between two messages: on the wire the assistant's
    // parentUuid points at the user message, not at the snapshot (which
    // has no uuid to point at).
    let jsonl = format!(
        concat!(
            r#"{{"type":"file-history-snapshot","messageId":"m-1","snapshot":{{"messageId":"m-1","trackedFileBackups":{{}},"timestamp":"2026-01-01T00:00:00.000Z"}},"isSnapshotUpdate":false}}"#,
            "\n",
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{s}","timestamp":"2026-01-01T00:00:01Z","message":{{"role":"user","content":"hello"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"{s}","timestamp":"2026-01-01T00:00:02Z","message":{{"role":"assistant","model":"claude-fable-5","content":[{{"type":"text","text":"hi"}}]}}}}"#,
            "\n",
        ),
        s = SESSION
    );

    let projected = roundtrip_jsonl(&jsonl);

    let user = projected
        .entries
        .iter()
        .find(|e| e.uuid == "u1")
        .expect("user entry survives");
    assert_eq!(
        user.parent_uuid, None,
        "first message must stay a root, not chain onto a preamble event step"
    );

    let assistant = projected
        .entries
        .iter()
        .find(|e| e.uuid == "a1")
        .expect("assistant entry survives");
    assert_eq!(
        assistant.parent_uuid.as_deref(),
        Some("u1"),
        "assistant must chain onto the user message, not a synthesized event id"
    );

    // The snapshot line itself still round-trips (as preamble).
    assert_eq!(projected.preamble.len(), 1);
    assert_eq!(
        projected.preamble[0].get("type").and_then(|v| v.as_str()),
        Some("file-history-snapshot")
    );
}

#[test]
fn attachment_chain_through_absorbed_tool_result_survives() {
    // Real shape: `a1 ← tr (tool_result carrier) ← att ← a2`. The carrier is
    // absorbed into a1's tool uses, so on the way in `att` re-parents onto
    // a1 and on the way out onto the synthesized carrier, keeping every
    // projected `parentUuid` resolvable and every step on the head's
    // ancestry.
    let jsonl = format!(
        concat!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{s}","timestamp":"2026-01-01T00:00:01Z","message":{{"role":"user","content":"read it"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"{s}","timestamp":"2026-01-01T00:00:02Z","message":{{"role":"assistant","model":"claude-fable-5","content":[{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"a.rs"}}}}]}}}}"#,
            "\n",
            r#"{{"type":"user","uuid":"tr","parentUuid":"a1","sessionId":"{s}","timestamp":"2026-01-01T00:00:03Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"fn main() {{}}","is_error":false}}]}}}}"#,
            "\n",
            r#"{{"type":"attachment","uuid":"att","parentUuid":"tr","sessionId":"{s}","timestamp":"2026-01-01T00:00:04Z","attachment":{{"type":"file","fileName":"a.rs"}}}}"#,
            "\n",
            r#"{{"type":"assistant","uuid":"a2","parentUuid":"att","sessionId":"{s}","timestamp":"2026-01-01T00:00:05Z","message":{{"role":"assistant","model":"claude-fable-5","content":[{{"type":"text","text":"done"}}]}}}}"#,
            "\n",
        ),
        s = SESSION
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&file_path, &jsonl).expect("write fixture");
    let convo = ConversationReader::read_conversation(&file_path).expect("read fixture");
    let view = toolpath_claude::provider::to_view(&convo);

    let att = view
        .events()
        .find(|e| e.id == "att")
        .expect("attachment event");
    assert_eq!(att.parent_id.as_deref(), Some("a1"));
    let a2 = view.turns().find(|t| t.id == "a2").expect("a2");
    assert_eq!(a2.parent_id.as_deref(), Some("att"));

    let path = derive_path(&view, &DeriveConfig::default());
    let dead: Vec<&str> = toolpath::v1::query::dead_ends(&path.steps, &path.path.head)
        .iter()
        .map(|s| s.step.id.as_str())
        .collect();
    assert!(dead.is_empty(), "unexpected dead ends: {dead:?}");

    let projected = ClaudeProjector
        .project(&extract_conversation(&path))
        .expect("project back to Claude JSONL");
    let uuids: Vec<&str> = projected.entries.iter().map(|e| e.uuid.as_str()).collect();
    assert_eq!(uuids, vec!["u1", "a1", "a1-result-t1", "att", "a2"]);
    let parents: Vec<Option<&str>> = projected
        .entries
        .iter()
        .map(|e| e.parent_uuid.as_deref())
        .collect();
    assert_eq!(
        parents,
        vec![
            None,
            Some("u1"),
            Some("a1"),
            Some("a1-result-t1"),
            Some("att")
        ]
    );
}
