//! The core types, as RFC.md and "Toolpath at a glance" define them: graph,
//! path, step, base, head, dead end, fork, merge, actors, change perspectives,
//! meta.intent / meta.refs, `$ref` paths and ID uniqueness.
//!
//! Two pins:
//!
//! * `core_types@dag`: the DAG features of the exploration example computed
//!   with the crate's own queries. Dead ends must equal all steps minus the
//!   ancestors of `path.head` (the RFC's definition), a fork is a step with
//!   more than one child, a merge a step with more than one parent.
//! * `core_types@validate`: what `path p validate` says about each valid
//!   document and its minimal invalid twin, one row per rule. A row whose
//!   `accepted` differs from what the RFC requires (`rfc_says`) is a gap the
//!   validator does not enforce yet; it is pinned as it is, not hidden.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::{Value, json};
use toolpath::v1::{Graph, PathOrRef, query};

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("json")
}

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn example(name: &str) -> Value {
    let bytes = std::fs::read(examples().join(name)).expect("read example");
    serde_json::from_slice(&bytes).expect("example is JSON")
}

#[test]
fn dag_features_of_the_exploration_example() {
    let text = std::fs::read_to_string(examples().join("path-04-exploration.path.json")).unwrap();
    let g = Graph::from_json(&text).expect("exploration example is a Graph");
    let PathOrRef::Path(p) = &g.paths[0] else {
        panic!("inline path")
    };
    let steps = &p.steps;
    let head = &p.path.head;

    let all: BTreeSet<String> = steps.iter().map(|s| s.step.id.clone()).collect();
    let ancestors: BTreeSet<String> = query::ancestors(steps, head).into_iter().collect();
    let dead: BTreeSet<String> = query::dead_ends(steps, head)
        .into_iter()
        .map(|s| s.step.id.clone())
        .collect();
    let rfc_dead: BTreeSet<String> = all.difference(&ancestors).cloned().collect();
    assert_eq!(
        dead, rfc_dead,
        "dead ends are all steps minus ancestors(head)"
    );

    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in steps {
        for parent in &s.step.parents {
            children
                .entry(parent.clone())
                .or_default()
                .push(s.step.id.clone());
        }
    }
    let forks: BTreeMap<String, Vec<String>> =
        children.into_iter().filter(|(_, c)| c.len() > 1).collect();
    let merges: BTreeMap<String, Vec<String>> = steps
        .iter()
        .filter(|s| s.step.parents.len() > 1)
        .map(|s| (s.step.id.clone(), s.step.parents.clone()))
        .collect();
    let actors: BTreeSet<String> = steps
        .iter()
        .filter_map(|s| s.step.actor.split(':').next().map(str::to_string))
        .collect();

    insta::assert_snapshot!(
        "core_types@dag",
        pretty(&json!({
            "head": head,
            "base": p.path.base,
            "steps": all,
            "ancestors_of_head": ancestors,
            "dead_ends": dead,
            "forks": forks,
            "merges": merges,
            "actor_kinds": actors,
        }))
    );
}

/// One rule: a document, the RFC's verdict on it, and what `path p validate` says.
struct Case {
    rule: &'static str,
    rfc_says: &'static str,
    doc: Value,
    suffix: &'static str,
}

fn with(mut v: Value, f: impl FnOnce(&mut Value)) -> Value {
    f(&mut v);
    v
}

fn cases() -> Vec<Case> {
    let minimal = example("step-01-minimal.json");
    let exploration = example("path-04-exploration.path.json");
    let step_only = minimal["paths"][0]["steps"][0].clone();
    let path_only = minimal["paths"][0].clone();
    vec![
        Case {
            rule: "graph is the root (step-01-minimal)",
            rfc_says: "valid",
            doc: minimal.clone(),
            suffix: ".path.json",
        },
        Case {
            rule: "a step alone is not a document root",
            rfc_says: "invalid",
            doc: step_only,
            suffix: ".path.json",
        },
        Case {
            rule: "a path alone is not a document root",
            rfc_says: "invalid",
            doc: path_only,
            suffix: ".path.json",
        },
        Case {
            rule: "fork, merge and dead end (path-04-exploration)",
            rfc_says: "valid",
            doc: exploration.clone(),
            suffix: ".path.json",
        },
        Case {
            rule: "step ids are unique within a path",
            rfc_says: "invalid",
            doc: with(exploration.clone(), |v| {
                let first = v["paths"][0]["steps"][0].clone();
                v["paths"][0]["steps"].as_array_mut().unwrap().push(first);
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "path ids are unique within a graph",
            rfc_says: "invalid",
            doc: with(minimal.clone(), |v| {
                let p = v["paths"][0].clone();
                v["paths"].as_array_mut().unwrap().push(p);
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "a parent reference names a step in the path",
            rfc_says: "invalid",
            doc: with(minimal.clone(), |v| {
                v["paths"][0]["steps"][0]["step"]["parents"] = json!(["step-does-not-exist"]);
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "path.head names a step in the path",
            rfc_says: "invalid",
            doc: with(minimal.clone(), |v| {
                v["paths"][0]["path"]["head"] = json!("step-missing")
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "actors human:, agent:, tool:, ci:",
            rfc_says: "valid",
            doc: with(minimal.clone(), |v| {
                let mut steps = vec![];
                for (i, a) in [
                    "human:alex",
                    "agent:claude-code/session-xyz",
                    "tool:rustfmt/1.5.0",
                    "ci:github-actions/workflow-123",
                ]
                .iter()
                .enumerate()
                {
                    let mut s = v["paths"][0]["steps"][0].clone();
                    s["step"]["id"] = json!(format!("s{i}"));
                    s["step"]["actor"] = json!(a);
                    if i > 0 {
                        s["step"]["parents"] = json!([format!("s{}", i - 1)]);
                    }
                    steps.push(s);
                }
                v["paths"][0]["steps"] = json!(steps);
                v["paths"][0]["path"]["head"] = json!("s3");
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "an actor prefix is one of human, agent, tool, ci",
            rfc_says: "invalid",
            doc: with(minimal.clone(), |v| {
                v["paths"][0]["steps"][0]["step"]["actor"] = json!("robot:r2")
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "perspectives: raw and structural on one artifact",
            rfc_says: "valid",
            doc: example("step-02-agent.json"),
            suffix: ".path.json",
        },
        Case {
            rule: "meta.intent and meta.refs",
            rfc_says: "valid",
            doc: example("path-01-pr.path.json"),
            suffix: ".path.json",
        },
        Case {
            rule: "base: toolpath:<path-id>/<step-id>",
            rfc_says: "valid",
            doc: with(minimal.clone(), |v| {
                v["paths"][0]["path"]["base"] = json!({"uri": "toolpath:path-main/step-005"})
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "a $ref path to a sealed .path.json",
            rfc_says: "valid",
            doc: with(minimal.clone(), |v| {
                v["paths"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"$ref": "https://example.com/toolpath/path-44.path.json"}));
            }),
            suffix: ".path.json",
        },
        Case {
            rule: "a $ref MUST NOT point to a .path.jsonl stream",
            rfc_says: "invalid",
            doc: with(minimal.clone(), |v| {
                v["paths"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"$ref": "https://example.com/toolpath/path-44.path.jsonl"}));
            }),
            suffix: ".path.json",
        },
    ]
}

#[test]
fn validate_on_each_type_rule_and_its_invalid_twin() {
    let dir = tempfile::tempdir().unwrap();
    let mut rows = Vec::new();
    let mut gaps = Vec::new();
    for (i, c) in cases().into_iter().enumerate() {
        let file = dir.path().join(format!("case-{i:02}{}", c.suffix));
        std::fs::write(&file, serde_json::to_vec_pretty(&c.doc).unwrap()).unwrap();
        let out = Command::cargo_bin("path")
            .unwrap()
            .env("HOME", dir.path())
            .args(["p", "validate", "--input"])
            .arg(&file)
            .output()
            .expect("run path p validate");
        let accepted = out.status.success();
        let expected = c.rfc_says == "valid";
        if accepted != expected {
            gaps.push(c.rule);
        }
        rows.push(json!({
            "rule": c.rule,
            "rfc_says": c.rfc_says,
            "accepted": accepted,
            "agrees_with_rfc": accepted == expected,
        }));
    }
    insta::assert_snapshot!(
        "core_types@validate",
        pretty(&json!({ "rows": rows, "validator_does_not_enforce": gaps }))
    );
    // Everything the RFC calls valid must validate; the invalid twins it
    // misses are pinned above (`validator_does_not_enforce`), not failed here.
    let false_rejections: Vec<&str> = cases()
        .iter()
        .zip(&rows)
        .filter(|(c, r)| c.rfc_says == "valid" && r["accepted"] == json!(false))
        .map(|(c, _)| c.rule)
        .collect();
    assert!(
        false_rejections.is_empty(),
        "the validator rejects documents the RFC calls valid: {false_rejections:?}"
    );
}
