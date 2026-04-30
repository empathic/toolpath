//! JSON Schema validation against the canonical `toolpath.schema.json`.
//!
//! The schema bytes are sourced from [`toolpath::SCHEMA_JSON`], which is
//! `include_str!`-baked into the `toolpath` crate. Hosting the const in the
//! types crate (rather than vendoring a copy here) keeps the schema next
//! to the types it describes and means there's exactly one source of truth.

use std::sync::OnceLock;

use jsonschema::Validator;

const SCHEMA_SOURCE: &str = toolpath::SCHEMA_JSON;

fn validator() -> &'static Validator {
    static VALIDATOR: OnceLock<Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA_SOURCE)
            .expect("toolpath.schema.json embedded in binary parses as JSON");
        jsonschema::validator_for(&schema)
            .expect("toolpath.schema.json embedded in binary is itself a valid JSON Schema")
    })
}

/// Validate a parsed JSON value against `toolpath.schema.json`.
///
/// Returns `Ok(())` when valid; returns an `anyhow::Error` whose Display
/// concatenates each schema violation (one per line, prefixed with the JSON
/// pointer to the offending location).
pub fn validate(instance: &serde_json::Value) -> anyhow::Result<()> {
    let errors: Vec<String> = validator()
        .iter_errors(instance)
        .map(|err| {
            let pointer = err.instance_path().as_str();
            let location = if pointer.is_empty() { "/" } else { pointer };
            format!("  at {location}: {err}")
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "schema validation failed:\n{}",
            errors.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn embedded_schema_compiles() {
        let _ = validator();
    }

    #[test]
    fn empty_graph_is_valid() {
        validate(&json!({"graph": {"id": "g1"}, "paths": []}))
            .expect("an empty graph is the simplest valid document");
    }

    #[test]
    fn single_path_graph_is_valid() {
        let doc = json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {"id": "p1", "head": "s1"},
                "steps": [{
                    "step": {
                        "id": "s1",
                        "actor": "human:alex",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"src/main.rs": {"raw": "@@ -1 +1 @@\n-old\n+new"}}
                }]
            }]
        });
        validate(&doc).expect("single-path single-step graph should validate");
    }

    /// `path.base` accepts `uri`, `ref`, and `branch`. Anything else (a
    /// stray `commit` field, for example) must be flagged.
    #[test]
    fn path_base_rejects_unknown_field() {
        let doc = json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {
                    "id": "p1",
                    "base": {
                        "uri": "github:org/repo",
                        "ref": "abc123",
                        "commit": "abc123"
                    },
                    "head": "s1"
                },
                "steps": [{
                    "step": {
                        "id": "s1",
                        "actor": "human:alex",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"src/main.rs": {"raw": "@@ -1 +1 @@\n-a\n+b"}}
                }]
            }]
        });
        let err = validate(&doc).expect_err("commit is not a permitted base property");
        let msg = err.to_string();
        assert!(
            msg.contains("commit"),
            "error should mention the offending field, got: {msg}"
        );
    }

    /// `path.base.branch` is the human VCS label (branch name); it stands
    /// alongside `ref` (the immutable VCS state identifier).
    #[test]
    fn path_base_accepts_branch_alongside_ref() {
        let doc = json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {
                    "id": "p1",
                    "base": {
                        "uri": "github:org/repo",
                        "ref": "abc123def456",
                        "branch": "main"
                    },
                    "head": "s1"
                },
                "steps": [{
                    "step": {
                        "id": "s1",
                        "actor": "human:alex",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"src/main.rs": {"raw": "@@ -1 +1 @@\n-a\n+b"}}
                }]
            }]
        });
        validate(&doc).expect("base may carry both ref and branch");
    }

    /// `path.base` is optional: a Graph that wraps a single step (the shape
    /// of the new `step-NN.json` example fixtures) has no base, and that's
    /// fine.
    #[test]
    fn path_without_base_is_valid() {
        let doc = json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {"id": "p1", "head": "s1"},
                "steps": [{
                    "step": {
                        "id": "s1",
                        "actor": "human:alex",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"src/main.rs": {"raw": "@@ -1 +1 @@\n-a\n+b"}}
                }]
            }]
        });
        validate(&doc).expect("base is optional on path identity");
    }
}
