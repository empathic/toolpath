//! JSON Schema validation against the canonical `toolpath.schema.json`,
//! plus per-path *kind* validation.
//!
//! The base schema bytes are sourced from [`toolpath::SCHEMA_JSON`], which is
//! `include_str!`-baked into the `toolpath` crate. Hosting the const in the
//! types crate (rather than vendoring a copy here) keeps the schema next
//! to the types it describes and means there's exactly one source of truth.
//!
//! Kind schemas are additive constraints a path opts into via `meta.kind`
//! (a URI naming a hosted spec). JSON Schema doesn't dispatch on a field
//! value by itself, so [`validate`] does it: for every path carrying a
//! `meta.kind` we recognize, it applies that kind's schema on top of the
//! base. The kind schema bytes are bundled from `site/kinds/**/schema.json`
//! and matched by their exact URI; an unrecognized `kind` is treated as a
//! generic path (base schema only), exactly as the format intends.

use std::collections::HashMap;
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

/// Compiled validator for each known kind URI, built once on first use.
/// Sourced from [`crate::kinds::BUNDLED_KINDS`] so the validator set and the
/// `path kind` / `path query --kind` surface stay in lockstep.
fn kind_validators() -> &'static HashMap<&'static str, Validator> {
    static VALIDATORS: OnceLock<HashMap<&'static str, Validator>> = OnceLock::new();
    VALIDATORS.get_or_init(|| {
        crate::kinds::BUNDLED_KINDS
            .iter()
            .map(|k| {
                let schema: serde_json::Value =
                    serde_json::from_str(k.schema).unwrap_or_else(|e| {
                        panic!("bundled kind schema {} is not valid JSON: {e}", k.uri)
                    });
                let v = jsonschema::validator_for(&schema).unwrap_or_else(|e| {
                    panic!(
                        "bundled kind schema {} is not a valid JSON Schema: {e}",
                        k.uri
                    )
                });
                (k.uri, v)
            })
            .collect()
    })
}

/// Validate a parsed JSON value against `toolpath.schema.json`.
///
/// Returns `Ok(())` when valid; returns an `anyhow::Error` whose Display
/// concatenates each schema violation (one per line, prefixed with the JSON
/// pointer to the offending location).
pub fn validate(instance: &serde_json::Value) -> anyhow::Result<()> {
    let mut errors: Vec<String> = validator()
        .iter_errors(instance)
        .map(|err| {
            let pointer = err.instance_path().as_str();
            let location = if pointer.is_empty() { "/" } else { pointer };
            format!("  at {location}: {err}")
        })
        .collect();

    // Per-path kind validation: for each path that opts into a kind we
    // recognize, apply that kind's schema on top of the base. Unknown
    // `kind` URIs are left alone (generic path).
    if let Some(paths) = instance.get("paths").and_then(|p| p.as_array()) {
        let kinds = kind_validators();
        for (i, path) in paths.iter().enumerate() {
            let Some(kind) = path.pointer("/meta/kind").and_then(|k| k.as_str()) else {
                continue;
            };
            let Some(kv) = kinds.get(kind) else {
                continue;
            };
            for err in kv.iter_errors(path) {
                errors.push(format!(
                    "  at /paths/{i}{}: {err} (kind {kind})",
                    err.instance_path()
                ));
            }
        }
    }

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

    const ACS_KIND: &str = "https://toolpath.net/kinds/agent-coding-session/v1.1.0";

    fn acs_graph(append: serde_json::Value) -> serde_json::Value {
        json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {"id": "p1", "head": "s1"},
                "meta": {"kind": ACS_KIND},
                "steps": [{
                    "step": {
                        "id": "s1",
                        "actor": "human:user",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"agent://claude-code/s1": {"structural": append}}
                }]
            }]
        })
    }

    #[test]
    fn agent_coding_session_kind_validates_when_well_formed() {
        let doc = acs_graph(json!({
            "type": "conversation.append",
            "role": "user",
            "text": "hi"
        }));
        validate(&doc).expect("a well-formed agent-coding-session path should pass base + kind");
    }

    #[test]
    fn agent_coding_session_kind_constraints_are_enforced() {
        // `conversation.append` requires `text`; the base schema alone
        // wouldn't catch this — only the kind schema does.
        let doc = acs_graph(json!({
            "type": "conversation.append",
            "role": "user"
        }));
        let err = validate(&doc).expect_err("missing `text` violates the kind");
        let msg = err.to_string();
        assert!(
            msg.contains("text"),
            "error should name the missing field: {msg}"
        );
        assert!(
            msg.contains(ACS_KIND),
            "error should attribute the kind: {msg}"
        );
    }

    #[test]
    fn unknown_kind_is_treated_as_generic() {
        // A path claiming an unrecognized kind URI is validated against
        // the base schema only — the would-be kind violation is ignored.
        let mut doc = acs_graph(json!({
            "type": "conversation.append",
            "role": "user"
        }));
        doc["paths"][0]["meta"]["kind"] = json!("https://toolpath.net/kinds/made-up/v9.9.9");
        validate(&doc).expect("an unknown kind imposes no extra constraints");
    }

    #[test]
    fn derived_claude_path_conforms_to_its_own_kind() {
        // End-to-end: derive a real Claude session, wrap it as a graph,
        // and validate. derive_path stamps meta.kind = agent-coding-session,
        // so this proves the derivation's output satisfies the kind it
        // claims, not just the base schema.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-fixtures/claude/convo.jsonl");
        let convo = toolpath_claude::ConversationReader::read_conversation(&fixture)
            .expect("read claude fixture");
        let path =
            toolpath_claude::derive::derive_path(&convo, &Default::default()).expect("derive");
        assert_eq!(
            path.meta.as_ref().and_then(|m| m.kind.as_deref()),
            Some(ACS_KIND),
            "derive_path must stamp the agent-coding-session kind"
        );
        let doc = json!({
            "graph": {"id": "g1"},
            "paths": [serde_json::to_value(&path).unwrap()],
        });
        validate(&doc).expect("derived claude path should satisfy base + its own kind");
    }
}
