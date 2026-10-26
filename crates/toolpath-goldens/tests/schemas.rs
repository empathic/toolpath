//! The JSON Schema of every snapshot kind, and every committed snapshot
//! checked against its kind's schema.
//!
//! * `generated_schemas_are_current`: the schemas generated from the model
//!   types (`toolpath_goldens::schemas::generated`) equal the committed files
//!   under `docs/baseline/schemas/`. `UPDATE_SCHEMAS=1` rewrites them.
//! * `every_snapshot_matches_its_kind`: each `snapshots/<kind>@….snap` body
//!   (the JSON after the insta header) validates against
//!   `<kind>.schema.json`. `document@` bodies are Toolpath documents: they are
//!   checked the way `path p validate` checks them (base schema plus the
//!   bundled kind). `projection@` bodies are harness-native files and `ir@`
//!   bodies are conversation views: neither has a schema here (see the
//!   README beside the schemas).
//! * `recording_provenance_matches`: every `cassettes/<harness>/provenance.json`
//!   validates against `provenance.schema.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toolpath_goldens::model::Validation;
use toolpath_goldens::schemas;
use toolpath_goldens::validate::validate;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots");

/// Snapshot kinds with no schema here, and why.
const NO_SCHEMA: &[(&str, &str)] = &[
    (
        "projection",
        "harness-native: see docs/agents/formats/<harness>",
    ),
    (
        "ir",
        "a toolpath-convo ConversationView, which has no JSON Schema yet",
    ),
];

fn root() -> PathBuf {
    Path::new(ROOT).to_path_buf()
}

fn schema(kind: &str) -> Value {
    let path = root().join(schemas::file(kind));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (no schema for kind {kind})", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The JSON body of an insta snapshot: everything after the header's closing
/// `---`.
fn body(snap: &str) -> &str {
    let rest = snap
        .strip_prefix("---\n")
        .expect("an insta header opens the file");
    let end = rest.find("\n---\n").expect("an insta header closes");
    &rest[end + "\n---\n".len()..]
}

fn errors(schema: &Value, instance: &Value) -> Vec<String> {
    let v = jsonschema::validator_for(schema).expect("the schema compiles");
    v.iter_errors(instance)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect()
}

#[test]
fn generated_schemas_are_current() {
    let update = std::env::var_os("UPDATE_SCHEMAS").is_some_and(|v| !v.is_empty());
    let mut stale = Vec::new();
    for (kind, generated) in schemas::generated() {
        let path = root().join(schemas::file(kind));
        let want = schemas::render(&generated);
        if update {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &want).unwrap();
            continue;
        }
        if fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
            stale.push(kind);
        }
    }
    assert!(
        stale.is_empty(),
        "generated schemas differ from {}: {stale:?}. Regenerate: UPDATE_SCHEMAS=1 cargo test -p toolpath-goldens --test schemas",
        schemas::DIR
    );
}

#[test]
fn every_snapshot_matches_its_kind() {
    let mut names: Vec<_> = fs::read_dir(SNAPSHOTS)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".snap"))
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut checked = 0;
    for name in &names {
        let kind = name.split('@').next().unwrap();
        if NO_SCHEMA.iter().any(|(k, _)| *k == kind) {
            continue;
        }
        let text = fs::read_to_string(Path::new(SNAPSHOTS).join(name)).unwrap();
        let instance: Value = serde_json::from_str(body(&text))
            .unwrap_or_else(|e| panic!("{name}: body is not JSON: {e}"));
        let errs = if kind == "document" {
            match validate(&instance) {
                Validation::Valid => vec![],
                Validation::Invalid { errors } => errors,
                Validation::NotDerived => vec!["not derived".into()],
            }
        } else {
            errors(&schema(kind), &instance)
        };
        if !errs.is_empty() {
            failures.push(format!("{name}:\n  {}", errs.join("\n  ")));
        }
        checked += 1;
    }
    assert!(
        failures.is_empty(),
        "snapshots that break their kind's schema:\n{}",
        failures.join("\n")
    );
    assert!(
        checked > 100,
        "suspiciously few snapshots checked: {checked}"
    );
}

#[test]
fn recording_provenance_matches() {
    let schema = schema("provenance");
    let dir = root().join("crates/toolpath-webmock/cassettes");
    let mut checked = 0;
    let mut failures = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path().join("provenance.json");
        if !p.exists() {
            continue;
        }
        let instance: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        let errs = errors(&schema, &instance);
        if !errs.is_empty() {
            failures.push(format!("{}:\n  {}", p.display(), errs.join("\n  ")));
        }
        checked += 1;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        checked >= 7,
        "expected a provenance file per recorded harness, found {checked}"
    );
}
