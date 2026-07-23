//! Real-fixture projection round-trip:
//! Captured Pi session → `PiSession` → `ConversationView` → `Path`
//! (serialized) → `ConversationView` → `PiSession` via [`PiProjector`].
//!
//! Loads the shared real-world fixture at `test-fixtures/pi/convo.jsonl`
//! (refreshed via `scripts/capture-elicit-fixtures.sh`), runs it through
//! the full provider + projection pipeline, and asserts the projected
//! output is functionally equivalent to the source and re-parses
//! through `reader::read_session_from_file`.
//!
//! Complements `tests/projection_roundtrip.rs` (synthetic minimum-shape
//! tests) by running on production-shape input.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, Turn, derive_path,
    extract_conversation,
};
use toolpath_pi::project::PiProjector;
use toolpath_pi::{reader, session_to_view};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("pi")
        .join("convo.jsonl")
}

fn load_fixture_view() -> ConversationView {
    let session = reader::read_session_from_file(&fixture_path()).expect("read pi fixture");
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

fn is_system_envelope(turn: &Turn) -> bool {
    if !matches!(turn.role, Role::User) {
        return false;
    }
    let t = turn.text.trim_start();
    t.starts_with('<') && t.contains('>')
}

fn meaningful(view: &ConversationView) -> Vec<&Turn> {
    view.turns().filter(|t| !is_system_envelope(t)).collect()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn fixture_loads() {
    let view = load_fixture_view();
    assert!(
        view.turns().next().is_some(),
        "pi fixture should produce a non-empty view"
    );
    let m = meaningful(&view);
    assert!(
        m.iter().any(|t| matches!(t.role, Role::User)),
        "fixture should contain at least one meaningful user turn"
    );
    assert!(
        m.iter().any(|t| matches!(t.role, Role::Assistant)),
        "fixture should contain at least one assistant turn"
    );
}

#[test]
fn roundtrip_preserves_meaningful_turn_count_and_roles() {
    let original = load_fixture_view();
    let after = ir_roundtrip(&original);

    let o = meaningful(&original);
    let a = meaningful(&after);
    assert_eq!(
        o.len(),
        a.len(),
        "meaningful turn count diverged: original={} after={}",
        o.len(),
        a.len()
    );
    for (i, (x, y)) in o.iter().zip(a.iter()).enumerate() {
        assert_eq!(
            x.role, y.role,
            "role at meaningful turn {i}: {:?} vs {:?}",
            x.role, y.role
        );
    }
}

#[test]
fn roundtrip_preserves_turn_text() {
    let original = load_fixture_view();
    let after = ir_roundtrip(&original);

    for (i, (x, y)) in meaningful(&original)
        .iter()
        .zip(meaningful(&after).iter())
        .enumerate()
    {
        assert_eq!(
            norm(&x.text),
            norm(&y.text),
            "text at turn {i} diverged\n  original: {:?}\n  after:    {:?}",
            x.text,
            y.text
        );
    }
}

#[test]
fn roundtrip_preserves_tool_call_topology() {
    let original = load_fixture_view();
    let after = ir_roundtrip(&original);

    for (i, (x, y)) in meaningful(&original)
        .iter()
        .zip(meaningful(&after).iter())
        .enumerate()
    {
        if !matches!(x.role, Role::Assistant) {
            continue;
        }
        let xs: BTreeSet<&str> = x.tool_uses.iter().map(|t| t.id.as_str()).collect();
        let ys: BTreeSet<&str> = y.tool_uses.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            xs, ys,
            "tool_use id set diverged at turn {i}: {xs:?} vs {ys:?}"
        );

        for tx in &x.tool_uses {
            let ty = y
                .tool_uses
                .iter()
                .find(|t| t.id == tx.id)
                .unwrap_or_else(|| panic!("missing tool {} after roundtrip", tx.id));
            assert_eq!(tx.name, ty.name, "tool {} name diverged", tx.id);
            match (&tx.result, &ty.result) {
                (Some(rx), Some(ry)) => {
                    assert_eq!(
                        rx.content, ry.content,
                        "tool {} result content diverged",
                        tx.id
                    );
                    assert_eq!(rx.is_error, ry.is_error, "tool {} is_error diverged", tx.id);
                }
                (None, None) => {}
                (l, r) => panic!(
                    "tool {} result presence diverged: original={} after={}",
                    tx.id,
                    l.is_some(),
                    r.is_some()
                ),
            }
        }
    }
}

/// Delegation content (sub-agent work) survives self-roundtrip.
/// Vacuously passes when the fixture has no delegations; fires the
/// moment one is captured. Pi sessions can link to a parent via
/// `parentSession` — those land as `Turn.delegations` in the IR.
#[test]
fn roundtrip_preserves_delegations() {
    let original = load_fixture_view();
    let after = ir_roundtrip(&original);

    let total_before: usize = original.turns().map(|t| t.delegations.len()).sum();
    let total_after: usize = after.turns().map(|t| t.delegations.len()).sum();
    assert_eq!(
        total_before, total_after,
        "total delegation count diverged: {total_before} → {total_after}"
    );

    for (i, (a, b)) in original.turns().zip(after.turns()).enumerate() {
        assert_eq!(
            a.delegations.len(),
            b.delegations.len(),
            "turn {i} delegation count diverged"
        );
        for da in &a.delegations {
            let db = b
                .delegations
                .iter()
                .find(|d| d.agent_id == da.agent_id)
                .unwrap_or_else(|| panic!("delegation {} dropped at turn {i}", da.agent_id));
            assert_eq!(
                norm(&da.prompt),
                norm(&db.prompt),
                "delegation {} prompt diverged at turn {i}",
                da.agent_id
            );
            assert_eq!(
                da.turns.len(),
                db.turns.len(),
                "delegation {} child-turn count diverged at turn {i}",
                da.agent_id
            );
        }
    }
}

#[test]
fn projector_output_is_re_parseable_by_reader() {
    let view = load_fixture_view();
    let after = ir_roundtrip(&view);
    let projector = PiProjector::new();
    let session = projector.project(&after).expect("project to pi session");

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

fn compacted_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("pi")
        .join("compacted-real.jsonl")
}

/// Real compacted session (captured from pi interactively, 2026-07-21):
/// model changes, tool-call turns that create files, a manual compaction
/// with a six-turn kept run, and a post-compaction exchange. The kept
/// turns' steps carry `file.write` changes next to `conversation.append` —
/// the shape whose hash-order-dependent classification silently emptied
/// the wire `kept` list about one run in four. Looped: each iteration
/// builds fresh maps with fresh hash keys.
#[test]
fn compacted_fixture_derive_extract_derive_is_stable() {
    let session =
        reader::read_session_from_file(&compacted_fixture_path()).expect("read compacted fixture");
    let view = session_to_view(&session);
    let compaction = view
        .compactions()
        .next()
        .expect("fixture must contain a compaction");
    assert!(
        compaction.kept_from.is_some(),
        "fixture compaction must carry a kept anchor"
    );

    for _ in 0..32 {
        let gen1 = derive_path(&view, &DeriveConfig::default());
        let gen2 = derive_path(&extract_conversation(&gen1), &DeriveConfig::default());
        let v1 = serde_json::to_value(&gen1).expect("serialize gen1");
        let v2 = serde_json::to_value(&gen2).expect("serialize gen2");
        assert_eq!(v1, v2, "derive → extract → derive changed the document");

        let kept = gen1
            .steps
            .iter()
            .find_map(|s| {
                s.change.values().find_map(|ch| {
                    ch.structural
                        .as_ref()
                        .filter(|st| st.change_type == "conversation.compact")
                        .and_then(|st| st.extra.get("kept"))
                })
            })
            .expect("compact step must carry a kept list");
        assert_eq!(
            kept.as_array().map(Vec::len),
            Some(6),
            "kept run must cover all six pre-compaction turns"
        );
    }
}
