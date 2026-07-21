//! Compaction-boundary detection: loading the real captured Claude session
//! with an inline `compact_boundary` marker should surface exactly one
//! `Item::Compaction` at its true position in the ordered item stream — the
//! boundary's `compactMetadata` becomes the `Compaction`, the synthetic
//! `isCompactSummary` entry is folded into `Compaction.summary` (not surfaced
//! as a turn), and the surrounding turns are preserved.
//!
//! The fixture is `test-fixtures/claude/convo-compacted.jsonl` — a real Claude
//! Code 2.1.x session captured while running `/compact` (manual trigger).

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{
    CompactionTrigger, ConversationProjector, ConversationView, DeriveConfig, Item, derive_path,
    expand_kept, extract_conversation, testing::assert_fixpoint,
};

/// The real captured Claude session with one manual compaction boundary.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join("convo-compacted.jsonl")
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

/// Project the view back into a Claude `Conversation`, then re-read it with
/// the forward path. The compaction must survive: project re-emits the
/// boundary (+ summary) entries, and `to_view` re-folds them into one
/// `Item::Compaction`.
fn project_and_reread(view: &ConversationView) -> ConversationView {
    let convo = ClaudeProjector.project(view).expect("project view");
    toolpath_claude::provider::to_view(&convo)
}

fn only_compaction(view: &ConversationView) -> &toolpath_convo::Compaction {
    let compactions: Vec<_> = view.items.iter().filter_map(Item::as_compaction).collect();
    assert_eq!(
        compactions.len(),
        1,
        "expected exactly one Item::Compaction, found {}",
        compactions.len()
    );
    compactions[0]
}

#[test]
fn boundary_becomes_single_compaction_item_with_expected_fields() {
    let view = load_view();
    let c = only_compaction(&view);

    assert_eq!(
        c.trigger,
        Some(CompactionTrigger::Manual),
        "fixture ran /compact (manual trigger)"
    );
    assert!(
        c.summary.is_some(),
        "summary folded from isCompactSummary entry"
    );
    assert!(
        c.pre_tokens.is_some(),
        "preTokens carried from compactMetadata"
    );
    let preserved = vec![
        "8a1c3178-ba2b-43cc-a376-3ad159a03d25".to_string(),
        "1b85db73-91ac-4095-a45e-6feb3e495282".to_string(),
    ];
    assert_eq!(
        c.kept_from.as_deref(),
        Some("8a1c3178-ba2b-43cc-a376-3ad159a03d25"),
        "kept_from = the oldest preserved turn on the boundary's parent chain"
    );
    assert_eq!(
        expand_kept(&view.items, c),
        preserved,
        "the anchor expands to the fixture's contiguous preserved tail \
         (compactMetadata.preservedMessages.uuids)"
    );
    assert!(
        c.parent_id.is_some(),
        "logicalParentUuid maps to the compaction's parent"
    );
}

#[test]
fn compaction_lands_between_surrounding_turns() {
    let view = load_view();

    let compaction_pos = view
        .items
        .iter()
        .position(|i| matches!(i, Item::Compaction(_)))
        .expect("compaction present");

    // There must be at least one turn before and after the boundary.
    let turns_before = view.items[..compaction_pos]
        .iter()
        .filter(|i| matches!(i, Item::Turn(_)))
        .count();
    let turns_after = view.items[compaction_pos + 1..]
        .iter()
        .filter(|i| matches!(i, Item::Turn(_)))
        .count();
    assert!(turns_before > 0, "pre-compaction turns missing");
    assert!(turns_after > 0, "post-compaction turns missing");
}

#[test]
fn summary_entry_is_not_surfaced_as_a_turn() {
    let view = load_view();
    let c = only_compaction(&view);
    let summary = c.summary.as_deref().expect("summary present");

    // The synthetic summary's text must live on the Compaction, not on any
    // turn (it was folded, not emitted).
    let summary_head = &summary[..summary.len().min(60)];
    for turn in view.turns() {
        assert!(
            !turn.text.contains(summary_head),
            "summary text leaked into a turn: {:?}",
            turn.id
        );
    }
}

#[test]
fn compaction_roundtrips_through_derive_and_extract() {
    let original = load_view();
    let orig_c = only_compaction(&original).clone();

    let after = ir_roundtrip(&original);
    let after_c = only_compaction(&after);

    assert_eq!(after_c.id, orig_c.id, "compaction id diverged");
    assert_eq!(after_c.trigger, orig_c.trigger, "trigger diverged");
    assert_eq!(after_c.summary, orig_c.summary, "summary diverged");
    assert_eq!(after_c.pre_tokens, orig_c.pre_tokens, "pre_tokens diverged");
    assert_eq!(after_c.kept_from, orig_c.kept_from, "kept_from diverged");
    assert_eq!(
        expand_kept(&after.items, after_c),
        expand_kept(&original.items, &orig_c),
        "kept runs diverged"
    );
    assert_eq!(after_c.parent_id, orig_c.parent_id, "parent_id diverged");
}

#[test]
fn surrounding_turns_survive_roundtrip() {
    let original = load_view();
    let after = ir_roundtrip(&original);

    // A turn from before the boundary and one from after should both survive
    // the derive→extract roundtrip. Use the first and last user turns as
    // stable anchors keyed by id.
    let orig_turn_ids: Vec<String> = original.turns().map(|t| t.id.clone()).collect();
    assert!(orig_turn_ids.len() >= 2, "need at least two turns to test");

    let after_turn_ids: std::collections::HashSet<String> =
        after.turns().map(|t| t.id.clone()).collect();

    let first = &orig_turn_ids[0];
    let last = orig_turn_ids.last().unwrap();
    assert!(
        after_turn_ids.contains(first),
        "first turn {first} dropped after roundtrip"
    );
    assert!(
        after_turn_ids.contains(last),
        "last turn {last} dropped after roundtrip"
    );
}

#[test]
fn compaction_survives_projection_roundtrip() {
    let original = load_view();
    let orig_c = only_compaction(&original).clone();

    // view → project (emit boundary + summary entries) → to_view (re-fold).
    // The shared oracle asserts the full contract: idempotency (a second
    // cycle is the identity), summary/trigger/kept-anchor/boundary-position
    // survival, and structural invariants on both output views.
    let after = project_and_reread(&original);
    let twice = project_and_reread(&after);
    assert_fixpoint(&original, &after, &twice);

    // Claude→Claude projection preserves turn uuids, so the parent chain
    // must survive verbatim — including the first post-boundary turn, which
    // chains through the compaction on both sides of the trip.
    let parents = |v: &ConversationView| -> Vec<(String, Option<String>)> {
        v.turns()
            .map(|t| (t.id.clone(), t.parent_id.clone()))
            .collect()
    };
    assert_eq!(
        parents(&original),
        parents(&after),
        "turn parent chain changed across projection"
    );

    let after_c = only_compaction(&after);

    assert_eq!(after_c.trigger, orig_c.trigger, "trigger diverged");
    assert_eq!(
        after_c.summary.is_some(),
        orig_c.summary.is_some(),
        "summary presence diverged"
    );
    assert_eq!(after_c.pre_tokens, orig_c.pre_tokens, "pre_tokens diverged");
    assert_eq!(after_c.kept_from, orig_c.kept_from, "kept_from diverged");
    assert_eq!(
        expand_kept(&after.items, after_c),
        expand_kept(&original.items, &orig_c),
        "kept runs diverged"
    );

    // The re-folded compaction must sit between turns, not at an edge.
    let pos = after
        .items
        .iter()
        .position(|i| matches!(i, Item::Compaction(_)))
        .expect("compaction present after projection roundtrip");
    let turns_before = after.items[..pos]
        .iter()
        .filter(|i| matches!(i, Item::Turn(_)))
        .count();
    let turns_after = after.items[pos + 1..]
        .iter()
        .filter(|i| matches!(i, Item::Turn(_)))
        .count();
    assert!(turns_before > 0, "no pre-compaction turn after projection");
    assert!(turns_after > 0, "no post-compaction turn after projection");

    // The summary text must not have leaked into any turn — it stays folded
    // on the Compaction.
    let summary = after_c.summary.as_deref().expect("summary present");
    let summary_head = &summary[..summary.len().min(60)];
    for turn in after.turns() {
        assert!(
            !turn.text.contains(summary_head),
            "summary text leaked into a turn after projection: {:?}",
            turn.id
        );
    }
}

/// The re-emission strip keeps step ids unique so `derive_path` succeeds, the
/// boundary's `kept_from` anchor resolves to a non-empty kept run, every
/// surviving turn appears exactly once, and the compaction survives a
/// project → re-read roundtrip with the same anchor and run.
#[test]
fn re_emission_is_stripped_and_kept_round_trips() {
    use std::collections::HashSet;

    let view = load_view();

    // Forward: derive_path must NOT error on duplicate step ids — the
    // re-emitted (duplicate-uuid) entries were stripped during `to_view`.
    let path = derive_path(&view, &DeriveConfig::default());
    let mut ids = HashSet::new();
    for step in &path.steps {
        assert!(
            ids.insert(step.step.id.clone()),
            "duplicate step id leaked through: {}",
            step.step.id
        );
    }

    // Every turn in the view appears exactly once (re-emission stripped).
    let mut turn_ids = HashSet::new();
    for turn in view.turns() {
        assert!(
            turn_ids.insert(turn.id.clone()),
            "turn {} appears more than once — re-emission not stripped",
            turn.id
        );
    }

    // The kept anchor is populated and expands to a non-empty run.
    let c = only_compaction(&view);
    assert!(c.kept_from.is_some(), "kept_from anchor should be resolved");
    assert!(
        !expand_kept(&view.items, c).is_empty(),
        "kept run should be non-empty"
    );

    // Reverse: project → re-read. The compaction survives with the same
    // anchor and kept run, and re-reading still produces unique step ids.
    let after = project_and_reread(&view);
    let after_c = only_compaction(&after);
    assert_eq!(
        after_c.kept_from, c.kept_from,
        "kept_from diverged after projection"
    );
    assert_eq!(
        expand_kept(&after.items, after_c),
        expand_kept(&view.items, c),
        "kept run diverged after projection"
    );

    let path2 = derive_path(&after, &DeriveConfig::default());
    let mut ids2 = HashSet::new();
    for step in &path2.steps {
        assert!(
            ids2.insert(step.step.id.clone()),
            "duplicate step id after projection roundtrip: {}",
            step.step.id
        );
    }
}
