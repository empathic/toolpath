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
    view.turns
        .iter()
        .filter(|t| !is_system_envelope(t))
        .collect()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn fixture_loads() {
    let view = load_fixture_view();
    assert!(
        !view.turns.is_empty(),
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

    let total_before: usize = original.turns.iter().map(|t| t.delegations.len()).sum();
    let total_after: usize = after.turns.iter().map(|t| t.delegations.len()).sum();
    assert_eq!(
        total_before, total_after,
        "total delegation count diverged: {total_before} → {total_after}"
    );

    for (i, (a, b)) in original.turns.iter().zip(after.turns.iter()).enumerate() {
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

/// Pi never populates `Turn::file_mutations` (its provider always leaves it
/// `Vec::new()` -- see `crates/toolpath-pi/src/provider.rs`); every file
/// write instead falls through `derive_path`'s `FileWrite`-category
/// `tool_uses` fallback, which pulls the path out of the tool's raw JSON
/// `input` (`file_path`/`path`/`filename`/`file`, first match wins -- the
/// same field priority as `derive.rs`'s private `extract_file_path`,
/// reimplemented here since ground truth for this provider lives in
/// `tool_uses`, not `file_mutations`).
fn ground_truth_paths(view: &ConversationView) -> Vec<String> {
    view.turns
        .iter()
        .flat_map(|t| {
            t.tool_uses.iter().filter_map(|tool| {
                if tool.category != Some(toolpath_convo::ToolCategory::FileWrite) {
                    return None;
                }
                ["file_path", "path", "filename", "file"].iter().find_map(|field| {
                    tool.input
                        .get(*field)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            })
        })
        .collect()
}

/// Ground-truth invariant for #124 (relativized file-change keys) run
/// against a real recorded session: derive `view`, then for every
/// pre-derive file-write path assert (a) it produced a relativized key
/// iff it actually sat under `path.base` on a path-component boundary --
/// no absolute-under-base leak, and no wrongly-relativized outside-base
/// key -- and (b) extracting and re-deriving reproduces the identical
/// `file.write` key set (idempotency).
///
/// "Under base" is independently recomputed here via `std::path::Path`
/// component stripping rather than by calling `toolpath_convo`'s own
/// (private) `relativize_key`, so this exercises its output rather than
/// re-asserting its internals.
fn assert_file_write_keys_match_base(view: &ConversationView) {
    let path = derive_path(view, &DeriveConfig::default());
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|s| s.trim_end_matches('/').to_string());

    let ground_truth = ground_truth_paths(view);
    assert!(
        !ground_truth.is_empty(),
        "fixture must exercise at least one file write for this test to be meaningful"
    );

    let file_write_keys = |p: &toolpath::v1::Path| -> BTreeSet<String> {
        p.steps
            .iter()
            .flat_map(|s| s.change.iter())
            .filter(|(_, ch)| {
                ch.structural
                    .as_ref()
                    .is_some_and(|s| s.change_type == "file.write")
            })
            .map(|(k, _)| k.clone())
            .collect()
    };
    let derived_keys = file_write_keys(&path);

    for gt in &ground_truth {
        let under_base = base_root.as_deref().is_some_and(|root| {
            std::path::Path::new(gt)
                .strip_prefix(root)
                .is_ok_and(|rest| rest != std::path::Path::new(""))
        });
        if under_base {
            let root = base_root.as_deref().unwrap();
            let expected_relative = gt.strip_prefix(root).unwrap().trim_start_matches('/');
            assert!(
                derived_keys.contains(expected_relative),
                "expected relativized key {expected_relative:?} for {gt:?} under base {root:?}, got {derived_keys:?}"
            );
            assert!(
                !derived_keys.contains(gt.as_str()),
                "absolute-under-base leak: {gt:?} should have been relativized but the absolute form is still a key"
            );
        } else {
            assert!(
                derived_keys.contains(gt.as_str()),
                "expected {gt:?} to remain an absolute (or opaque) key outside the base, got {derived_keys:?}"
            );
        }
    }

    let view2 = extract_conversation(&path);
    let path2 = derive_path(&view2, &DeriveConfig::default());
    assert_eq!(
        derived_keys,
        file_write_keys(&path2),
        "re-derive must reproduce the identical file.write key set"
    );
}

#[test]
fn file_write_keys_relativized_with_no_leak_and_stable_on_re_derive() {
    // The captured fixture's tool-call file paths are already
    // provider-relative strings (e.g. "notes.md", "count.sh") --
    // `extract_file_path` returns them verbatim, with no cwd-join -- so
    // there's no absolute path in this session to relativize, and the
    // no-leak invariant would hold trivially without exercising real
    // stripping. Inject one synthetic absolute `FileWrite` tool call under
    // the fixture's own recorded working_dir to genuinely exercise
    // stripping against a real base, alongside the fixture's own
    // (already-relative, therefore untouched) tool calls.
    let mut view = load_fixture_view();
    let base_dir = view
        .base
        .as_ref()
        .and_then(|b| b.working_dir.clone())
        .expect("fixture records a working_dir");
    let turn = view
        .turns
        .iter_mut()
        .find(|t| {
            t.tool_uses
                .iter()
                .any(|tool| tool.category == Some(toolpath_convo::ToolCategory::FileWrite))
        })
        .expect("fixture has at least one FileWrite tool call");
    turn.tool_uses.push(toolpath_convo::ToolInvocation {
        id: "synthetic-tool-call".to_string(),
        name: "write".to_string(),
        input: serde_json::json!({
            "path": format!("{}/synthetic-nested/synth.rs", base_dir.trim_end_matches('/')),
        }),
        result: None,
        category: Some(toolpath_convo::ToolCategory::FileWrite),
    });

    assert_file_write_keys_match_base(&view);
}
