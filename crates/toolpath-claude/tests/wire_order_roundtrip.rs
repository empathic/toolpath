//! Wire-level entry-stream fidelity against the real captured session
//! (`test-fixtures/claude/convo.jsonl`).
//!
//! Real Claude interleaves attachment entries with the turns. The projector
//! used to emit all events from a trailing pass, which regrouped them at
//! the end of the file — a resumed session then replayed its entries out
//! of order.
//!
//! What is pinned: the per-line `type` sequence of the direct
//! `to_view` → `project` pipeline matches the source line for line —
//! headerless lines (`last-prompt`, `ai-title`, `queue-operation`, …)
//! included, at their file position, and attachments in place, not
//! regrouped at the end.
//!
//! One position the IR cannot express: a tool-result carrier is absorbed
//! into the assistant turn it answers, so a headerless run written between
//! that turn and the carrier is indistinguishable from one written right
//! after the carrier. The projector puts the run before the carrier (the
//! shape Claude Code writes far more often); the captured fixture has two
//! runs of the other shape, so the fixture comparison is exact over every
//! line except the carriers, and the hand-written fixture is exact over
//! every line.
//!
//! What is NOT pinned: `parentUuid` values. 19 of the fixture's 71 entries
//! legitimately diverge — the projector re-synthesizes tool-result carrier
//! entries under derived uuids (`<turn-uuid>-result-<tool-id>`), and each
//! diverged entry points at a re-synthesized carrier uuid. Also not pinned: the
//! derive → extract → project pipeline — only the direct projection is
//! exercised here.

use std::path::{Path, PathBuf};

use toolpath_claude::{ClaudeProjector, ConversationReader, Line};
use toolpath_convo::ConversationProjector;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("claude")
        .join("convo.jsonl")
}

fn project(entries_of: &toolpath_claude::Conversation) -> toolpath_claude::Conversation {
    let view = toolpath_claude::provider::to_view(entries_of);
    ClaudeProjector.project(&view).expect("project view")
}

/// Every line's `type`, headerless lines prefixed `H:` so a headerless
/// line and an entry of the same type cannot pass for each other.
fn type_sequence(c: &toolpath_claude::Conversation) -> Vec<String> {
    c.lines()
        .map(|line| match line {
            Line::Headerless(raw) => format!("H:{}", raw["type"].as_str().unwrap_or("?")),
            Line::Entry(e) => e.entry_type.clone(),
        })
        .collect()
}

fn is_tool_result_carrier(e: &toolpath_claude::ConversationEntry) -> bool {
    e.message
        .as_ref()
        .is_some_and(|m| m.text().is_empty() && !m.tool_results().is_empty())
}

/// `type_sequence` without the tool-result carriers.
fn type_sequence_sans_carriers(c: &toolpath_claude::Conversation) -> Vec<String> {
    c.lines()
        .filter(|line| !matches!(line, Line::Entry(e) if is_tool_result_carrier(e)))
        .map(|line| match line {
            Line::Headerless(raw) => format!("H:{}", raw["type"].as_str().unwrap_or("?")),
            Line::Entry(e) => e.entry_type.clone(),
        })
        .collect()
}

#[test]
fn projected_entry_type_sequence_matches_source() {
    let convo = ConversationReader::read_conversation(fixture_path()).expect("read fixture");
    assert!(
        convo.headerless.iter().any(|h| h.before > 0),
        "fixture must interleave headerless lines with entries"
    );
    let projected = project(&convo);
    assert_eq!(
        type_sequence_sans_carriers(&convo),
        type_sequence_sans_carriers(&projected),
        "line stream must keep the source interleaving (headerless lines, \
         attachments and system entries in place, not regrouped)"
    );
    let entry_types = |c: &toolpath_claude::Conversation| -> Vec<String> {
        c.entries.iter().map(|e| e.entry_type.clone()).collect()
    };
    assert_eq!(entry_types(&convo), entry_types(&projected));
    assert_eq!(convo.lines().count(), projected.lines().count());
}

#[test]
fn headerless_lines_keep_their_position_through_derive_and_extract() {
    use toolpath_convo::{DeriveConfig, derive_path, extract_conversation};

    let jsonl = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/headerless_interleaved.jsonl"),
    )
    .expect("read fixture");
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir
        .path()
        .join("headerless-1111-2222-3333-444444444444.jsonl");
    std::fs::write(&file_path, &jsonl).expect("write fixture");

    let convo = ConversationReader::read_conversation(&file_path).expect("read fixture");
    let view = toolpath_claude::provider::to_view(&convo);
    let path = derive_path(&view, &DeriveConfig::default());

    let dead: Vec<&str> = toolpath::v1::query::dead_ends(&path.steps, &path.path.head)
        .iter()
        .map(|s| s.step.id.as_str())
        .collect();
    assert!(dead.is_empty(), "unexpected dead ends: {dead:?}");

    let projected = ClaudeProjector
        .project(&extract_conversation(&path))
        .expect("project view");
    assert_eq!(type_sequence(&convo), type_sequence(&projected));

    let parents: Vec<(String, Option<String>)> = projected
        .entries
        .iter()
        .map(|e| (e.uuid.clone(), e.parent_uuid.clone()))
        .collect();
    let source_parents: Vec<(String, Option<String>)> = convo
        .entries
        .iter()
        .map(|e| (e.uuid.clone(), e.parent_uuid.clone()))
        .collect();
    assert_eq!(
        parents, source_parents,
        "wire parents must resolve back past the headerless events"
    );
}

/// Appending lines to a session file only appends steps to the derived
/// path: for every prefix of the fixture, the derived `(id, parents)`
/// sequence is a prefix of the full derivation's. Headerless lines used to
/// break this — they all derived at the front, so each turn's
/// `last-prompt`/`ai-title`/`mode` group inserted steps before the first
/// turn.
#[test]
fn derived_step_sequence_is_append_only_under_file_growth() {
    use toolpath_convo::{DeriveConfig, derive_path};

    let source = std::fs::read_to_string(fixture_path()).expect("read fixture");
    let lines: Vec<&str> = source.lines().filter(|l| !l.trim().is_empty()).collect();
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("append-1111-2222-3333-444444444444.jsonl");

    let linkage = |n: usize| -> Vec<(String, Vec<String>)> {
        let mut body = lines[..n].join("\n");
        body.push('\n');
        std::fs::write(&file_path, body).expect("write prefix");
        let convo = ConversationReader::read_conversation(&file_path).expect("read prefix");
        let view = toolpath_claude::provider::to_view(&convo);
        derive_path(&view, &DeriveConfig::default())
            .steps
            .into_iter()
            .map(|s| (s.step.id, s.step.parents))
            .collect()
    };

    let full = linkage(lines.len());
    for n in 1..lines.len() {
        let prefix = linkage(n);
        assert!(
            full.starts_with(&prefix),
            "derivation of the first {n} lines is not a prefix of the full derivation:\n\
             prefix tail: {:?}\nfull at that index: {:?}",
            prefix.last(),
            full.get(prefix.len() - 1)
        );
    }
}
