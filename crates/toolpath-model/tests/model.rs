//! The committed model keeps its own rules. It is generated (`cargo run -p
//! toolpath-model`), so a failure here means the work moved and the records did
//! not: a planned piece names commits that are not on the branch, a claim reads
//! as pinned with no snapshot behind it, or a known defect has no piece or
//! concern that owns it. Fix the record or the code, then regenerate.

use std::path::Path;

#[test]
fn committed_model_keeps_its_rules() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/model/model.json");
    let text = std::fs::read_to_string(&path)
        .expect("docs/model/model.json (generate it with cargo run -p toolpath-model)");
    let model: serde_json::Value = serde_json::from_str(&text).expect("model.json is JSON");
    assert_eq!(model["format"], toolpath_model::FORMAT);
    for k in [
        "harnesses",
        "operations",
        "sets",
        "snapshots",
        "findings",
        "claims",
        "pieces",
        "concerns",
        "feedback",
    ] {
        assert!(model[k].is_array(), "{k} is a list");
    }
    let schema_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/model/model.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path).expect("model.schema.json"))
            .expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("a valid JSON Schema");
    let errors: Vec<String> = validator
        .iter_errors(&model)
        .take(10)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "model.json does not match model.schema.json:\n{}",
        errors.join("\n")
    );
    let bad = toolpath_model::check(&model);
    assert!(
        bad.is_empty(),
        "the model breaks its rules:\n{}",
        bad.join("\n")
    );
}
