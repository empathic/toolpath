//! Generate a typed Pathbase client from `openapi.json`.
//!
//! Pathbase emits OpenAPI 3.1, but progenitor's `openapiv3` crate only
//! understands 3.0. We downgrade in-place: `"type": ["string", "null"]`
//! becomes `"type": "string", "nullable": true`, and the document version
//! is rewritten to `3.0.3`. The committed spec stays faithful to what the
//! server actually publishes.
//!
//! **Exit strategy:** delete `downgrade_to_oas_30` when either of the
//! following ships and we're consuming it:
//!
//! - progenitor / openapiv3 grow first-class OAS 3.1 support — tracking
//!   <https://github.com/oxidecomputer/progenitor/issues/762> and
//!   <https://github.com/oxidecomputer/progenitor/issues/1268>.
//! - The Pathbase server emits OAS 3.0 instead of 3.1.
//!
//! Once the canonical document parses without preprocessing, the
//! `downgrade_to_oas_30` / `rewrite_nullable_types` /
//! `fill_empty_media_types` helpers come out and `build.rs` is ~15 lines.
use std::env;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

fn main() {
    // Spec lives next to the crate so `cargo publish` packages it with the
    // rest of the source. (When the crate is unpacked from crates.io, the
    // workspace's schema/ directory doesn't exist.)
    let spec_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
    println!("cargo:rerun-if-changed={}", spec_path.display());

    let spec_text = fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", spec_path.display()));
    let mut spec: Value = serde_json::from_str(&spec_text).expect("parse openapi.json");

    downgrade_to_oas_30(&mut spec);

    let spec: openapiv3::OpenAPI =
        serde_json::from_value(spec).expect("downgraded spec doesn't match openapiv3 model");

    let mut generator = progenitor::Generator::default();
    let tokens = generator
        .generate_tokens(&spec)
        .expect("progenitor generation failed");
    let ast = syn::parse2::<syn::File>(tokens).expect("parse generated tokens");
    let formatted = prettyplease::unparse(&ast);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let out_file = out_dir.join("pathbase_client.rs");
    fs::write(&out_file, formatted).unwrap_or_else(|e| panic!("write {}: {e}", out_file.display()));
}

/// Walk the document and rewrite OAS 3.1 idioms into OAS 3.0 ones.
fn downgrade_to_oas_30(value: &mut Value) {
    if let Value::Object(map) = value
        && let Some(version) = map.get_mut("openapi")
        && version.as_str().is_some_and(|s| s.starts_with("3.1"))
    {
        *version = Value::String("3.0.3".to_string());
    }
    rewrite_nullable_types(value);
    fill_empty_media_types(value);
}

/// Progenitor's codegen panics on `"content": {"<mime>": {}}` (no schema).
/// Inject a permissive `{"type": "object"}` schema so the generator
/// produces a `serde_json::Value`-typed handler. The download endpoint in
/// pathbase's spec is the only current offender, but this is generic.
fn fill_empty_media_types(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(content)) = map.get_mut("content") {
                for media in content.values_mut() {
                    if let Value::Object(media_map) = media
                        && !media_map.contains_key("schema")
                    {
                        let mut schema = serde_json::Map::new();
                        schema.insert("type".into(), Value::String("object".into()));
                        media_map.insert("schema".into(), Value::Object(schema));
                    }
                }
            }
            for v in map.values_mut() {
                fill_empty_media_types(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                fill_empty_media_types(v);
            }
        }
        _ => {}
    }
}

/// Convert `"type": ["X", "null"]` (OAS 3.1) to `"type": "X", "nullable": true`
/// (OAS 3.0). Recurses through every array and object.
fn rewrite_nullable_types(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Detect the pattern at this level.
            if let Some(Value::Array(arr)) = map.get("type") {
                let strings: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                if strings.len() == arr.len() {
                    let nullable = strings.iter().any(|s| s == "null");
                    let non_null: Vec<String> =
                        strings.into_iter().filter(|s| s != "null").collect();
                    if nullable && non_null.len() == 1 {
                        map.insert("type".into(), Value::String(non_null[0].clone()));
                        map.insert("nullable".into(), Value::Bool(true));
                    } else if non_null.len() == 1 {
                        map.insert("type".into(), Value::String(non_null[0].clone()));
                    }
                    // If multiple non-null types remain, leave it alone — progenitor
                    // will surface a clear error and we can extend the converter.
                }
            }
            for v in map.values_mut() {
                rewrite_nullable_types(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                rewrite_nullable_types(v);
            }
        }
        _ => {}
    }
}
