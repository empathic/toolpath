//! Compaction handling for Codex rollouts.
//!
//! Codex appends a single `compacted` line when it condenses history
//! mid-session (same file, same session id). `toolpath-codex` now maps
//! that marker to an `Item::Compaction` positioned between the turns it
//! separates, rather than dropping it or surfacing it as a generic event.
//!
//! The marker payload is `{message, replacement_history?, window_id?}`
//! (see `docs/agents/formats/codex.md`). Only `message` is consumed, as
//! `Compaction.summary`. Codex never persists the manual-vs-auto trigger
//! or the pre-compaction token count, and we don't fold in
//! `replacement_history`, so `trigger`/`pre_tokens` are `None` and
//! `kept_from` is `None` (wholesale).
//!
//! Two fixtures:
//!   - synthetic `tests/fixtures/compacted_session.jsonl` — small,
//!     deterministic pre/post turns around one compaction. (Justified per
//!     project policy: real compaction fires only when the context window
//!     fills mid-session, which a short capture prompt can't reliably
//!     trigger.) Its `compacted` line uses the real
//!     `{message, replacement_history}` shape.
//!   - real `test-fixtures/codex/convo-compacted.jsonl` — a captured
//!     production rollout that actually compacted (empty `message`).
//!
//! What these tests assert:
//!   - The fixtures load via `RolloutReader::read_session` without
//!     crashing on the `compacted` line.
//!   - Exactly one `Item::Compaction` is emitted, with the field shape
//!     the Codex payload supports.
//!   - The compaction and its surrounding turns survive the
//!     `derive_path` → `extract_conversation` round-trip.
//!   - The conversation projects back to JSONL that re-parses through
//!     `RolloutReader`.

use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_codex::project::CodexProjector;
use toolpath_codex::{RolloutReader, to_view};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Item, derive_path, extract_conversation,
};

fn synthetic_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compacted_session.jsonl")
}

fn real_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("codex")
        .join("convo-compacted.jsonl")
}

fn load_view(path: PathBuf) -> ConversationView {
    let session = RolloutReader::read_session(path).expect("read fixture");
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

/// One native cycle: project the view to a Codex session, serialize it to
/// JSONL, re-read through `RolloutReader`, and run `to_view`.
fn native_roundtrip(view: &ConversationView) -> ConversationView {
    let session = CodexProjector::new().project(view).expect("project");
    let body = session
        .lines
        .iter()
        .map(|l| serde_json::to_string(l).expect("serialize rollout line"))
        .collect::<Vec<_>>()
        .join("\n");
    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), body).expect("write tempfile");
    let reread = RolloutReader::read_session(tmp.path()).expect("re-read projected JSONL");
    to_view(&reread)
}

/// Index of the single compaction in the item stream, asserting there is
/// exactly one.
fn sole_compaction_index(view: &ConversationView) -> usize {
    let indices: Vec<usize> = view
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it, Item::Compaction(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        indices.len(),
        1,
        "expected exactly one Item::Compaction, got {}",
        indices.len()
    );
    indices[0]
}

// ── Synthetic fixture ───────────────────────────────────────────────

#[test]
fn synthetic_fixture_loads_without_panic() {
    let view = load_view(synthetic_fixture_path());
    assert!(
        view.turns().next().is_some(),
        "compaction fixture should produce turns"
    );
}

#[test]
fn synthetic_emits_one_compaction_with_codex_field_shape() {
    let view = load_view(synthetic_fixture_path());
    let idx = sole_compaction_index(&view);
    let Item::Compaction(c) = &view.items[idx] else {
        unreachable!()
    };

    // `message` becomes the summary.
    assert_eq!(
        c.summary.as_deref(),
        Some(
            "Earlier in this session: read src/auth.rs, identified that login() lacks session-token validation."
        )
    );
    // Codex never persists trigger or pre-token count; we don't consume
    // replacement_history, so no kept anchor (wholesale).
    assert_eq!(c.trigger, None);
    assert_eq!(c.pre_tokens, None);
    assert!(c.kept_from.is_none());
    // Synthesized stable id, and a parent that links to the prior turn.
    assert_eq!(c.id, "compact-1");
    assert!(
        c.parent_id.is_some(),
        "compaction should parent on the prior turn"
    );

    // The compaction sits between the pre-compact and post-compact turns.
    let turn_idx_before = view.items[..idx]
        .iter()
        .rposition(|it| matches!(it, Item::Turn(_)));
    let turn_idx_after = view.items[idx + 1..]
        .iter()
        .position(|it| matches!(it, Item::Turn(_)));
    assert!(
        turn_idx_before.is_some(),
        "a turn should precede the compaction"
    );
    assert!(
        turn_idx_after.is_some(),
        "a turn should follow the compaction"
    );
}

#[test]
fn synthetic_compaction_and_turns_survive_roundtrip() {
    let original = load_view(synthetic_fixture_path());
    let after = ir_roundtrip(&original);

    // The compaction itself survives, with its summary intact.
    let idx = sole_compaction_index(&after);
    let Item::Compaction(c) = &after.items[idx] else {
        unreachable!()
    };
    assert!(
        c.summary
            .as_deref()
            .unwrap()
            .contains("session-token validation")
    );
    assert!(c.parent_id.is_some());

    // Surrounding pre/post turn content survives.
    let needles = [
        "refactor the auth module",
        "reading the current auth code",
        "now add session validation",
        "added session validation to login()",
    ];
    for n in needles {
        assert!(
            original.turns().any(|t| t.text.contains(n)),
            "text {n:?} missing from initial view"
        );
        assert!(
            after.turns().any(|t| t.text.contains(n)),
            "text {n:?} dropped after roundtrip"
        );
    }
}

#[test]
fn synthetic_projector_output_is_re_parseable_by_reader() {
    let view = load_view(synthetic_fixture_path());
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

#[test]
fn synthetic_projection_fixpoint() {
    let source = load_view(synthetic_fixture_path());
    let once = native_roundtrip(&source);
    let twice = native_roundtrip(&once);
    toolpath_convo::testing::assert_fixpoint(&source, &once, &twice);
}

// ── Real captured fixture ───────────────────────────────────────────

#[test]
fn real_fixture_emits_one_compaction() {
    let view = load_view(real_fixture_path());
    let idx = sole_compaction_index(&view);
    let Item::Compaction(c) = &view.items[idx] else {
        unreachable!()
    };

    // The real capture's `message` is the empty string, so summary is
    // `Some("")` — present but empty. The remaining fields follow the
    // Codex payload shape: no trigger, no pre-token count, no kept anchor.
    assert!(
        c.summary.is_some(),
        "summary should be Some (message field present, even if empty)"
    );
    assert_eq!(c.trigger, None);
    assert_eq!(c.pre_tokens, None);
    assert!(c.kept_from.is_none());
    assert!(
        c.parent_id.is_some(),
        "compaction should parent on the prior turn"
    );
    assert_eq!(c.id, "compact-1");
}

#[test]
fn real_fixture_compaction_survives_roundtrip() {
    let original = load_view(real_fixture_path());
    let pre_turns = original.turns().count();
    assert!(pre_turns > 0, "real fixture should have turns");

    let after = ir_roundtrip(&original);

    // Exactly one compaction survives the round-trip.
    let idx = sole_compaction_index(&after);
    let Item::Compaction(c) = &after.items[idx] else {
        unreachable!()
    };
    assert!(c.summary.is_some());
    assert!(c.parent_id.is_some());

    // Surrounding turns survive (count preserved through derive ↔ extract).
    assert_eq!(
        after.turns().count(),
        pre_turns,
        "turn count should survive the round-trip"
    );
}

#[test]
fn real_fixture_projector_output_is_re_parseable_by_reader() {
    let view = load_view(real_fixture_path());
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

#[test]
fn real_fixture_projection_fixpoint() {
    let source = load_view(real_fixture_path());
    let once = native_roundtrip(&source);
    let twice = native_roundtrip(&once);
    toolpath_convo::testing::assert_fixpoint(&source, &once, &twice);
}

/// View → Codex `Session` → JSONL → `Session` → view: the compaction
/// boundary the projector now emits as a `compacted` line must reappear
/// as exactly one `Item::Compaction` when the projected session is read
/// back, preserving the summary and its position between turns.
///
/// The real capture's `message` is empty, so the round-tripped summary is
/// `Some("")` rather than `None` — present but empty.
#[test]
fn real_fixture_projection_round_trips_compaction() {
    let original = load_view(real_fixture_path());
    let orig_idx = sole_compaction_index(&original);
    let Item::Compaction(orig) = &original.items[orig_idx] else {
        unreachable!()
    };
    let orig_summary = orig.summary.clone();

    // Project directly (no IR detour) so we exercise the projector's
    // `Item::Compaction` → `compacted` line path on its own.
    let session = CodexProjector::new().project(&original).expect("project");

    // Exactly one `compacted` line, carrying the summary as `message`.
    let compacted: Vec<&toolpath_codex::RolloutLine> = session
        .lines
        .iter()
        .filter(|l| l.kind == "compacted")
        .collect();
    assert_eq!(
        compacted.len(),
        1,
        "projector should emit exactly one compacted line"
    );
    assert_eq!(
        compacted[0].payload.get("message").and_then(|m| m.as_str()),
        orig_summary.as_deref(),
        "compacted line `message` should carry the compaction summary"
    );

    // Serialize one JSON line per rollout entry and read it back through
    // the crate's reader, then run the forward `to_view`.
    let body = session
        .lines
        .iter()
        .map(|l| serde_json::to_string(l).expect("serialize rollout line"))
        .collect::<Vec<_>>()
        .join("\n");
    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), body).expect("write tempfile");
    let reread = RolloutReader::read_session(tmp.path()).expect("re-read projected JSONL");
    let after = to_view(&reread);

    // Exactly one compaction survives, with the original summary intact
    // and no trigger (Codex never persists it).
    let idx = sole_compaction_index(&after);
    let Item::Compaction(c) = &after.items[idx] else {
        unreachable!()
    };
    assert_eq!(c.summary, orig_summary, "summary should round-trip");
    assert_eq!(c.trigger, None, "Codex never persists the trigger");
    assert!(c.pre_tokens.is_none());
    assert!(c.kept_from.is_none());

    // The compaction sits between turns: a turn precedes it and a turn
    // follows it in the re-read item stream.
    let turn_before = after.items[..idx]
        .iter()
        .rposition(|it| matches!(it, Item::Turn(_)));
    let turn_after = after.items[idx + 1..]
        .iter()
        .position(|it| matches!(it, Item::Turn(_)));
    assert!(
        turn_before.is_some(),
        "a turn should precede the round-tripped compaction"
    );
    assert!(
        turn_after.is_some(),
        "a turn should follow the round-tripped compaction"
    );
}
