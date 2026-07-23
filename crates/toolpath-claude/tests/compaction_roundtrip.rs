//! Compaction-event roundtrip: a Claude session that includes a
//! `compact_boundary` marker and a synthetic compaction summary should
//! still preserve the pre-compact and post-compact conversation
//! content through the projection round-trip.
//!
//! Synthetic fixture is justified per project policy: real compaction
//! events fire when the context window fills, which can't reliably be
//! triggered by a 5-minute capture prompt. A real production session
//! covering this code path would routinely run for hundreds of turns.
//!
//! What this test asserts (and why):
//!
//!   - The fixture loads cleanly (no parser crash on `compact_boundary`).
//!   - Pre-compact user/assistant content survives the round-trip.
//!   - Post-compact user/assistant content survives the round-trip.
//!   - The conversation can be re-projected to Claude JSONL and
//!     re-parsed by `ConversationReader` without error.
//!
//! Known limitation (documented, not asserted): the `compact_boundary`
//! marker entry itself has no `message` field, so the current
//! provider drops it on the floor going Claude → IR. The synthetic
//! `isCompactSummary: true` summary entry is currently surfaced as a
//! plain `Role::User` turn — `toolpath-claude` does not yet recognize
//! the `isCompactSummary` flag. Both are acceptable losses for "good
//! UX" today (the compacted summary text still lands in the
//! transcript), but if/when we tighten this, this test gets
//! tightened with it.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, derive_path, extract_conversation,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compacted_session.jsonl")
}

fn load_view() -> ConversationView {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read fixture");
    toolpath_claude::provider::to_view(&convo)
}

fn ir_roundtrip(view: &ConversationView) -> ConversationView {
    let path = derive_path(view, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let path = back.into_single_path().expect("single path");
    extract_conversation(&path)
}

#[test]
fn fixture_loads_without_panic() {
    let view = load_view();
    assert!(
        view.turns().next().is_some(),
        "compaction fixture should produce turns"
    );
}

#[test]
fn pre_compact_content_survives_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    let pre_user_text = "Help me refactor the auth module.";
    let pre_assistant_text = "I'll start by reading the current auth code.";

    assert!(
        original
            .turns()
            .any(|t| t.text.contains(pre_user_text)),
        "pre-compact user prompt missing from initial view"
    );
    assert!(
        original
            .turns()
            .any(|t| t.text.contains(pre_assistant_text)),
        "pre-compact assistant response missing from initial view"
    );

    assert!(
        after.turns().any(|t| t.text.contains(pre_user_text)),
        "pre-compact user prompt dropped after roundtrip"
    );
    assert!(
        after
            .turns()
            .any(|t| t.text.contains(pre_assistant_text)),
        "pre-compact assistant response dropped after roundtrip"
    );
}

#[test]
fn post_compact_content_survives_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    let post_user_text = "Now add the session validation.";
    let post_assistant_text = "Adding session validation to login().";
    let post_summary_text = "Done. The auth module now validates sessions on login.";

    for needle in [post_user_text, post_assistant_text, post_summary_text] {
        assert!(
            original.turns().any(|t| t.text.contains(needle)),
            "post-compact text {needle:?} missing from initial view"
        );
        assert!(
            after.turns().any(|t| t.text.contains(needle)),
            "post-compact text {needle:?} dropped after roundtrip"
        );
    }
}

#[test]
fn pre_compact_tool_call_pairs_survive_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    let target_id = "t-pre-1";
    let original_tool = original
        .turns()
        .find_map(|t| t.tool_uses.iter().find(|tu| tu.id == target_id))
        .expect("pre-compact tool call missing from initial view");
    let after_tool = after
        .turns()
        .find_map(|t| t.tool_uses.iter().find(|tu| tu.id == target_id))
        .expect("pre-compact tool call dropped after roundtrip");

    assert_eq!(original_tool.name, after_tool.name, "tool name diverged");
    let or = original_tool
        .result
        .as_ref()
        .expect("original result missing");
    let ar = after_tool
        .result
        .as_ref()
        .expect("result dropped after roundtrip");
    assert_eq!(or.content, ar.content, "tool result content diverged");
}

#[test]
fn post_compact_tool_call_pairs_survive_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    let target_id = "t-post-1";
    let original_tool = original
        .turns()
        .find_map(|t| t.tool_uses.iter().find(|tu| tu.id == target_id))
        .expect("post-compact tool call missing from initial view");
    let after_tool = after
        .turns()
        .find_map(|t| t.tool_uses.iter().find(|tu| tu.id == target_id))
        .expect("post-compact tool call dropped after roundtrip");

    assert_eq!(original_tool.name, after_tool.name, "tool name diverged");
    let or = original_tool
        .result
        .as_ref()
        .expect("original result missing");
    let ar = after_tool
        .result
        .as_ref()
        .expect("result dropped after roundtrip");
    assert_eq!(or.content, ar.content, "tool result content diverged");
}

#[test]
fn projector_output_is_re_parseable_by_reader() {
    let view = load_view();
    let after = ir_roundtrip(&view);
    let projector = ClaudeProjector;
    let convo = projector
        .project(&after)
        .expect("project to claude conversation");

    let mut lines: Vec<String> = Vec::new();
    for raw in &convo.preamble {
        lines.push(serde_json::to_string(raw).expect("serialize preamble"));
    }
    for entry in &convo.entries {
        lines.push(serde_json::to_string(entry).expect("serialize entry"));
    }

    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), lines.join("\n")).expect("write tempfile");
    ConversationReader::read_conversation(tmp.path()).expect("re-read projected JSONL");
}

#[test]
fn role_distribution_is_sane() {
    let view = load_view();
    let user_count = view
        .turns()
        .filter(|t| matches!(t.role, Role::User))
        .count();
    let assistant_count = view
        .turns()
        .filter(|t| matches!(t.role, Role::Assistant))
        .count();
    assert!(
        user_count >= 2,
        "expected at least 2 user turns, got {user_count}"
    );
    assert!(
        assistant_count >= 2,
        "expected at least 2 assistant turns, got {assistant_count}"
    );
}
