//! Compaction-event roundtrip: a Pi session that includes an
//! `Entry::Compaction` line in the middle should still preserve the
//! pre-compact and post-compact conversation content through the
//! projection round-trip.
//!
//! Synthetic fixture is justified per project policy: real compaction
//! fires when the model context window fills mid-session and can't
//! reliably be triggered by a 5-minute capture prompt. Pi treats
//! compaction as a first-class entry type (alongside `BranchSummary`),
//! so the parser path differs meaningfully from a plain message-only
//! session — worth a regression test.
//!
//! What this test asserts (and why):
//!
//!   - The fixture loads via `reader::read_session_from_file` without
//!     crashing on the `Entry::Compaction` line.
//!   - Pre-compact user/assistant content survives the round-trip.
//!   - Post-compact user/assistant content survives the round-trip.
//!   - The conversation projects back to JSONL that re-parses through
//!     the Pi reader.
//!
//! Known limitation (documented, not asserted): the compaction marker
//! itself (with its `summary` text and `tokensBefore` metadata) lands
//! in `Turn.extra["pi"]["compaction"]` per the format docs, but the
//! full structural preservation through `derive → extract → project`
//! is not asserted here. Acceptable loss for "good UX" — the real
//! conversation content lives in the surrounding messages.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_pi::project::PiProjector;
use toolpath_pi::{reader, session_to_view};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compacted_session.jsonl")
}

fn load_view() -> ConversationView {
    let session = reader::read_session_from_file(&fixture_path()).expect("read fixture");
    session_to_view(&session)
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

#[test]
fn projector_output_is_re_parseable_by_reader() {
    let view = load_view();
    let after = ir_roundtrip(&view);
    let projector = PiProjector::new();
    let session = projector.project(&after).expect("project");

    let mut lines: Vec<String> = Vec::new();
    for entry in &session.entries {
        lines.push(serde_json::to_string(entry).expect("serialize pi entry"));
    }

    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), lines.join("\n")).expect("write tempfile");
    reader::read_session_from_file(tmp.path()).expect("re-read projected JSONL");
}
