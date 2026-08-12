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
//!   - The `compact_boundary` marker becomes an `Item::Compaction` in
//!     `view.items` at its stream position (after the last pre-compact
//!     turn, before the post-compact turns), and holds that position
//!     through derive → extract with its id intact.
//!   - The conversation can be re-projected to Claude JSONL and
//!     re-parsed by `ConversationReader` without error.
//!
//! Boundary handling — asserted in depth by `compaction_view.rs` — is now
//! first-class: the `compact_boundary` marker is read as an
//! `Item::Compaction`, and the synthetic `isCompactSummary: true` entry is
//! folded into that boundary's `summary` rather than surfaced as a
//! `Role::User` turn. This test covers the complement: the *surrounding*
//! content — the pre- and post-compact turns and their tool-call pairs —
//! surviving the derive → project → re-read round-trip intact.
//!
//! Known limitation: the fold applies only when a `compact_boundary`
//! immediately precedes the summary entry. An orphan `isCompactSummary`
//! entry with no inline boundary — the old-style rotation-chain shape,
//! where a continuation file opens with the summary alone — surfaces as a
//! plain user turn, and projecting that turn back to JSONL drops its
//! `isCompactSummary` and `isVisibleInTranscriptOnly` flags.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Item, Role, derive_path,
    extract_conversation,
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

fn project_and_reread(view: &ConversationView) -> ConversationView {
    let convo = ClaudeProjector.project(view).expect("project view");
    toolpath_claude::provider::to_view(&convo)
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
        original.turns().any(|t| t.text.contains(pre_user_text)),
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
        after.turns().any(|t| t.text.contains(pre_assistant_text)),
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
fn compact_boundary_survives_at_stream_position() {
    // Position among the turns — not resolved step parents — is the
    // round-trip contract asserted here: the boundary's `Item::Compaction`
    // must sit after the two pre-compact turns and before the first
    // post-compact turn, both in the initial view and after derive → extract.
    let original = load_view();
    let after = ir_roundtrip(&original);

    for (label, view) in [("initial view", &original), ("extracted view", &after)] {
        let idx = view
            .items
            .iter()
            .position(|item| matches!(item, Item::Compaction(_)))
            .unwrap_or_else(|| panic!("Item::Compaction missing from {label}"));

        let compaction = view.items[idx].as_compaction().unwrap();
        assert_eq!(
            compaction.id, "uuid-boundary",
            "compaction id diverged in {label}"
        );
        assert!(
            compaction
                .summary
                .as_deref()
                .is_some_and(|s| s.contains("[compacted summary]")),
            "isCompactSummary text must stay folded into the boundary in {label}"
        );

        let turns_before = view.items[..idx].iter().filter_map(Item::as_turn).count();
        assert_eq!(
            turns_before, 2,
            "boundary must follow the two pre-compact turns in {label}"
        );

        let next_turn = view.items[idx + 1..]
            .iter()
            .find_map(Item::as_turn)
            .unwrap_or_else(|| panic!("no turn after the boundary in {label}"));
        assert!(
            next_turn.text.contains("Now add the session validation."),
            "boundary must precede the first post-compact turn in {label}, got {:?}",
            next_turn.text
        );
    }
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
fn projection_fixpoint_holds() {
    // Shared oracle: one projection may normalize (re-mint the summary uuid,
    // reorder events), but a second cycle must be the identity, the
    // compaction payload must survive the first, and both output views must
    // satisfy the structural invariants.
    let source = load_view();
    let once = project_and_reread(&source);
    let twice = project_and_reread(&once);
    toolpath_convo::testing::assert_fixpoint(&source, &once, &twice);

    // Claude→Claude projection preserves turn uuids, so the parent chain
    // must survive verbatim — including the first post-boundary turn, which
    // chains through the compaction on both sides of the trip.
    let parents = |v: &ConversationView| -> Vec<(String, Option<String>)> {
        v.turns()
            .map(|t| (t.id.clone(), t.parent_id.clone()))
            .collect()
    };
    assert_eq!(
        parents(&source),
        parents(&once),
        "turn parent chain changed across projection"
    );
}

#[test]
fn projected_summary_carries_render_flags() {
    let view = load_view();
    let convo = ClaudeProjector.project(&view).expect("project view");
    let summary = convo
        .entries
        .iter()
        .find(|e| e.extra.get("isCompactSummary") == Some(&serde_json::json!(true)))
        .expect("projected conversation should contain a compact-summary entry");
    assert_eq!(
        summary.extra.get("isVisibleInTranscriptOnly"),
        Some(&serde_json::json!(true)),
        "summary must be transcript-only or the TUI renders it inline on resume"
    );
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
