//! Compaction-event roundtrip: a Codex rollout that includes a
//! `compacted` line in the middle should still preserve the
//! pre-compact and post-compact conversation content through the
//! projection round-trip.
//!
//! Synthetic fixture is justified per project policy: real compaction
//! fires when the model context window fills mid-session and can't
//! reliably be triggered by a 5-minute capture prompt.
//!
//! What this test asserts (and why):
//!
//!   - The fixture loads via `RolloutReader::read_session` without
//!     crashing on the `compacted` line.
//!   - Pre-compact user/assistant content survives the round-trip.
//!   - Post-compact user/assistant content survives the round-trip.
//!   - The `compacted` marker rides as an opaque event at its rollout
//!     position — between the pre- and post-compact turns — and keeps
//!     that position (and its `message` payload) through the
//!     derive → extract round-trip. Typing the boundary is the
//!     compaction-provenance follow-up's concern; position and payload
//!     survival are pinned here.
//!   - The conversation projects back to JSONL that re-parses through
//!     `RolloutReader`.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_codex::project::CodexProjector;
use toolpath_codex::{RolloutReader, to_view};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compacted_session.jsonl")
}

fn load_view() -> ConversationView {
    let session = RolloutReader::read_session(fixture_path()).expect("read fixture");
    to_view(&session)
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

    let needles = ["refactor the auth module", "reading the current auth code"];
    for n in needles {
        assert!(
            original.turns().any(|t| t.text.contains(n)),
            "pre-compact text {n:?} missing from initial view"
        );
        assert!(
            after.turns().any(|t| t.text.contains(n)),
            "pre-compact text {n:?} dropped after roundtrip"
        );
    }
}

#[test]
fn post_compact_content_survives_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    let needles = [
        "now add session validation",
        "added session validation to login()",
    ];
    for n in needles {
        assert!(
            original.turns().any(|t| t.text.contains(n)),
            "post-compact text {n:?} missing from initial view"
        );
        assert!(
            after.turns().any(|t| t.text.contains(n)),
            "post-compact text {n:?} dropped after roundtrip"
        );
    }
}

/// Item kinds in stream order: `T` for a turn, or the event's type.
fn item_shape(view: &ConversationView) -> Vec<String> {
    view.items
        .iter()
        .map(|item| match item {
            toolpath_convo::Item::Turn(_) => "T".to_string(),
            toolpath_convo::Item::Event(e) => e.event_type.clone(),
        })
        .collect()
}

#[test]
fn compacted_event_keeps_its_stream_position_through_roundtrip() {
    let original = load_view();
    let expected = [
        "session_meta",
        "task_started",
        "T",
        "T",
        "compacted",
        "T",
        "T",
        "task_complete",
    ];
    assert_eq!(
        item_shape(&original),
        expected,
        "items should preserve the rollout's interleaving"
    );

    let after = ir_roundtrip(&original);
    assert_eq!(
        item_shape(&after),
        expected,
        "interleaving should survive derive → extract"
    );

    let summary = "login() lacks session-token validation";
    let compacted = after
        .events()
        .find(|e| e.event_type == "compacted")
        .expect("compacted event survives roundtrip");
    assert!(
        serde_json::to_string(&compacted.data)
            .expect("serialize event data")
            .contains(summary),
        "compacted message payload should survive roundtrip"
    );
}

#[test]
fn projector_output_is_re_parseable_by_reader() {
    let view = load_view();
    let after = ir_roundtrip(&view);
    let projector = CodexProjector::new();
    let session = projector.project(&after).expect("project");

    let mut lines: Vec<String> = Vec::new();
    for line in &session.lines {
        lines.push(serde_json::to_string(line).expect("serialize rollout line"));
    }

    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), lines.join("\n")).expect("write tempfile");
    RolloutReader::read_session(tmp.path()).expect("re-read projected JSONL");
}
