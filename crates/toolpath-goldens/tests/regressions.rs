//! The regression corpus: every fidelity bug CHANGELOG.md (and the resume
//! matrix) records, re-introduced as a fault in one stage's output of a real
//! set, and the check that must name it: the field and the stage. Production
//! code is not touched; each case mutates what the pipeline produced and runs
//! the goldens' own checks over the result.
//!
//! A case here that stops failing means the goldens stopped catching a bug
//! that already shipped once. Bugs the goldens cannot reach are listed in
//! `docs/GOLDENS.md` ("What the regression corpus does not reach") with the
//! test that does.

use std::path::Path;

use serde_json::Value;
use toolpath_goldens::accounting;
use toolpath_goldens::contracts::{Violation, check_document, check_projection};
use toolpath_goldens::model::{Agent, Set, Stages, Validation};
use toolpath_goldens::pipeline::run;
use toolpath_goldens::validate::validate;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-fixtures");
const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../goldens");
const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots");

/// One set through the pipeline, every store under a temp HOME.
fn stages(input: &str) -> Stages {
    let home = tempfile::tempdir().expect("temp HOME");
    let set = Set::from_input_file(Path::new(input)).expect("readable input");
    run(&set, home.path()).unwrap_or_else(|e| panic!("{}: {e}", set.name))
}

fn fixture(agent: &str) -> String {
    let dir = Path::new(FIXTURES).join(agent);
    let f = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("convo"))
        .expect("convo.*");
    f.display().to_string()
}

fn golden(name: &str) -> String {
    let dir = Path::new(GOLDENS).join(name);
    let f = std::fs::read_dir(&dir)
        .expect("golden dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("input"))
        .expect("input.*");
    f.display().to_string()
}

/// The committed snapshot's body, as insta compares it.
fn snapshot_body(name: &str) -> String {
    let text = std::fs::read_to_string(Path::new(SNAPSHOTS).join(format!("{name}.snap")))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut parts = text.splitn(3, "---\n");
    parts.next();
    parts.next();
    parts.next().unwrap_or_default().to_owned()
}

/// The first line on which `mutated` differs from snapshot `name`: what the
/// goldens test would report for the stage it pins.
fn snapshot_diff(name: &str, mutated: &str) -> Option<usize> {
    let pinned = snapshot_body(name);
    let a: Vec<&str> = pinned.trim_end().lines().collect();
    let b: Vec<&str> = mutated.trim_end().lines().collect();
    (0..a.len().max(b.len()))
        .find(|&i| a.get(i) != b.get(i))
        .map(|i| i + 1)
}

fn rules(v: &[Violation]) -> Vec<&'static str> {
    v.iter().map(|v| v.rule).collect()
}

fn edit_lines(text: &str, mut f: impl FnMut(usize, &mut Value) -> bool) -> String {
    let mut done = false;
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        let mut v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                out += line;
                out.push('\n');
                continue;
            }
        };
        if !done && f(i, &mut v) {
            done = true;
        }
        out += &serde_json::to_string(&v).unwrap();
        out.push('\n');
    }
    assert!(done, "the fault found nothing to change");
    out
}

/// 2026-07-30, toolpath-claude 0.12.2: a thinking block written with
/// `"signature": null`; the API answers 400 on the first message after
/// resume. Stage: project (→ claude).
#[test]
fn claude_thinking_signature_null() {
    let s = stages(&fixture("copilot"));
    let projected = &s.projections[&Agent::Claude];
    let broken = edit_lines(projected, |_, v| {
        let Some(blocks) = v
            .pointer_mut("/message/content")
            .and_then(Value::as_array_mut)
        else {
            return false;
        };
        for b in blocks {
            if b["type"] == "thinking" {
                b["signature"] = Value::Null;
                return true;
            }
        }
        false
    });
    let v = check_projection(Agent::Claude, &broken);
    assert_eq!(rules(&v), ["claude-null-signature"], "{v:?}");
    assert_eq!(v[0].field, "message.content[].signature");
    assert!(snapshot_diff("projection@copilot@claude", &broken).is_some());
}

/// 2026-07-30, path-cli 0.16.1: a JSONL export without a final newline;
/// Claude Code's first append lands on the projected last line. Stage:
/// project. insta trims trailing whitespace, so only the contract sees it.
#[test]
fn jsonl_export_without_trailing_newline() {
    let s = stages(&fixture("codex"));
    let projected = &s.projections[&Agent::Claude];
    let broken = projected.trim_end_matches('\n').to_owned();
    assert_eq!(
        snapshot_diff("projection@codex@claude", &broken),
        None,
        "the snapshot cannot see a missing final newline"
    );
    let v = check_projection(Agent::Claude, &broken);
    assert_eq!(rules(&v), ["trailing-newline"], "{v:?}");
}

/// 2026-06-17 (kind v1.1.0): Claude Code repeats the message's usage on every
/// content-block line; one step per line carrying the full usage over-counts
/// output tokens by the block count. Stage: derive. The accounting contract
/// names the field.
#[test]
fn claude_usage_repeated_on_every_block() {
    let s = stages(&golden("claude-session"));
    let mut doc = s.document.clone();
    let mut usage_by_group = std::collections::BTreeMap::new();
    for step in doc["paths"][0]["steps"].as_array().unwrap() {
        for c in step["change"].as_object().unwrap().values() {
            let st = &c["structural"];
            if let (Some(g), Some(u)) = (st["group_id"].as_str(), st.get("token_usage"))
                && !u.is_null()
            {
                usage_by_group.insert(g.to_owned(), u.clone());
            }
        }
    }
    let mut repeated = 0;
    for step in doc["paths"][0]["steps"].as_array_mut().unwrap() {
        for c in step["change"].as_object_mut().unwrap().values_mut() {
            let st = &mut c["structural"];
            if let Some(u) = st["group_id"]
                .as_str()
                .and_then(|g| usage_by_group.get(g))
                .cloned()
                && st.get("token_usage").is_none_or(Value::is_null)
            {
                st["token_usage"] = u;
                repeated += 1;
            }
        }
    }
    assert!(repeated > 0, "the set has no multi-block message");
    let gaps = accounting::check(&doc, &s.ir).expect("single path");
    let fields: Vec<&str> = gaps.iter().map(|g| g.field.as_str()).collect();
    assert!(fields.contains(&"output_tokens"), "{gaps:?}");
}

/// 2026-06-17: toolpath-codex stamped the cumulative session counter
/// (`total_token_usage`) on each assistant turn; per-step sums grew
/// quadratically. Stage: derive.
#[test]
fn codex_cumulative_usage_per_turn() {
    // The fixture's session carries its usage once; the bug stamped the
    // session's running total on every assistant turn. Stamp it so.
    let s = stages(&fixture("codex"));
    let mut doc = s.document.clone();
    let total = doc["paths"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|st| st["change"].as_object().unwrap().values())
        .find_map(|c| {
            c["structural"]
                .get("token_usage")
                .filter(|u| !u.is_null())
                .cloned()
        })
        .expect("a usage step");
    let mut stamped = 0;
    for step in doc["paths"][0]["steps"].as_array_mut().unwrap() {
        for c in step["change"].as_object_mut().unwrap().values_mut() {
            let st = &mut c["structural"];
            if st["role"] == "assistant" && st.get("token_usage").is_none_or(Value::is_null) {
                st["token_usage"] = total.clone();
                stamped += 1;
            }
        }
    }
    assert!(stamped > 0, "no assistant turn to stamp");
    let gaps = accounting::check(&doc, &s.ir).expect("single path");
    assert!(gaps.iter().any(|g| g.field == "output_tokens"), "{gaps:?}");
}

/// 2026-07-01, toolpath-convo 0.11.1: Claude Code reuses `uuid` on
/// attachment lines; a duplicate step id broke every consumer keyed on it.
/// Stage: derive. RFC "ID Uniqueness".
#[test]
fn duplicate_step_ids() {
    let s = stages(&golden("claude-session"));
    let mut doc = s.document.clone();
    let steps = doc["paths"][0]["steps"].as_array_mut().unwrap();
    let first = steps[0]["step"]["id"].clone();
    steps[1]["step"]["id"] = first;
    let v = check_document(&doc);
    assert_eq!(rules(&v), ["unique-step-ids"], "{v:?}");
    assert!(
        snapshot_diff(
            "document@claude-session",
            &serde_json::to_string_pretty(&doc).unwrap()
        )
        .is_some()
    );
}

/// "Actor validation fixes — derived paths conform to the base schema": an
/// actor string outside `human|agent|tool|ci:<name>`. Stage: validate.
#[test]
fn actor_outside_the_base_schema() {
    let s = stages(&golden("claude-session"));
    let mut doc = s.document.clone();
    doc["paths"][0]["steps"][1]["step"]["actor"] = "claude code".into();
    match validate(&doc) {
        Validation::Invalid { errors } => {
            assert!(errors.iter().any(|e| e.contains("actor")), "{errors:?}")
        }
        other => panic!("validate accepted it: {other:?}"),
    }
}

/// Resume matrix 2026-10-15, fixed in #361: pi's parent chain cut after the
/// first tool call; Claude Code and pi resumed with only the newest message.
/// Stage: project (→ pi).
#[test]
fn pi_parent_chain_cut() {
    let s = stages(&fixture("codex"));
    let projected = &s.projections[&Agent::Pi];
    let broken = edit_lines(projected, |i, v| {
        if i > 3 && v["type"] == "message" && v["parentId"].is_string() {
            v["parentId"] = Value::Null;
            return true;
        }
        false
    });
    let v = check_projection(Agent::Pi, &broken);
    assert_eq!(rules(&v), ["parent-chain"], "{v:?}");
    assert_eq!(v[0].field, "parentId");
}

/// Resume matrix 2026-10-15, fixed in #361: copilot → claude wrote a failed
/// tool call as an errored tool_result with empty content; the API answers
/// 400. Stage: project (→ claude).
#[test]
fn copilot_empty_errored_tool_result() {
    let s = stages(&fixture("copilot"));
    let projected = &s.projections[&Agent::Claude];
    let broken = edit_lines(projected, |_, v| {
        let Some(blocks) = v
            .pointer_mut("/message/content")
            .and_then(Value::as_array_mut)
        else {
            return false;
        };
        for b in blocks {
            if b["type"] == "tool_result" {
                b["is_error"] = true.into();
                b["content"] = "".into();
                return true;
            }
        }
        false
    });
    let v = check_projection(Agent::Claude, &broken);
    assert_eq!(rules(&v), ["claude-empty-error-result"], "{v:?}");
}

/// 2026-10-15, fixed in #365: projected opencode session ids in toolpath's
/// shape (`ses_` + 24 hex); Zen's free tier answered 403 FreeTierError.
/// Stage: project (→ opencode).
#[test]
fn opencode_session_id_shape() {
    let s = stages(&fixture("codex"));
    let projected = &s.projections[&Agent::Opencode];
    let mut v: Value = serde_json::from_str(projected).unwrap();
    v["id"] = "ses_0123456789abcdef01234567".into();
    let broken = serde_json::to_string_pretty(&v).unwrap();
    let found = check_projection(Agent::Opencode, &broken);
    assert_eq!(rules(&found), ["opencode-session-id"], "{found:?}");
}
