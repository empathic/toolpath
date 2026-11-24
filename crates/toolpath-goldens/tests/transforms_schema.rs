//! A schema check on every transform: each document a transform produces is
//! validated against the base schema and the `agent-coding-session` v1.1.0
//! kind schema (the bundled `crates/path-cli/kinds/agent-coding-session/
//! v1.1.0/schema.json`, which is what toolpath.net publishes: see
//! `bundled_kind_schema_is_the_published_one`).
//!
//! Transforms covered, per golden set (`validate_transforms@<set>`):
//! * `document`: the derived document (the pinned `document@<set>`);
//! * `round_trips.<agent>`: that document projected for each other agent,
//!   read back by that agent's adapter and derived again (the 54 pairs);
//! * `jsonl_sealed`: the document streamed to `.path.jsonl` and sealed back.
//!
//! And `validate_transforms@writers`: the generated full document through
//! each writer alone, derived again.
//!
//! Each entry records the outcome and the `meta.kind` of every path. A path
//! with no kind would skip the kind schema silently, so a document that
//! claims no kind fails here by name, as does any schema error.

use std::path::Path;

use serde_json::{Value, json};
use toolpath::v1::Graph;
use toolpath_convo::{ConversationView, DeriveConfig, derive_path};
use toolpath_goldens::model::{Agent, Validation};
use toolpath_goldens::pipeline::writer_with_document;
use toolpath_goldens::validate::validate;

const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots");
const KIND: &str = toolpath::v1::PATH_KIND_AGENT_CODING_SESSION;
const VIEW: &str = include_str!("../fixtures/full-view.json");

fn snapshot(path: &Path) -> (String, String) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut parts = text.splitn(3, "---\n");
    parts.next();
    let header = parts.next().unwrap().to_string();
    (header, parts.next().unwrap().to_string())
}

fn kinds(doc: &Value) -> Vec<Value> {
    doc["paths"]
        .as_array()
        .map(|ps| {
            ps.iter()
                .map(|p| p.pointer("/meta/kind").cloned().unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default()
}

/// Outcome of one transform: schema result plus the kinds its paths claim.
fn check(doc: Option<&Value>, failures: &mut Vec<String>, label: &str) -> Value {
    let Some(doc) = doc else {
        return json!({ "outcome": "not_derived" });
    };
    let ks = kinds(doc);
    if ks.iter().any(|k| k.as_str() != Some(KIND)) {
        failures.push(format!(
            "{label}: a path does not claim {KIND} (kinds {ks:?})"
        ));
    }
    match validate(doc) {
        Validation::Valid => json!({ "outcome": "valid", "kinds": ks }),
        Validation::Invalid { errors } => {
            failures.push(format!("{label}: {}", errors.join("; ")));
            json!({ "outcome": "invalid", "kinds": ks, "errors": errors })
        }
        Validation::NotDerived => json!({ "outcome": "not_derived" }),
    }
}

fn agent_named(name: &str) -> Agent {
    *Agent::TARGETS
        .iter()
        .find(|a| a.name() == name)
        .expect("known agent")
}

fn point_every_store_at(home: &Path) {
    let vars = [
        ("HOME", home.to_path_buf()),
        ("TMPDIR", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CACHE_HOME", home.join(".cache")),
        ("CLAUDE_CONFIG_DIR", home.join(".claude")),
        ("CODEX_HOME", home.join(".codex")),
        ("COPILOT_HOME", home.join(".copilot")),
        ("TOOLPATH_CONFIG_DIR", home.join(".toolpath")),
    ];
    for (k, v) in vars {
        // SAFETY: one test in this binary, set before any adapter runs.
        unsafe { std::env::set_var(k, v) };
    }
}

#[test]
fn every_transform_validates_against_the_v1_1_0_kind() {
    let home = tempfile::tempdir().unwrap();
    point_every_store_at(home.path());
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.set_sort_maps(true);

    let mut failures = Vec::new();
    let mut docs: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(SNAPSHOTS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let n = p.file_name()?.to_str()?;
            let set = n
                .strip_prefix("document@")?
                .strip_suffix(".snap")?
                .to_string();
            Some((set, p))
        })
        .collect();
    docs.sort();
    // the 9 recordings and the 4 compactions forced in the agents (empathic/toolpath#108)
    assert_eq!(docs.len(), 13);

    settings.bind(|| {
        for (set, path) in &docs {
            let (header, body) = snapshot(path);
            let agent = header
                .lines()
                .find_map(|l| l.trim().strip_prefix("agent: "))
                .map(agent_named)
                .expect("document snapshot names its agent");
            let document: Value = serde_json::from_str(&body).unwrap();
            let mut round_trips = serde_json::Map::new();
            for target in agent.targets() {
                let (_, again) = writer_with_document(&body, target, home.path())
                    .unwrap_or_else(|e| panic!("{set} -> {}: {e}", target.name()));
                round_trips.insert(
                    target.name().to_string(),
                    check(
                        again.as_ref(),
                        &mut failures,
                        &format!("{set} -> {}", target.name()),
                    ),
                );
            }
            let g = Graph::from_json(&body).unwrap();
            let sealed = Graph::from_jsonl_str(&g.to_jsonl_string().unwrap()).unwrap();
            let sealed = serde_json::to_value(&sealed).unwrap();
            let result = json!({
                "document": check(Some(&document), &mut failures, &format!("{set} document")),
                "round_trips": round_trips,
                "jsonl_sealed": check(Some(&sealed), &mut failures, &format!("{set} jsonl")),
            });
            insta::assert_json_snapshot!(format!("validate_transforms@{set}"), result);
        }

        let view: ConversationView = serde_json::from_str(VIEW).unwrap();
        let cfg = DeriveConfig {
            include_thinking: true,
            include_tool_uses: true,
            ..DeriveConfig::default()
        };
        let document_json = Graph::from_path(derive_path(&view, &cfg))
            .to_json()
            .unwrap();
        let mut writers = serde_json::Map::new();
        for agent in Agent::TARGETS {
            let (_, again) = writer_with_document(&document_json, agent, home.path())
                .unwrap_or_else(|e| panic!("writer {}: {e}", agent.name()));
            writers.insert(
                agent.name().to_string(),
                check(
                    again.as_ref(),
                    &mut failures,
                    &format!("writer {}", agent.name()),
                ),
            );
        }
        insta::assert_json_snapshot!("validate_transforms@writers", Value::Object(writers));
    });

    assert!(
        failures.is_empty(),
        "transforms whose document the v1.1.0 kind rejects (or that claim no kind):\n{}",
        failures.join("\n")
    );
}

/// The bundled kind schema and the copy the site publishes
/// (`site/kinds/agent-coding-session/v1.1.0/schema.json`, served at
/// https://toolpath.net/kinds/agent-coding-session/v1.1.0/schema.json) are the
/// same bytes. Checked against the live URL once by hand on 2026-10-19
/// (sha256 0557c0c06fe285704a672ac7f86abbd78f2e6e17e44aed0abbbdcc524ad523da,
/// identical); this keeps the two in-repo copies from drifting.
#[test]
fn bundled_kind_schema_is_the_published_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bundled =
        std::fs::read(root.join("crates/path-cli/kinds/agent-coding-session/v1.1.0/schema.json"))
            .unwrap();
    let site =
        std::fs::read(root.join("site/kinds/agent-coding-session/v1.1.0/schema.json")).unwrap();
    assert!(
        bundled == site,
        "the bundled v1.1.0 kind schema differs from the site's copy"
    );
}
