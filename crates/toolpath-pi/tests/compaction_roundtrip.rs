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
//!   - Each `Entry::Compaction` becomes an `Item::Compaction` at its
//!     position in the stream (not a synthetic `System` turn).
//!   - Pre-compact user/assistant content survives the round-trip.
//!   - Post-compact user/assistant content survives the round-trip.
//!   - The compaction items round-trip through
//!     `derive_path → extract_conversation` (a `conversation.compact`
//!     step in between), carrying `summary` and `pre_tokens`.
//!   - The conversation projects back to JSONL that re-parses through
//!     the Pi reader.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Item, derive_path, extract_conversation,
};
use toolpath_pi::project::PiProjector;
use toolpath_pi::{reader, session_to_view};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compacted_session.jsonl")
}

/// The real captured Pi session with two compaction boundaries.
fn real_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("pi")
        .join("convo-compacted.jsonl")
}

fn load_view() -> ConversationView {
    let session = reader::read_session_from_file(&fixture_path()).expect("read fixture");
    session_to_view(&session)
}

fn load_real_view() -> ConversationView {
    let session = reader::read_session_from_file(&real_fixture_path()).expect("read real fixture");
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
fn compaction_entry_becomes_compaction_item() {
    let view = load_view();
    let comps: Vec<&toolpath_convo::Compaction> =
        view.items.iter().filter_map(Item::as_compaction).collect();
    assert_eq!(comps.len(), 1, "synthetic fixture has one compaction");
    // No synthetic System turn stands in for the compaction.
    assert!(
        !view.turns().any(|t| t.text.starts_with("Compacted")),
        "compaction should not emit a synthetic turn"
    );
}

#[test]
fn real_fixture_has_two_compaction_items() {
    let view = load_real_view();
    let comps: Vec<&toolpath_convo::Compaction> =
        view.items.iter().filter_map(Item::as_compaction).collect();
    assert_eq!(comps.len(), 2, "real fixture has two compactions");
    for c in &comps {
        assert!(c.summary.is_some(), "summary should be carried");
        assert!(c.pre_tokens.is_some(), "pre_tokens should be carried");
        assert_eq!(c.trigger, None, "Pi doesn't persist auto-vs-manual");
        assert_eq!(c.kept.len(), 1, "one kept range per compaction");
    }
}

#[test]
fn real_fixture_compactions_and_turns_survive_roundtrip() {
    let original = load_real_view();
    let after = ir_roundtrip(&original);

    let comps_after = after.items.iter().filter_map(Item::as_compaction).count();
    assert_eq!(
        comps_after, 2,
        "both compactions survive derive → extract"
    );
    for c in after.items.iter().filter_map(Item::as_compaction) {
        assert!(c.summary.is_some(), "summary survives roundtrip");
        assert!(c.pre_tokens.is_some(), "pre_tokens survives roundtrip");
    }

    // Surrounding turns (pre- and post-compaction) survive too.
    for needle in [
        "walk through a small set of tasks",
        "Now print the single word: done.",
    ] {
        assert!(
            after.turns().any(|t| t.text.contains(needle)),
            "turn text {needle:?} dropped after roundtrip"
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

/// Direct projection round-trip on the real two-compaction fixture:
/// view → `PiProjector` → JSONL → reader → `session_to_view`. Both
/// `Item::Compaction`s must survive the projector reconstructing
/// `Entry::Compaction` from the `Compaction` fields, and they must stay
/// positioned between the surrounding turns.
#[test]
fn projector_reconstructs_compaction_entries() {
    let view = load_real_view();

    let session = PiProjector::new().project(&view).expect("project");

    // The projector must emit a real `Entry::Compaction` per
    // `Item::Compaction` (not fold them into turns).
    let emitted_compactions = session
        .entries
        .iter()
        .filter(|e| matches!(e, toolpath_pi::Entry::Compaction { .. }))
        .count();
    assert_eq!(
        emitted_compactions, 2,
        "projector should emit two compaction entries"
    );

    // Re-read the projected JSONL through the Pi reader and back into a view.
    let lines: Vec<String> = session
        .entries
        .iter()
        .map(|e| serde_json::to_string(e).expect("serialize pi entry"))
        .collect();
    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), lines.join("\n")).expect("write tempfile");
    let reread = reader::read_session_from_file(tmp.path()).expect("re-read projected JSONL");
    let after = session_to_view(&reread);

    let comps: Vec<&toolpath_convo::Compaction> =
        after.items.iter().filter_map(Item::as_compaction).collect();
    assert_eq!(comps.len(), 2, "both compactions survive projection");
    for c in &comps {
        assert!(c.summary.is_some(), "summary survives projection");
        assert!(c.pre_tokens.is_some(), "pre_tokens survives projection");
        assert_eq!(c.trigger, None, "Pi doesn't persist auto-vs-manual");
        assert_eq!(c.kept.len(), 1, "one kept range per compaction");
    }

    // Each compaction is positioned in the entry stream after the turns
    // it summarizes — never the first item, always preceded by a turn.
    let comp_indices: Vec<usize> = after
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.as_compaction().is_some())
        .map(|(idx, _)| idx)
        .collect();
    for &idx in &comp_indices {
        assert!(idx > 0, "compaction should not be the first item");
        assert!(
            after.items[..idx].iter().any(|i| i.as_turn().is_some()),
            "a turn precedes the compaction"
        );
    }
    // And at least one compaction sits strictly between two turns (the
    // first boundary in this fixture is followed by more conversation).
    assert!(
        comp_indices
            .iter()
            .any(|&idx| after.items[idx + 1..].iter().any(|i| i.as_turn().is_some())),
        "at least one compaction is followed by a turn"
    );
}
