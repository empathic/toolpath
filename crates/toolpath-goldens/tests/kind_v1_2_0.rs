//! Kind readiness: assume ONE combined agent-coding-session v1.2.0 lands with all three additions
//! (#108 the `conversation.compact` step, #353 additive token classes, #132 an optional `model`
//! on conversation.append), and test it before it does.
//!
//! `fixtures/kind-v1.2.0-combined.schema.json` is a TEST FIXTURE, composed by a three-way merge of
//! the three PRs' own `v1.2.0/schema.json` against v1.1.0 (credit: empathic/toolpath #108 Ben
//! Barber, #353 ulmentflam, #132). Only the `description` needed a hand-merge (each PR edits it).
//! It is not our kind: the owners publish it. When any of the three merges, swap the fixture for
//! the real schema and diff.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use toolpath_goldens::model::Validation;
use toolpath_goldens::validate::validate;

const V11: &str = "https://toolpath.net/kinds/agent-coding-session/v1.1.0";
const V12: &str = "https://toolpath.net/kinds/agent-coding-session/v1.2.0";
const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots");

fn combined() -> jsonschema::Validator {
    let text = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/kind-v1.2.0-combined.schema.json"
    ))
    .expect("the combined schema fixture");
    let schema: Value = serde_json::from_str(&text).expect("it is JSON");
    jsonschema::validator_for(&schema).expect("it compiles")
}

fn body(snap: &str) -> &str {
    let rest = snap.strip_prefix("---\n").expect("insta header");
    let end = rest.find("\n---\n").expect("insta header closes");
    &rest[end + "\n---\n".len()..]
}

/// A path claiming v1.2.0 with the given steps' `structural` objects.
fn path_with(structurals: Vec<Value>) -> Value {
    let steps: Vec<Value> = structurals
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "step": {"id": format!("s{i}"), "actor": "agent:x", "timestamp": "2026-01-01T00:00:00Z"},
                "change": {"claude-code://t": {"structural": s}}
            })
        })
        .collect();
    json!({"path": {"id": "p", "head": "s0"}, "steps": steps, "meta": {"kind": V12}})
}

fn append(extra: Value) -> Value {
    let mut v = json!({"type": "conversation.append", "role": "assistant", "text": "hi"});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn errors(path: &Value) -> Vec<String> {
    combined()
        .iter_errors(path)
        .map(|e| e.to_string())
        .collect()
}

/// Every baseline document validates against v1.1.0 (what `path p validate` does today) AND, with
/// its kind URI moved to v1.2.0, against the combined v1.2.0: the new kind is additive.
#[test]
fn every_baseline_document_validates_under_v1_1_0_and_the_combined_v1_2_0() {
    let v12 = combined();
    let mut checked = 0;
    let mut failures = Vec::new();
    for entry in fs::read_dir(SNAPSHOTS).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !(name.starts_with("document@") && name.ends_with(".snap")) {
            continue;
        }
        let text = fs::read_to_string(Path::new(SNAPSHOTS).join(&name)).unwrap();
        let doc: Value = serde_json::from_str(body(&text)).unwrap();
        if !matches!(validate(&doc), Validation::Valid) {
            failures.push(format!("{name}: not valid under v1.1.0"));
        }
        for (i, path) in doc["paths"].as_array().into_iter().flatten().enumerate() {
            let mut moved = path.clone();
            if moved.pointer("/meta/kind").and_then(Value::as_str) != Some(V11) {
                continue;
            }
            moved["meta"]["kind"] = json!(V12);
            for e in v12.iter_errors(&moved) {
                failures.push(format!("{name} path {i}: {e}"));
            }
        }
        checked += 1;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(checked >= 13, "documents checked: {checked}");
}

#[test]
fn a_compact_step_validates_and_a_malformed_one_does_not() {
    let ok = path_with(vec![
        append(json!({})),
        json!({"type": "conversation.compact", "trigger": "auto", "pre_tokens": 1234.0,
               "summary": "s", "kept": ["a", "b"]}),
    ]);
    assert_eq!(errors(&ok), Vec::<String>::new());
    let bad = path_with(vec![
        json!({"type": "conversation.compact", "trigger": "sometimes"}),
    ]);
    assert!(!errors(&bad).is_empty(), "trigger must be auto or manual");
}

#[test]
fn token_classes_are_non_negative_and_breakdowns_are_named_classes() {
    let usage = |u: Value| path_with(vec![append(json!({"token_usage": u}))]);
    let ok = usage(
        json!({"input_tokens": 10, "output_tokens": 5, "cache_read_tokens": 3,
                          "cache_write_tokens": 0,
                          "breakdowns": {"output": {"reasoning": 2}}}),
    );
    assert_eq!(errors(&ok), Vec::<String>::new());
    let negative = usage(json!({"input_tokens": 10, "output_tokens": 5, "cache_read_tokens": -1}));
    assert!(!errors(&negative).is_empty(), "a negative class is refused");
    let odd = usage(json!({"input_tokens": 1, "output_tokens": 1,
                           "breakdowns": {"mystery": {"x": 1}}}));
    assert!(!errors(&odd).is_empty(), "a breakdown keys a known class");
}

#[test]
fn model_is_optional_on_append() {
    assert_eq!(
        errors(&path_with(vec![append(
            json!({"model": "claude-haiku-4.5"})
        )])),
        Vec::<String>::new()
    );
    assert_eq!(
        errors(&path_with(vec![append(json!({}))])),
        Vec::<String>::new()
    );
    assert!(
        !errors(&path_with(vec![append(json!({"model": 7}))])).is_empty(),
        "model is a string"
    );
}
