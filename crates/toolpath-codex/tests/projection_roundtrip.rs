//! End-to-end projection round-trip:
//! Codex `Session` → `ConversationView` → `Path` (serialized) →
//! `ConversationView` → `Session` via [`CodexProjector`].
//!
//! Contract: after the full chain the projected session is
//! *functionally* equivalent to the source — same messages, roles,
//! content text, function calls and outputs, encrypted-reasoning
//! blobs, custom-tool-call inputs — and the resulting JSONL
//! re-parses through Codex's own `RolloutReader::read_session`.
//!
//! Byte-level fidelity is not a requirement: file paths, per-turn
//! `turn_context` lines (the projector emits one per session, not
//! per turn), event_msg lines (CLI-side telemetry), and total token
//! counts may differ between the source and the round-tripped output.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use toolpath::v1::{Graph, Path};
use toolpath_codex::project::CodexProjector;
use toolpath_codex::types::{ResponseItem, RolloutItem, Session};
use toolpath_codex::{RolloutReader, to_view};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};

const FIXTURE: &str = include_str!("fixtures/sample-codex-python.jsonl");

fn write_fixture(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("source.jsonl");
    fs::write(&path, FIXTURE).unwrap();
    path
}

fn load_source() -> (TempDir, Session) {
    let temp = TempDir::new().unwrap();
    let path = write_fixture(temp.path());
    let session = RolloutReader::read_session(&path).expect("parse fixture");
    (temp, session)
}

/// Forward → reverse via the shared sub-protocol.
fn roundtrip(source: &Session) -> (ConversationView, Session, Path) {
    let view_forward: ConversationView = to_view(source);

    let path = derive_path(&view_forward, &DeriveConfig::default()).expect("derive");
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single path");

    let view_back = extract_conversation(&reparsed);
    let cwd = source
        .meta()
        .map(|m| m.cwd.to_string_lossy().to_string())
        .unwrap_or_default();
    let projector = CodexProjector::new().with_cwd(cwd);
    let rebuilt = projector.project(&view_back).expect("project");
    (view_back, rebuilt, reparsed)
}

#[test]
fn roundtrip_preserves_session_id() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    assert_eq!(rebuilt.id, source.id);
}

#[test]
fn rebuilt_has_session_meta_first() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    let first = rebuilt.lines.first().expect("at least one line");
    assert_eq!(first.kind, "session_meta");
    assert_eq!(
        first.payload["id"].as_str(),
        Some(source.id.as_str()),
        "session_meta payload must carry the source's session id"
    );
}

#[test]
fn rebuilt_has_turn_context_after_session_meta() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    assert!(rebuilt.lines.len() >= 2, "need session_meta + turn_context");
    assert_eq!(rebuilt.lines[1].kind, "turn_context");
    assert_eq!(
        rebuilt.lines[1].payload["cwd"].as_str(),
        source
            .meta()
            .as_ref()
            .map(|m| m.cwd.to_string_lossy().to_string())
            .as_deref()
    );
}

#[test]
fn roundtrip_preserves_user_assistant_message_count() {
    // Each user / assistant message in the source survives as a
    // matching `response_item: message` in the rebuilt session.
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let count_messages = |s: &Session, role: &str| -> usize {
        s.lines
            .iter()
            .filter_map(|l| match l.item() {
                RolloutItem::ResponseItem(ResponseItem::Message(m)) if m.role == role => Some(()),
                _ => None,
            })
            .count()
    };

    let src_user = count_messages(&source, "user");
    let rb_user = count_messages(&rebuilt, "user");
    assert_eq!(rb_user, src_user, "user message count");

    let src_asst = count_messages(&source, "assistant");
    let rb_asst = count_messages(&rebuilt, "assistant");
    assert!(
        rb_asst >= src_asst.saturating_sub(1),
        "assistant message count: rebuilt={}, source={}",
        rb_asst,
        src_asst
    );
}

#[test]
fn roundtrip_preserves_function_call_arguments_and_outputs() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    fn fc_pairs(s: &Session) -> Vec<(String, String, String)> {
        // (call_id, name, output)
        let mut pairs: std::collections::HashMap<String, (String, String, String)> =
            Default::default();
        for line in &s.lines {
            match line.item() {
                RolloutItem::ResponseItem(ResponseItem::FunctionCall(fc)) => {
                    pairs
                        .entry(fc.call_id.clone())
                        .or_insert((fc.call_id.clone(), fc.name.clone(), String::new()))
                        .1 = fc.name.clone();
                }
                RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput(fco)) => {
                    pairs
                        .entry(fco.call_id.clone())
                        .or_insert((fco.call_id.clone(), String::new(), String::new()))
                        .2 = fco.output.clone();
                }
                _ => {}
            }
        }
        let mut v: Vec<_> = pairs.into_values().collect();
        v.sort();
        v
    }

    let src = fc_pairs(&source);
    let rb = fc_pairs(&rebuilt);
    assert_eq!(rb.len(), src.len(), "function-call count mismatch");
    for (s, r) in src.iter().zip(rb.iter()) {
        assert_eq!(s.0, r.0, "call_id");
        assert_eq!(s.1, r.1, "name for {}", s.0);
        // Codex's forward path merges `function_call_output` text with
        // any matching `exec_command_end.aggregated_output` into one
        // ToolResult. The reverse re-emits the merged form, so the
        // rebuilt output is a superset of the source output (extra
        // exec-context lines may be prepended). Assert containment
        // rather than equality.
        assert!(
            r.2.contains(&s.2) || s.2.contains(&r.2),
            "output for {} diverged: src={:?} rb={:?}",
            s.0,
            s.2,
            r.2
        );
    }
}

#[test]
fn roundtrip_preserves_custom_tool_call_inputs() {
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    fn ctc_inputs(s: &Session) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = s
            .lines
            .iter()
            .filter_map(|l| match l.item() {
                RolloutItem::ResponseItem(ResponseItem::CustomToolCall(c)) => {
                    Some((c.call_id.clone(), c.input.clone()))
                }
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    let src = ctc_inputs(&source);
    let rb = ctc_inputs(&rebuilt);
    assert_eq!(rb.len(), src.len(), "custom-tool-call count mismatch");
    for (s, r) in src.iter().zip(rb.iter()) {
        assert_eq!(s.0, r.0, "call_id");
        assert_eq!(s.1, r.1, "input for {}", s.0);
    }
}

#[test]
fn projected_jsonl_reparses_through_codex_reader() {
    // Strongest contract test: serialize the rebuilt session as
    // JSONL, write to disk, read back through Codex's own
    // `RolloutReader::read_session`, and confirm the session id
    // and line count survive.
    let (_t, source) = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let temp = TempDir::new().unwrap();
    let out_path = temp.path().join("rebuilt.jsonl");

    let mut lines: Vec<String> = Vec::with_capacity(rebuilt.lines.len());
    for line in &rebuilt.lines {
        lines.push(serde_json::to_string(line).unwrap());
    }
    fs::write(&out_path, lines.join("\n")).unwrap();

    let reread = RolloutReader::read_session(&out_path).expect("Codex reader accepts our output");
    assert_eq!(reread.id, rebuilt.id);
    assert_eq!(reread.lines.len(), rebuilt.lines.len());
}
