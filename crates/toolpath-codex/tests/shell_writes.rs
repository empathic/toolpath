//! Shell-written files in a derived Codex path (fixture: tests/fixtures/shell-writes.jsonl).

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Value, json};
use toolpath::v1::{ArtifactChange, Path};
use toolpath_codex::{RolloutReader, derive, provider::to_view};

const SESSION: &str = "019e0000-0000-7000-8000-000000000001";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn derived(name: &str) -> Path {
    let session = RolloutReader::read_session(fixture(name)).unwrap();
    derive::derive_path(&session, &derive::DeriveConfig::default())
}

fn change_by<'p>(path: &'p Path, key: &str, tool_id: &str) -> &'p ArtifactChange {
    path.steps
        .iter()
        .filter_map(|s| s.change.get(key))
        .find(|c| c.structural.as_ref().unwrap().extra["tool_id"] == tool_id)
        .unwrap_or_else(|| panic!("no {key} change from {tool_id}"))
}

fn extra(c: &ArtifactChange) -> &HashMap<String, Value> {
    &c.structural.as_ref().unwrap().extra
}

#[test]
fn heredoc_write_then_tracked_append() {
    let p = derived("shell-writes.jsonl");
    let w = extra(change_by(&p, "/tmp/sw/wc.py", "c1"));
    assert_eq!(w["after"], "print(1)\n");
    assert_eq!(w["codex"]["source"], "shell-heredoc");
    assert_eq!(
        w["codex"]["executions"][0],
        json!({
            "tool_id": "c1", "tool": "exec_command", "redirect": "write", "via": "cat",
            "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
            "sole_command": true, "implied_by_success": true,
            "tag": "EOF", "tag_quoted": true, "body": "print(1)\n"
        })
    );

    let a = change_by(&p, "/tmp/sw/wc.py", "c2");
    let ax = extra(a);
    assert_eq!(ax["operation"], "append");
    assert_eq!(
        (ax["before"].as_str(), ax["after"].as_str()),
        (Some("print(1)\n"), Some("print(1)\nprint(2)\n"))
    );
    assert_eq!(ax["codex"]["executions"][0]["append_base"], "tracked");
    assert!(a.raw.as_deref().unwrap().contains("+print(2)"));
}

#[test]
fn a_failed_write_is_recorded_as_a_failure() {
    let p = derived("shell-writes.jsonl");
    let f = extra(change_by(&p, "/tmp/sw/sub/fail.txt", "c3"));
    assert_eq!(f["after"], "nope\n");
    assert_eq!(f["codex"]["outcome"], "failure");
    let e = &f["codex"]["executions"][0];
    assert_eq!(e["exit_code"], 1);
    assert_eq!(e["sole_command"], false);
    assert_eq!(e["implied_by_success"], true);
}

#[test]
fn an_intercepted_shell_patch_is_recorded_once_from_patch_apply_end() {
    let p = derived("shell-writes.jsonl");
    let hits = p
        .steps
        .iter()
        .filter(|s| s.change.contains_key("/tmp/sw/notes.md"))
        .count();
    assert_eq!(hits, 1);
    let n = extra(change_by(&p, "/tmp/sw/notes.md", "c4"));
    assert_eq!(n["operation"], "add");
    assert_eq!(n["codex"]["source"], "shell-apply-patch");
    assert_eq!(n["codex"]["outcome"], "success");
    let e = &n["codex"]["executions"];
    assert_eq!(e.as_array().unwrap().len(), 1);
    assert_eq!(
        (
            e[0]["outcome_basis"].as_str(),
            e[0]["patch_apply_end"].as_bool()
        ),
        (Some("patch_apply_end"), Some(true))
    );
}

#[test]
fn an_argv_shell_patch_without_patch_apply_end_is_inferred() {
    let p = derived("shell-writes.jsonl");
    let u = change_by(&p, "/tmp/sw/wc.py", "c5");
    assert!(u.raw.is_none());
    let ux = extra(u);
    assert_eq!(ux["operation"], "update");
    assert_eq!(ux["codex"]["source"], "shell-apply-patch");
    let e = &ux["codex"]["executions"][0];
    assert_eq!(
        (e["via"].as_str(), e["exit_code"].as_i64()),
        (Some("apply_patch"), Some(0))
    );
    assert_eq!(e["sole_command"], true);
    assert!(e.get("tag").is_none());
}

#[test]
fn an_unanswered_write_is_recorded_as_unknown() {
    let p = derived("shell-writes.jsonl");
    let l = extra(change_by(&p, "/tmp/sw/late.txt", "c6"));
    assert_eq!(l["codex"]["outcome"], "unknown");
    assert_eq!(l["codex"]["executions"][0]["outcome_basis"], "no_result");
}

#[test]
fn unresolvable_targets_are_attempts_on_the_conversation_change() {
    let p = derived("shell-writes.jsonl");
    let key = format!("codex://{SESSION}");
    let attempts: Vec<&Value> = p
        .steps
        .iter()
        .filter_map(|s| s.change.get(&key))
        .filter_map(|c| extra(c).get("codex"))
        .flat_map(|c| c["unresolved_shell_writes"].as_array().unwrap())
        .collect();
    assert_eq!(attempts.len(), 3, "{attempts:?}");
    assert_eq!(
        *attempts[0],
        json!({
            "tool_id": "c7", "tool": "exec_command", "path_as_written": "$OUT",
            "reason": "not_literal", "via": "cat", "redirect": "write", "body": "x\n",
            "outcome": "success", "outcome_basis": "exit_code", "exit_code": 0,
            "sole_command": true, "implied_by_success": true
        })
    );
    let a = attempts[1];
    assert_eq!(
        (a["tool_id"].as_str(), a["path_as_written"].as_str()),
        (Some("c8"), Some("rel.txt"))
    );
    assert_eq!(
        (
            a["reason"].as_str(),
            a["outcome"].as_str(),
            a["exit_code"].as_i64()
        ),
        (Some("unknown_dir"), Some("failure"), Some(1))
    );
    let a = attempts[2];
    assert_eq!(
        (a["tool_id"].as_str(), a["path_as_written"].as_str()),
        (Some("c9"), Some("p.txt"))
    );
    assert_eq!(
        (
            a["reason"].as_str(),
            a["via"].as_str(),
            a["operation"].as_str()
        ),
        (Some("unknown_dir"), Some("apply_patch"), Some("add"))
    );
    // Never a guessed path.
    for s in &p.steps {
        for k in s.change.keys() {
            assert!(
                !k.contains("OUT") && !k.ends_with("rel.txt") && !k.ends_with("p.txt"),
                "{k}"
            );
        }
    }
}

#[test]
fn files_changed_follows_call_order() {
    let session = RolloutReader::read_session(fixture("shell-writes.jsonl")).unwrap();
    assert_eq!(
        to_view(&session).files_changed,
        vec![
            "/tmp/sw/wc.py",
            "/tmp/sw/sub/fail.txt",
            "/tmp/sw/notes.md",
            "/tmp/sw/late.txt"
        ]
    );
}

#[test]
fn existing_fixtures_carry_no_codex_stamp() {
    let convo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/codex/convo.jsonl");
    let paths = [
        derived("sample-codex-python.jsonl"),
        derived("compacted_session.jsonl"),
        derive::derive_path(
            &RolloutReader::read_session(convo).unwrap(),
            &derive::DeriveConfig::default(),
        ),
    ];
    for p in &paths {
        for s in &p.steps {
            for c in s.change.values() {
                if let Some(st) = &c.structural {
                    assert!(!st.extra.contains_key("codex"), "{}", s.step.id);
                }
            }
        }
    }
}
