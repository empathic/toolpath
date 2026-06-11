//! Real-fixture projection round-trip:
//! Captured Claude session → `Conversation` → `ConversationView` →
//! `Path` (serialized) → `ConversationView` → `Conversation` via
//! [`ClaudeProjector`].
//!
//! Loads the shared real-world fixture at
//! `test-fixtures/claude/convo.jsonl` (refreshed via
//! `scripts/capture-elicit-fixtures.sh`), runs it through the full
//! provider + projection pipeline, and asserts the projected output is
//! functionally equivalent to the source — same meaningful turns, same
//! tool-call topology, same per-turn text — and re-parses through the
//! reader (i.e. is loadable as a real Claude JSONL).
//!
//! Complements `tests/projection_roundtrip.rs`-style synthetic tests
//! (none today for claude) by exercising the projector on production
//! shape rather than a hand-built minimum.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toolpath::v1::Graph;
use toolpath_claude::{ClaudeProjector, ConversationReader};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Role, Turn, derive_path,
    extract_conversation,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join("convo.jsonl")
}

fn load_fixture_view() -> ConversationView {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read claude fixture");
    toolpath_claude::provider::to_view(&convo)
}

fn ir_roundtrip(view: &ConversationView) -> ConversationView {
    let path = derive_path(view, &DeriveConfig::default()).expect("derive");
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let path = back.into_single_path().expect("single path");
    extract_conversation(&path)
}

/// User-text-only system envelopes (e.g. `<environment_context>` blobs)
/// don't carry meaningful conversation content; filter them out before
/// per-turn comparisons so we don't false-fail on harness-internal
/// chatter that gets canonicalized away.
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
        "claude fixture should produce a non-empty view"
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
/// moment one is captured. Sub-agent dispatch is part of normal
/// production usage — flattening it would be a visible UX regression.
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
fn roundtrip_preserves_total_token_usage_when_present() {
    let original = load_fixture_view();
    let after = ir_roundtrip(&original);

    if let Some(o) = &original.total_usage {
        let a = after
            .total_usage
            .as_ref()
            .expect("total_usage dropped after roundtrip");
        assert_eq!(o.input_tokens, a.input_tokens, "input_tokens diverged");
        assert_eq!(o.output_tokens, a.output_tokens, "output_tokens diverged");
    }
}

/// Reading + re-projecting a real fixture must preserve every JSONL line.
///
/// This is the bluntest possible UX-loss check: count source lines, count
/// projected lines, expect them equal. Catches dropped attachments,
/// metadata headers (ai-title, last-prompt, queue-operation), and the
/// permission-mode preamble — all of which were silently lost before.
#[test]
fn read_then_project_preserves_line_count() {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read claude fixture");
    let view = toolpath_claude::provider::to_view(&convo);
    let projected = ClaudeProjector
        .project(&view)
        .expect("project back to claude");

    let source_lines = std::fs::read_to_string(fixture_path())
        .expect("read fixture")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let projected_lines = projected.preamble.len() + projected.entries.len();

    assert_eq!(
        source_lines, projected_lines,
        "line count diverged on read→project (source={source_lines}, projected={projected_lines}). \
         Indicates silent drops at the reader, IR-conversion, or projection step."
    );
}

/// Header / attachment events survive read → IR → project, byte-for-byte.
///
/// `ai-title`, `last-prompt`, `queue-operation`, `attachment` carry user-
/// visible context. They must come back identical after a self-roundtrip.
#[test]
fn read_then_project_preserves_metadata_entries() {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read claude fixture");
    let view = toolpath_claude::provider::to_view(&convo);
    let projected = ClaudeProjector
        .project(&view)
        .expect("project back to claude");

    let source_attachments: usize = convo
        .entries
        .iter()
        .filter(|e| e.entry_type == "attachment")
        .count();
    let projected_attachments: usize = projected
        .entries
        .iter()
        .filter(|e| e.entry_type == "attachment")
        .count();
    assert_eq!(
        source_attachments, projected_attachments,
        "attachment entries dropped on roundtrip ({source_attachments} → {projected_attachments})"
    );

    assert_eq!(
        convo.preamble.len(),
        projected.preamble.len(),
        "preamble line count diverged ({} → {})",
        convo.preamble.len(),
        projected.preamble.len()
    );
}

/// `toolUseResult` (Bash stdout/stderr, file edit blob, search results)
/// drives Claude Code's UI rendering of tool calls. Without it the user
/// loads a roundtripped session and sees no diffs and no shell output.
#[test]
fn read_then_project_preserves_tool_use_result_count() {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read claude fixture");
    let view = toolpath_claude::provider::to_view(&convo);
    let projected = ClaudeProjector
        .project(&view)
        .expect("project back to claude");

    let source_with_tur = convo
        .entries
        .iter()
        .filter(|e| e.tool_use_result.is_some())
        .count();
    let projected_with_tur = projected
        .entries
        .iter()
        .filter(|e| e.tool_use_result.is_some())
        .count();

    assert_eq!(
        source_with_tur, projected_with_tur,
        "toolUseResult count diverged: source had {source_with_tur}, projection has \
         {projected_with_tur}. The Claude UI uses this field for diff/shell-output \
         rendering, so a drop here means visible regression on resume."
    );
    assert!(
        source_with_tur > 0,
        "fixture should contain at least one tool result — refresh capture"
    );
}

/// End-to-end: Claude JSONL → toolpath_claude::derive::derive_path → cached JSON →
/// toolpath_convo::extract_conversation → ClaudeProjector → JSONL.
///
/// This is the actual `path import` / `path export` flow. Tests headerless
/// preamble lines (ai-title, last-prompt, file-history-snapshot,
/// permission-mode), attachments, and assistant entries with bare-thinking
/// content all survive the cache round-trip — the dimensions that were
/// dropping ~10–25% of source lines on real sessions.
#[test]
fn cache_roundtrip_preserves_line_counts_per_type() {
    use toolpath::v1::Graph;
    use toolpath_convo::extract_conversation;

    let convo = ConversationReader::read_conversation(fixture_path()).expect("read claude fixture");
    let path = toolpath_claude::derive::derive_path(
        &convo,
        &toolpath_claude::derive::DeriveConfig {
            include_thinking: false,
            ..Default::default()
        },
    )
    .expect("derive");
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize cached graph");
    let back = Graph::from_json(&json).expect("re-parse cached graph");
    let path = back.into_single_path().expect("single path");
    let view = extract_conversation(&path);
    let projected = ClaudeProjector
        .project(&view)
        .expect("project back to claude");

    let source = std::fs::read_to_string(fixture_path()).expect("read fixture");
    let source_types: BTreeSet<String> = source
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // For every type that appears in the source, the export should also
    // produce at least one entry of that type. Catches whole-category drops
    // (the ai-title / last-prompt / file-history-snapshot regression).
    let projected_types: BTreeSet<String> = projected
        .preamble
        .iter()
        .filter_map(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .chain(projected.entries.iter().map(|e| e.entry_type.clone()))
        .collect();

    let missing: Vec<&String> = source_types.difference(&projected_types).collect();
    assert!(
        missing.is_empty(),
        "Whole entry-type categories dropped on cache round-trip: {missing:?}. \
         The Claude UI relies on these for resume/title/file-snapshot rendering."
    );
}

#[test]
fn projector_output_is_re_parseable_by_reader() {
    let view = load_fixture_view();
    let after = ir_roundtrip(&view);
    let projector = ClaudeProjector;
    let convo = projector
        .project(&after)
        .expect("project to claude conversation");

    let mut lines: Vec<String> = Vec::new();
    for raw in &convo.preamble {
        lines.push(serde_json::to_string(raw).expect("serialize preamble line"));
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
