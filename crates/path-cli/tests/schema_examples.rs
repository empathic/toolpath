//! Schema-conformance test for the canonical example documents.
//!
//! Every file under `examples/*.json` is the project's own demonstration of
//! the format. They MUST validate against `schema/toolpath.schema.json` —
//! otherwise the schema, the Rust types, and the published examples have
//! drifted apart.

use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/path-cli/. Walk up two levels.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/path-cli/ has a workspace root above it")
        .to_path_buf()
}

fn load_schema() -> serde_json::Value {
    let path = workspace_root().join("schema/toolpath.schema.json");
    let bytes =
        fs::read(&path).unwrap_or_else(|e| panic!("read schema at {}: {e}", path.display()));
    serde_json::from_slice(&bytes).expect("schema parses as JSON")
}

fn example_files() -> Vec<PathBuf> {
    let dir = workspace_root().join("examples");
    let mut out: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read examples dir at {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "expected at least one example fixture");
    out
}

#[test]
fn every_example_satisfies_the_schema() {
    let schema = load_schema();
    let validator = jsonschema::validator_for(&schema)
        .expect("schema/toolpath.schema.json is itself a valid JSON Schema");

    let mut failures: Vec<String> = Vec::new();

    for file in example_files() {
        let bytes = fs::read(&file).expect("read example");
        let instance: serde_json::Value =
            serde_json::from_slice(&bytes).expect("example parses as JSON");

        let errors: Vec<_> = validator.iter_errors(&instance).collect();
        if !errors.is_empty() {
            let rel = file
                .strip_prefix(workspace_root())
                .unwrap_or(&file)
                .display()
                .to_string();
            let mut detail = format!("{rel}:");
            for err in errors.iter().take(5) {
                let pointer = err.instance_path().as_str();
                detail.push_str(&format!("\n    at {pointer}: {err}"));
            }
            if errors.len() > 5 {
                detail.push_str(&format!("\n    … and {} more", errors.len() - 5));
            }
            failures.push(detail);
        }
    }

    assert!(
        failures.is_empty(),
        "{} example fixture(s) violate schema/toolpath.schema.json:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
