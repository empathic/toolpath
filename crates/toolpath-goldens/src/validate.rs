//! The validation `path p validate` runs: the base `toolpath.schema.json`
//! (the bytes the `toolpath` crate embeds), then for every path whose
//! `meta.kind` names a bundled kind, that kind's schema on top. The kind
//! schemas are the files `crates/path-cli/kinds/**/schema.json` that
//! toolpath.net/kinds publishes; path-cli's `schema` module is private, so
//! this is the same logic over the same bytes.

use std::sync::OnceLock;

use jsonschema::Validator;
use serde_json::Value;

use crate::model::Validation;

/// Every kind spec `path` bundles, by URI, oldest first.
const KINDS: &[(&str, &str)] = &[
    (
        toolpath::v1::PATH_KIND_AGENT_CODING_SESSION_V1_0_0,
        include_str!("../../path-cli/kinds/agent-coding-session/v1.0.0/schema.json"),
    ),
    (
        toolpath::v1::PATH_KIND_AGENT_CODING_SESSION,
        include_str!("../../path-cli/kinds/agent-coding-session/v1.1.0/schema.json"),
    ),
];

fn compile(name: &str, schema: &str) -> Validator {
    let schema: Value =
        serde_json::from_str(schema).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
    jsonschema::validator_for(&schema)
        .unwrap_or_else(|e| panic!("{name} is not a valid JSON Schema: {e}"))
}

fn base() -> &'static Validator {
    static BASE: OnceLock<Validator> = OnceLock::new();
    BASE.get_or_init(|| compile("toolpath.schema.json", toolpath::SCHEMA_JSON))
}

fn kinds() -> &'static [(&'static str, Validator)] {
    static KIND: OnceLock<Vec<(&'static str, Validator)>> = OnceLock::new();
    KIND.get_or_init(|| {
        KINDS
            .iter()
            .map(|(uri, schema)| (*uri, compile(uri, schema)))
            .collect()
    })
}

/// A document against the base schema and the kind each of its paths
/// claims. An unknown kind imposes nothing, as `path p validate` intends.
pub fn validate(document: &Value) -> Validation {
    let mut errors: Vec<String> = base()
        .iter_errors(document)
        .map(|err| {
            let pointer = err.instance_path().as_str();
            let location = if pointer.is_empty() { "/" } else { pointer };
            format!("at {location}: {err}")
        })
        .collect();
    if let Some(paths) = document["paths"].as_array() {
        for (i, path) in paths.iter().enumerate() {
            let Some(kind) = path.pointer("/meta/kind").and_then(Value::as_str) else {
                continue;
            };
            let Some((_, validator)) = kinds().iter().find(|(uri, _)| *uri == kind) else {
                continue;
            };
            errors.extend(
                validator.iter_errors(path).map(|err| {
                    format!("at /paths/{i}{}: {err} (kind {kind})", err.instance_path())
                }),
            );
        }
    }
    if errors.is_empty() {
        Validation::Valid
    } else {
        Validation::Invalid { errors }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(append: Value) -> Value {
        json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {"id": "p1", "head": "s1"},
                "meta": {"kind": toolpath::v1::PATH_KIND_AGENT_CODING_SESSION},
                "steps": [{
                    "step": {"id": "s1", "actor": "human:user", "timestamp": "2026-01-29T10:00:00Z"},
                    "change": {"agent://claude-code/s1": {"structural": append}}
                }]
            }]
        })
    }

    #[test]
    fn a_well_formed_session_is_valid() {
        let doc = session(json!({"type": "conversation.append", "role": "user", "text": "hi"}));
        assert_eq!(validate(&doc), Validation::Valid);
    }

    #[test]
    fn a_kind_violation_names_the_field_and_the_kind() {
        let doc = session(json!({"type": "conversation.append", "role": "user"}));
        let Validation::Invalid { errors } = validate(&doc) else {
            panic!("missing `text` violates the kind");
        };
        assert!(errors.iter().any(|e| e.contains("text")), "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.contains(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)),
            "{errors:?}"
        );
    }

    #[test]
    fn an_unknown_kind_is_a_generic_path() {
        let mut doc = session(json!({"type": "conversation.append", "role": "user"}));
        doc["paths"][0]["meta"]["kind"] = json!("https://toolpath.net/kinds/made-up/v9.9.9");
        assert_eq!(validate(&doc), Validation::Valid);
    }

    #[test]
    fn a_base_violation_carries_its_pointer() {
        let Validation::Invalid { errors } = validate(&json!({"paths": "no"})) else {
            panic!("a string is not a paths array");
        };
        assert!(errors.iter().any(|e| e.starts_with("at /")), "{errors:?}");
    }
}
