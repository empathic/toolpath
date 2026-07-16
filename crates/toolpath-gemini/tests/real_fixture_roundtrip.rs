//! Real-fixture projection round-trip:
//! Captured Gemini chat → `Conversation` → `ConversationView` → `Path`
//! (serialized) → `ConversationView` → `Conversation` via
//! [`GeminiProjector`].
//!
//! Loads the shared real-world fixture at
//! `test-fixtures/gemini/convo.jsonl` (refreshed via
//! `scripts/capture-elicit-fixtures.sh`). Recent Gemini versions write
//! a JSONL stream — line 1 is a `ChatFile` header, subsequent lines are
//! either `GeminiMessage` entries (carry `type`) or `$set`-style
//! header-mutation events (skipped). Older versions wrote a single
//! JSON object; this test handles the JSONL form to match what the
//! capture script writes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, Turn, derive_path,
    extract_conversation,
};
use toolpath_gemini::ConversationReader;
use toolpath_gemini::project::GeminiProjector;
use toolpath_gemini::types::{ChatFile, Conversation, GeminiMessage};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("gemini")
        .join("convo.jsonl")
}

fn load_fixture_conversation() -> Conversation {
    let content = std::fs::read_to_string(fixture_path()).expect("read gemini fixture");
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().expect("gemini header line");
    let mut chat_file: ChatFile = serde_json::from_str(header).expect("parse gemini header");
    for line in lines {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").is_none() {
            continue;
        }
        if let Ok(msg) = serde_json::from_value::<GeminiMessage>(v) {
            chat_file.messages.push(msg);
        }
    }
    let session_uuid = chat_file.session_id.clone();
    Conversation::new(session_uuid, chat_file)
}

fn load_fixture_view() -> ConversationView {
    let convo = load_fixture_conversation();
    toolpath_gemini::provider::to_view(&convo)
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
        "gemini fixture should produce a non-empty view"
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
/// moment one is captured. Gemini surfaces sub-agents as sibling UUID
/// directories folded into `Turn.delegations` with populated child
/// turns — a flatten-bug here would be especially visible.
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
    let projector = GeminiProjector::default();
    let convo = projector
        .project(&after)
        .expect("project to gemini conversation");

    let json = serde_json::to_string_pretty(&convo.main).expect("serialize ChatFile");
    let tmp = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), &json).expect("write tempfile");
    ConversationReader::read_chat_file(tmp.path()).expect("re-read projected ChatFile");
}

/// Ground-truth invariant for #124 (relativized file-change keys) run
/// against a real recorded session: derive `view` under `config`, then for
/// every pre-derive `FileMutation::path` assert (a) it produced a
/// relativized key iff it actually sat under `path.base` on a path-
/// component boundary -- no absolute-under-base leak, and no wrongly-
/// relativized outside-base key -- and (b) extracting and re-deriving
/// reproduces the identical `file.write` key set (idempotency).
///
/// "Under base" is independently recomputed here via `std::path::Path`
/// component stripping rather than by calling `toolpath_convo`'s own
/// (private) `relativize_key`, so this exercises its output rather than
/// re-asserting its internals.
fn assert_file_write_keys_match_base(view: &ConversationView, config: &DeriveConfig) {
    let path = derive_path(view, config);
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|s| s.trim_end_matches('/').to_string());

    let ground_truth: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|t| t.file_mutations.iter().map(|fm| fm.path.as_str()))
        .collect();
    assert!(
        !ground_truth.is_empty(),
        "fixture must exercise at least one file mutation for this test to be meaningful"
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

    // Track that the under-base branch -- the exact case #124 relativizes --
    // actually fires at least once, so a fixture (or a regression) with no
    // absolute-under-base path can't let this test silently no-op the
    // invariant it exists to guard.
    let mut saw_under_base = false;
    for gt in &ground_truth {
        let under_base = base_root.as_deref().is_some_and(|root| {
            std::path::Path::new(gt)
                .strip_prefix(root)
                .is_ok_and(|rest| rest != std::path::Path::new(""))
        });
        if under_base {
            saw_under_base = true;
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
    assert!(
        saw_under_base,
        "test must exercise at least one absolute-under-base key -- the invariant #124 changed"
    );

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
    // The captured fixture's `to_view()` conversation carries no
    // `directories`/`project_path`, AND Gemini's own tool-call args
    // record `file_path` as an already-relative string (e.g. "notes.md")
    // -- there's no absolute path anywhere in this session to
    // relativize, so the no-leak invariant would hold trivially without
    // exercising real stripping. Inject one synthetic absolute mutation
    // under a chosen base to genuinely exercise stripping, alongside the
    // fixture's own (already-relative, therefore untouched) mutations.
    let mut view = load_fixture_view();
    let turn = view
        .turns
        .iter_mut()
        .find(|t| !t.file_mutations.is_empty())
        .expect("fixture has at least one turn with file mutations");
    turn.file_mutations.push(toolpath_convo::FileMutation {
        path: "/synthetic-base/nested/synth.rs".to_string(),
        operation: Some("update".into()),
        raw_diff: Some("<diff>".into()),
        ..Default::default()
    });

    let config = DeriveConfig {
        base_uri: Some("file:///synthetic-base".to_string()),
        ..Default::default()
    };
    assert_file_write_keys_match_base(&view, &config);
}
