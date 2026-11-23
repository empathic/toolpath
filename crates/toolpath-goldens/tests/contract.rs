//! Each writer's declared transform (`fixtures/contract/<agent>.json`)
//! against what the writer stage measures on the generated document
//! (`fixtures/full-view.json`), in both directions: every measured loss and
//! gain has an op that explains it, every op has an effect, and every
//! synthesized value is the one declared. The contract is written up front;
//! a snapshot is accepted after the fact.

use std::collections::BTreeSet;

use toolpath::v1::Graph;
use toolpath_convo::{ConversationView, DeriveConfig, derive_path};
use toolpath_goldens::model::{Agent, RoundTrip};
use toolpath_goldens::pipeline::{projection, writer_with_document};
use toolpath_goldens::transform::{Contract, Measured, check, schema};

const VIEW: &str = include_str!("../fixtures/full-view.json");

fn document() -> String {
    let view: ConversationView =
        serde_json::from_str(VIEW).expect("fixtures/full-view.json is a ConversationView");
    let cfg = DeriveConfig {
        include_thinking: true,
        include_tool_uses: true,
        ..DeriveConfig::default()
    };
    Graph::from_path(derive_path(&view, &cfg))
        .to_json()
        .expect("document serializes")
}

fn contract(agent: Agent) -> Contract {
    let path = format!(
        "{}/fixtures/contract/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        agent.name()
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let c: Contract = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(c.writer, agent.name(), "{path} names another writer");
    c
}

#[test]
fn every_writer_matches_its_declared_transform() {
    let home = tempfile::tempdir().expect("temp HOME");
    let document_json = document();
    let before: serde_json::Value = serde_json::from_str(&document_json).unwrap();
    let mut failures = Vec::new();
    for agent in Agent::TARGETS {
        let (trip, after) = writer_with_document(&document_json, agent, home.path())
            .unwrap_or_else(|e| panic!("writer {}: {e}", agent.name()));
        let (RoundTrip::Compared { lost, gained, .. }, Some(after)) = (trip, after) else {
            failures.push(format!("{}: its own adapter cannot read it", agent.name()));
            continue;
        };
        let lost: BTreeSet<String> = lost.iter().map(|k| k.as_str().to_owned()).collect();
        let gained: BTreeSet<String> = gained.iter().map(|k| k.as_str().to_owned()).collect();
        let m = Measured {
            before: &before,
            after: &after,
            lost: &lost,
            gained: &gained,
        };
        for f in check(&contract(agent), &m) {
            failures.push(format!("{}: {f}", agent.name()));
        }
    }
    assert!(
        failures.is_empty(),
        "writers disagree with fixtures/contract/<agent>.json:\n{}",
        failures.join("\n")
    );
}

/// Each writer's declared synthesized defaults (`SYNTHESIZED` in its crate)
/// are literally present in its projection of the generated document, so the
/// table stays honest.
#[test]
fn declared_synthesized_values_are_written() {
    let document_json = document();
    let tables: [(Agent, &[(&str, &str)]); 4] = [
        (Agent::Claude, toolpath_claude::project::SYNTHESIZED),
        (Agent::Codex, toolpath_codex::project::SYNTHESIZED),
        (Agent::Opencode, toolpath_opencode::project::SYNTHESIZED),
        (Agent::Pi, toolpath_pi::project::SYNTHESIZED),
    ];
    for (agent, table) in tables {
        let text = projection(&document_json, agent).expect("projects");
        for (key, value) in table {
            // compact (jsonl writers) or pretty-printed (opencode's session JSON)
            let needle = format!("\"{key}\":\"{value}\"");
            let pretty = format!("\"{key}\": \"{value}\"");
            assert!(
                text.contains(&needle) || text.contains(&pretty),
                "{}: SYNTHESIZED declares {key}={value} but the projection has no {needle}",
                agent.name()
            );
        }
    }
}

/// The committed JSON Schema of the transform IR is what schemars derives.
#[test]
fn transform_schema_is_committed() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/baseline/schemas/transform.schema.json"
    );
    let want = serde_json::to_string_pretty(&schema()).unwrap() + "\n";
    if std::env::var_os("UPDATE_SCHEMAS").is_some() {
        std::fs::write(path, &want).unwrap();
    }
    let have = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        have, want,
        "regenerate with UPDATE_SCHEMAS=1 cargo test -p toolpath-goldens --test contract"
    );
}

/// Negative controls: a changed codex default, an op with no effect and an
/// undeclared loss are each named, so the check above cannot pass vacuously.
#[test]
fn the_check_names_what_disagrees() {
    use toolpath_goldens::transform::Op;
    let home = tempfile::tempdir().expect("temp HOME");
    let document_json = document();
    let before: serde_json::Value = serde_json::from_str(&document_json).unwrap();
    let (trip, after) = writer_with_document(&document_json, Agent::Codex, home.path()).unwrap();
    let (RoundTrip::Compared { lost, gained, .. }, Some(after)) = (trip, after) else {
        panic!("codex reads its own projection");
    };
    let lost: BTreeSet<String> = lost.iter().map(|k| k.as_str().to_owned()).collect();
    let gained: BTreeSet<String> = gained.iter().map(|k| k.as_str().to_owned()).collect();
    let m = Measured {
        before: &before,
        after: &after,
        lost: &lost,
        gained: &gained,
    };
    let model_provider = "paths[].steps[].change.<artifact>.structural.model_provider";

    // The writer's default changed (here: the contract says anthropic).
    let mut c = contract(Agent::Codex);
    for op in &mut c.ops {
        if let Op::Synthesize { path, value, .. } = op
            && path == model_provider
        {
            *value = Some("anthropic".into());
        }
    }
    let f = check(&c, &m);
    assert!(
        f.iter()
            .any(|x| x.contains(model_provider) && x.contains("\"openai\"")),
        "{f:?}"
    );

    // An op with no effect: declaring a kept path lost.
    let mut c = contract(Agent::Codex);
    let kept = "paths[].steps[].change.<artifact>.structural.text";
    c.ops.push(Op::Drop {
        path: kept.into(),
        why: "control".into(),
        defect: None,
    });
    assert!(
        check(&c, &m)
            .iter()
            .any(|x| x == &format!("declared lost but kept: {kept}"))
    );

    // An undeclared loss: removing a drop op.
    let mut c = contract(Agent::Codex);
    c.ops
        .retain(|op| !matches!(op, Op::Drop { path, .. } if path == "paths[].meta.vcs_remote"));
    assert!(
        check(&c, &m)
            .iter()
            .any(|x| x == "lost but not declared: paths[].meta.vcs_remote")
    );
}
