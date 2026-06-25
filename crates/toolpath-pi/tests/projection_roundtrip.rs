//! End-to-end projection round-trip:
//! Pi `PiSession` → `ConversationView` → `Path` (serialized) →
//! `ConversationView` → `PiSession` via [`PiProjector`].
//!
//! Contract: after the full chain the projected session is
//! *functionally* equivalent to the source — same messages with the
//! same roles, content, tool calls, results, and token usage — and
//! the resulting JSONL re-parses through Pi's own `read_session_from_file`.
//!
//! Byte-level fidelity is not a requirement; some fields (cost
//! breakdown, ms-precision timestamps as u64, error_message vs absence)
//! may differ between the source and the round-tripped output.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use toolpath::v1::{Graph, Path};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_pi::project::PiProjector;
use toolpath_pi::reader::{PiSession, read_session_from_file};
use toolpath_pi::session_to_view;
use toolpath_pi::types::{AgentMessage, ContentBlock, Entry, ToolResultContent};

const FIXTURE: &str = include_str!("fixtures/basic_session.jsonl");

fn write_fixture(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("source.jsonl");
    fs::write(&path, FIXTURE).unwrap();
    path
}

fn load_source() -> (TempDir, PiSession) {
    let temp = TempDir::new().unwrap();
    let path = write_fixture(temp.path());
    let session = read_session_from_file(&path).expect("parse fixture");
    (temp, session)
}

/// Forward → reverse, exercising the same serialisation that a `.path`
/// file on disk would go through.
fn roundtrip(source: &PiSession) -> (ConversationView, PiSession, Path) {
    let view_forward: ConversationView = session_to_view(source);

    // Serialize & re-parse the Path to simulate on-disk storage.
    let path = derive_path(&view_forward, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single path");

    let view_back = extract_conversation(&reparsed);
    let projector = PiProjector::new().with_cwd(source.header.cwd.clone());
    let rebuilt = projector.project(&view_back).expect("project");
    (view_back, rebuilt, reparsed)
}

#[test]
fn roundtrip_preserves_session_header_id_and_cwd() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    assert_eq!(rebuilt.header.id, source.header.id);
    assert_eq!(rebuilt.header.cwd, source.header.cwd);
}

#[test]
fn roundtrip_preserves_message_count() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let source_msgs = source
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::Message { .. }))
        .count();
    let rebuilt_msgs = rebuilt
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::Message { .. }))
        .count();
    assert_eq!(rebuilt_msgs, source_msgs);
}

#[test]
fn roundtrip_preserves_user_message_text() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let collect_user_text = |s: &PiSession| -> Vec<String> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message {
                    message: AgentMessage::User { content, .. },
                    ..
                } => Some(match content {
                    toolpath_pi::types::MessageContent::Text(s) => s.clone(),
                    toolpath_pi::types::MessageContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                }),
                _ => None,
            })
            .collect()
    };
    assert_eq!(collect_user_text(&rebuilt), collect_user_text(&source));
}

#[test]
fn roundtrip_preserves_assistant_text_and_model() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let texts = |s: &PiSession| -> Vec<(String, String)> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message {
                    message: AgentMessage::Assistant { content, model, .. },
                    ..
                } => {
                    let t: String = content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some((model.clone(), t))
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(texts(&rebuilt), texts(&source));
}

#[test]
fn roundtrip_preserves_tool_calls_with_results() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    // Source: tool calls live as ContentBlock::ToolCall in assistant content;
    // their results live in a separate AgentMessage::ToolResult entry. Pull
    // (tool_call_id, name, args, result_text, is_error) tuples from each.
    fn extract_calls(s: &PiSession) -> Vec<(String, String, serde_json::Value)> {
        let mut out = Vec::new();
        for e in &s.entries {
            if let Entry::Message {
                message: AgentMessage::Assistant { content, .. },
                ..
            } = e
            {
                for b in content {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } = b
                    {
                        out.push((id.clone(), name.clone(), arguments.clone()));
                    }
                }
            }
        }
        out
    }

    fn extract_results(s: &PiSession) -> Vec<(String, String, bool)> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message {
                    message:
                        AgentMessage::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                            ..
                        },
                    ..
                } => {
                    let text: String = content
                        .iter()
                        .filter_map(|c| match c {
                            ToolResultContent::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some((tool_call_id.clone(), text, *is_error))
                }
                _ => None,
            })
            .collect()
    }

    let src_calls = extract_calls(&source);
    let rb_calls = extract_calls(&rebuilt);
    assert_eq!(rb_calls.len(), src_calls.len(), "tool call count mismatch");
    for (s, r) in src_calls.iter().zip(rb_calls.iter()) {
        assert_eq!(s.0, r.0, "tool_call_id mismatch");
        assert_eq!(s.1, r.1, "tool name mismatch");
        assert_eq!(s.2, r.2, "tool args mismatch");
    }

    let src_results = extract_results(&source);
    let rb_results = extract_results(&rebuilt);
    assert_eq!(rb_results.len(), src_results.len());
    for (s, r) in src_results.iter().zip(rb_results.iter()) {
        assert_eq!(s.0, r.0, "tool_call_id on result mismatch");
        assert_eq!(s.1, r.1, "result text mismatch");
        assert_eq!(s.2, r.2, "is_error mismatch");
    }
}

#[test]
fn roundtrip_preserves_token_usage_per_assistant_turn() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let usages = |s: &PiSession| -> Vec<(u64, u64, u64)> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message {
                    message: AgentMessage::Assistant { usage, .. },
                    ..
                } => Some((usage.input, usage.output, usage.cache_read)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(usages(&rebuilt), usages(&source));
}

#[test]
fn projected_jsonl_reparses_through_pi_reader() {
    // The strongest contract test: serialize the rebuilt session as
    // JSONL, write to disk, read back through Pi's own
    // `read_session_from_file`, and confirm the resulting structure
    // matches what we projected.
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let temp = TempDir::new().unwrap();
    let out_path = temp.path().join("rebuilt.jsonl");

    let mut lines: Vec<String> = Vec::with_capacity(rebuilt.entries.len());
    for entry in &rebuilt.entries {
        lines.push(serde_json::to_string(entry).unwrap());
    }
    fs::write(&out_path, lines.join("\n")).unwrap();

    let reread = read_session_from_file(&out_path).expect("Pi reader accepts our output");
    assert_eq!(reread.header.id, rebuilt.header.id);
    assert_eq!(reread.entries.len(), rebuilt.entries.len());
}
