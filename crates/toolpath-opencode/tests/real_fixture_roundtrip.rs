//! Real-fixture projection round-trip:
//! Captured opencode session → `Session` → `ConversationView` → `Path`
//! (serialized) → `ConversationView` → `Session` via [`OpencodeProjector`].
//!
//! Loads the shared real-world fixture at `test-fixtures/opencode/convo.json`
//! (refreshed via `scripts/capture-elicit-fixtures.sh`). The capture
//! script writes `path export opencode` output, which is the camelCase +
//! nested-`info` wrapper format. The local `parse_opencode_export`
//! helper translates that into the snake-case flat `Session` shape that
//! `to_view` expects.
//!
//! Wire-level (SQLite) roundtrip is out of scope here — opencode's
//! `tests/projection_roundtrip.rs` covers projection against an
//! in-memory database via `BASIC_SQL`. This test exercises the same
//! contract using a real exported session.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, Turn, derive_path,
    extract_conversation,
};
use toolpath_opencode::project::OpencodeProjector;
use toolpath_opencode::to_view;
use toolpath_opencode::types::{Message, MessageData, Part, PartData, Session};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("opencode")
        .join("convo.json")
}

/// Convert opencode's `path export` JSON wrapper format into the
/// `Session` struct shape that `to_view` expects. The wrapper uses
/// camelCase + nested `info` blocks; the Session uses snake_case +
/// flat row columns. Mirrors the helper in
/// `crates/path-cli/tests/cross_harness_matrix.rs`.
fn parse_opencode_export(json: &str) -> Session {
    let v: Value = serde_json::from_str(json).expect("opencode wrapper parse");
    let info = &v["info"];
    let msgs_in = v["messages"].as_array().cloned().unwrap_or_default();

    let str_or = |key: &str, fallback: &str| -> String {
        info.get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let i64_at = |path: &[&str]| -> Option<i64> {
        let mut cur = info;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_i64()
    };

    let mut messages: Vec<Message> = Vec::with_capacity(msgs_in.len());
    for m in msgs_in {
        let mi = m.get("info").cloned().unwrap_or(Value::Null);
        let mi_obj = mi.as_object().cloned().unwrap_or_default();
        let id = mi_obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = mi_obj
            .get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let time_created = mi_obj
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let mut data_obj = mi_obj.clone();
        data_obj.remove("id");
        data_obj.remove("sessionID");
        let data: MessageData =
            serde_json::from_value(Value::Object(data_obj)).unwrap_or(MessageData::Other);

        let mut parts: Vec<Part> = Vec::new();
        if let Some(parts_in) = m.get("parts").and_then(Value::as_array) {
            for p in parts_in {
                let p_obj = p.as_object().cloned().unwrap_or_default();
                let pid = p_obj
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let pmsg = p_obj
                    .get("messageID")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                let psess = p_obj
                    .get("sessionID")
                    .and_then(Value::as_str)
                    .unwrap_or(&session_id)
                    .to_string();
                let mut data_obj = p_obj.clone();
                data_obj.remove("id");
                data_obj.remove("messageID");
                data_obj.remove("sessionID");
                let part_data: PartData =
                    serde_json::from_value(Value::Object(data_obj)).unwrap_or(PartData::Unknown);
                parts.push(Part {
                    id: pid,
                    message_id: pmsg,
                    session_id: psess,
                    time_created,
                    time_updated: time_created,
                    data: part_data,
                });
            }
        }

        messages.push(Message {
            id,
            session_id,
            time_created,
            time_updated: time_created,
            data,
            parts,
        });
    }

    Session {
        id: str_or("id", ""),
        project_id: str_or("projectID", ""),
        workspace_id: info
            .get("workspaceID")
            .and_then(Value::as_str)
            .map(str::to_string),
        parent_id: info
            .get("parentID")
            .and_then(Value::as_str)
            .map(str::to_string),
        slug: str_or("slug", ""),
        directory: PathBuf::from(str_or("directory", "/")),
        title: str_or("title", ""),
        version: str_or("version", "0.0.0"),
        share_url: info
            .get("shareURL")
            .and_then(Value::as_str)
            .map(str::to_string),
        summary_additions: i64_at(&["summary", "additions"]),
        summary_deletions: i64_at(&["summary", "deletions"]),
        summary_files: i64_at(&["summary", "files"]),
        time_created: i64_at(&["time", "created"]).unwrap_or(0),
        time_updated: i64_at(&["time", "updated"])
            .or_else(|| i64_at(&["time", "created"]))
            .unwrap_or(0),
        time_compacting: i64_at(&["time", "compacting"]),
        time_archived: i64_at(&["time", "archived"]),
        messages,
    }
}

fn load_fixture_session() -> Session {
    let json = std::fs::read_to_string(fixture_path()).expect("read opencode fixture");
    parse_opencode_export(&json)
}

fn load_fixture_view() -> ConversationView {
    let session = load_fixture_session();
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
        "opencode fixture should produce a non-empty view"
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
/// moment one is captured. opencode's `subtask` part type maps to
/// `Turn.delegations`.
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
fn projected_session_is_json_serde_symmetric() {
    let view = load_fixture_view();
    let after = ir_roundtrip(&view);
    let projector = OpencodeProjector::new();
    let session = projector
        .project(&after)
        .expect("project to opencode session");

    let json = serde_json::to_string(&session).expect("serialize Session");
    let _back: Session = serde_json::from_str(&json).expect("re-parse Session");
}

/// `Builder::compute_turn_mutations` runs unconditionally (even under
/// plain `to_view`, which opens no snapshot repo) -- when there's no
/// snapshot diff to draw from, it falls back to a tool-input-derived
/// `FileMutation` per `FileWrite` tool call via opencode's own
/// `tool_input_file_path` (which recognizes the `filePath` field
/// opencode's tools actually use). So, unlike pi/gemini, ground truth
/// for this provider lives in `Turn::file_mutations`.
fn ground_truth_paths(view: &ConversationView) -> Vec<&str> {
    view.turns
        .iter()
        .flat_map(|t| t.file_mutations.iter().map(|fm| fm.path.as_str()))
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
#[test]
fn file_write_keys_relativized_with_no_leak_and_stable_on_re_derive() {
    let view = load_fixture_view();
    let path = derive_path(&view, &DeriveConfig::default());
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|s| s.trim_end_matches('/').to_string());

    let ground_truth = ground_truth_paths(&view);
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
                !derived_keys.contains(*gt),
                "absolute-under-base leak: {gt:?} should have been relativized but the absolute form is still a key"
            );
        } else {
            assert!(
                derived_keys.contains(*gt),
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
