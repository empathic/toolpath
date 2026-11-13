//! Growing-rollout prefix stability: deriving the first `n` lines of a
//! rollout yields step ids and parents that the derive of the whole
//! rollout starts with, for every `n`. Step bodies are not compared: a
//! tool call's output and a round's token totals fill in on steps already
//! derived.

use std::path::{Path, PathBuf};

use toolpath_codex::{RolloutReader, to_view};
use toolpath_convo::{DeriveConfig, derive_path};

type Linkage = Vec<(String, Vec<String>)>;

fn workspace_fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(rel)
}

fn crate_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// `(id, parents)` of every step derived from `lines`.
fn linkage(lines: &[&str], file: &Path) -> Linkage {
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(file, body).expect("write prefix");
    let session = RolloutReader::read_session(file).expect("read prefix");
    derive_path(&to_view(&session), &DeriveConfig::default())
        .steps
        .into_iter()
        .map(|s| (s.step.id, s.step.parents))
        .collect()
}

fn assert_prefix_stable(lines: &[&str]) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("rollout.jsonl");
    let full = linkage(lines, &file);
    for n in 1..lines.len() {
        let prefix = linkage(&lines[..n], &file);
        assert!(
            full.starts_with(&prefix),
            "derivation of the first {n} lines is not a prefix of the full derivation:\n\
             prefix: {prefix:?}\nfull:   {full:?}"
        );
    }
}

fn assert_fixture_prefix_stable(path: PathBuf) {
    let source = std::fs::read_to_string(&path).expect("read fixture");
    let lines: Vec<&str> = source.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_prefix_stable(&lines);
}

#[test]
fn captured_rollout_derives_prefix_stably() {
    assert_fixture_prefix_stable(workspace_fixture("test-fixtures/codex/convo.jsonl"));
}

#[test]
fn sample_rollout_derives_prefix_stably() {
    assert_fixture_prefix_stable(crate_fixture("sample-codex-python.jsonl"));
}

#[test]
fn compacted_rollout_derives_prefix_stably() {
    assert_fixture_prefix_stable(crate_fixture("compacted_session.jsonl"));
}

/// An assistant message with no content is dropped when it is read; a
/// function call that follows opens a turn of its own after the events
/// already derived, instead of reviving the dropped turn in front of them.
#[test]
fn empty_assistant_message_stays_dropped_when_a_call_follows() {
    let lines = [
        r#"{"timestamp":"2026-06-01T10:00:00.000Z","type":"session_meta","payload":{"id":"019dabc6-8fef-7681-a054-b5bb75fcb97d","timestamp":"2026-06-01T10:00:00.000Z","cwd":"/work","originator":"codex_exec","cli_version":"0.125.0","source":"exec","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-06-01T10:00:01.000Z","type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/work","approval_policy":"never","sandbox_policy":{"type":"read-only"},"model":"gpt-5"}}"#,
        r#"{"timestamp":"2026-06-01T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        r#"{"timestamp":"2026-06-01T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}"#,
        r#"{"timestamp":"2026-06-01T10:00:04.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
        r#"{"timestamp":"2026-06-01T10:00:05.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"call-1"}}"#,
    ];
    assert_prefix_stable(&lines);

    let dir = tempfile::tempdir().expect("tempdir");
    let ids: Vec<String> = linkage(&lines, &dir.path().join("rollout.jsonl"))
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids.len(), 5, "one step per kept line: {ids:?}");
    assert!(ids[3].starts_with("token_count"), "{ids:?}");
    assert!(ids[4].starts_with("synth-"), "{ids:?}");
}
