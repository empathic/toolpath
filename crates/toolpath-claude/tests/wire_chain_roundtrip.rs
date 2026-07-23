//! Wire parent-chain fidelity through the full round-trip.
//!
//! Claude's headerless lines (`ai-title`, `last-prompt`,
//! `file-history-snapshot`, …) carry no `uuid`, so nothing on the wire can
//! ever chain through them. They survive the trip as IR events, and
//! `derive_path` deliberately splices them onto the head's ancestry — which
//! re-parents the neighboring *steps* through event steps whose ids
//! (`claude-preamble-N`) don't exist on the wire. `extract_conversation`
//! must undo that splice so a re-projected session's `parentUuid` chain
//! only ever names real entry uuids.

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
