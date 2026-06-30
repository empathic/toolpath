//! Full projection round-trip contract:
//!
//! native fixture → `session_to_view` → `derive_path` → serialize+reparse
//! `Path` → `extract_conversation` → `project` → OpenClaw session.
//!
//! Asserts the chain preserves messages, roles, tool calls/results, and the
//! OpenClaw channel metadata.

use std::collections::HashSet;
use std::path::Path;

use toolpath_convo::{ConversationProjector, Role, extract_conversation};
use toolpath_openclaw::project::OpenClawProjector;
use toolpath_openclaw::reader::read_session_from_file;
use toolpath_openclaw::types::{AgentMessage, ContentBlock, Entry};
use toolpath_openclaw::{DeriveConfig, derive_path, session_to_view};

#[test]
fn full_roundtrip_preserves_conversation() {
    // 1. native fixture → view
    let mut src = read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap();
    src.attach_routing_key();
    let view1 = session_to_view(&src);
    assert_eq!(view1.provider_id.as_deref(), Some("openclaw"));

    // 2. view → Path (channel-aware actor + meta), then serialize + reparse.
    let path = derive_path(&src, &DeriveConfig::default());
    let json = serde_json::to_string(&path).unwrap();
    let path2: toolpath::v1::Path = serde_json::from_str(&json).unwrap();

    // Channel metadata survives the JSON round-trip on the Path.
    let meta = path2.meta.as_ref().unwrap();
    assert_eq!(meta.source.as_deref(), Some("openclaw"));
    assert_eq!(meta.extra["openclaw"]["channel"], "whatsapp");
    assert!(
        path2
            .steps
            .iter()
            .any(|s| s.step.actor == "human:whatsapp/15555550123")
    );

    // 3. Path → view → project → OpenClaw session.
    let view2 = extract_conversation(&path2);
    assert!(view2.turns.iter().any(|t| t.role == Role::User));
    assert!(view2.turns.iter().any(|t| t.role == Role::Assistant));

    let projected = OpenClawProjector::default().project(&view2).unwrap();
    assert_eq!(projected.header.version, 3);

    // 4. Every entry serializes one-per-line and re-parses.
    for entry in &projected.entries {
        let line = serde_json::to_string(entry).unwrap();
        assert!(!line.contains('\n'), "entry has embedded newline: {line}");
        let _: Entry = serde_json::from_str(&line).unwrap();
    }

    // 5. Tool calls and their results survive with matching ids.
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    for e in &projected.entries {
        if let Entry::Message { message, .. } = e {
            match message {
                AgentMessage::Assistant { content, .. } => {
                    for b in content {
                        if let ContentBlock::ToolCall { id, .. } = b {
                            calls.insert(id.clone());
                        }
                    }
                }
                AgentMessage::ToolResult { tool_call_id, .. } => {
                    results.insert(tool_call_id.clone());
                }
                _ => {}
            }
        }
    }
    for id in ["call_1", "call_2"] {
        assert!(calls.contains(id), "missing tool call {id}");
        assert!(results.contains(id), "missing tool result for {id}");
    }
}
