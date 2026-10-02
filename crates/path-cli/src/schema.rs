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
//!
//! agent-coding-session v1.1.0 and later also carry accounting rules that
//! JSON Schema cannot express (one group total per run, breakdowns bounded
//! by their parent); [`validate`] checks those in code.

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
            if has_accounting_rules(kind) {
                for (pointer, msg) in accounting_violations(path) {
                    errors.push(format!("  at /paths/{i}{pointer}: {msg} (kind {kind})"));
                }
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

/// agent-coding-session v1.1.0 and later carry the group accounting rules,
/// which are prose in the spec because JSON Schema cannot express them.
fn has_accounting_rules(kind: &str) -> bool {
    crate::kinds::parse_kind_uri(kind)
        .is_some_and(|(name, v)| name == "agent-coding-session" && v.major == 1 && v.minor >= 1)
}

/// The `conversation.append` payloads of a step, with the JSON pointer of each.
fn appends(step: &serde_json::Value) -> Vec<(String, &serde_json::Value)> {
    let Some(change) = step.get("change").and_then(|c| c.as_object()) else {
        return Vec::new();
    };
    change
        .iter()
        .filter_map(|(key, c)| {
            let structural = c.get("structural")?;
            (structural.get("type")?.as_str()? == "conversation.append").then(|| {
                let key = key.replace('~', "~0").replace('/', "~1");
                (format!("/change/{key}/structural"), structural)
            })
        })
        .collect()
}

/// Breaches of the agent-coding-session accounting rules, as
/// `(pointer within the path, message)`:
/// - within a run of consecutive steps sharing a `group_id`, only the last
///   step carries `token_usage`;
/// - a breakdown's sub-classes sum to no more than their parent class, on
///   `token_usage` and `attributed_token_usage` alike.
fn accounting_violations(path: &serde_json::Value) -> Vec<(String, String)> {
    let Some(steps) = path.get("steps").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let per_step: Vec<Vec<(String, &serde_json::Value)>> = steps.iter().map(appends).collect();
    let group_of = |j: usize| {
        per_step[j]
            .iter()
            .find_map(|(_, a)| a.get("group_id").and_then(|g| g.as_str()))
    };

    let mut out = Vec::new();
    for (j, payloads) in per_step.iter().enumerate() {
        if let Some(group) = group_of(j)
            && j + 1 < steps.len()
            && group_of(j + 1) == Some(group)
        {
            for (pointer, append) in payloads {
                if append.get("token_usage").is_some() {
                    out.push((
                        format!("/steps/{j}{pointer}/token_usage"),
                        format!(
                            "token_usage on a step that is not the last of its group_id {group:?} \
                             run; the group total belongs on the run's last step only"
                        ),
                    ));
                }
            }
        }
        for (pointer, append) in payloads {
            for key in ["token_usage", "attributed_token_usage"] {
                if let Some(usage) = append.get(key) {
                    out.extend(breakdown_violations(usage).into_iter().map(|(class, msg)| {
                        (format!("/steps/{j}{pointer}/{key}/breakdowns/{class}"), msg)
                    }));
                }
            }
        }
    }
    out
}

fn breakdown_violations(usage: &serde_json::Value) -> Vec<(String, String)> {
    let Some(breakdowns) = usage.get("breakdowns").and_then(|b| b.as_object()) else {
        return Vec::new();
    };
    breakdowns
        .iter()
        .filter_map(|(class, inner)| {
            let parent = usage.get(format!("{class}_tokens"))?.as_u64()?;
            let sum: u64 = inner.as_object()?.values().filter_map(|n| n.as_u64()).sum();
            (sum > parent).then(|| {
                (
                    class.replace('~', "~0").replace('/', "~1"),
                    format!(
                        "breakdown sums to {sum}, more than its parent {class}_tokens ({parent})"
                    ),
                )
            })
        })
        .collect()
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

    const ACS_KIND: &str = toolpath::v1::PATH_KIND_AGENT_CODING_SESSION;

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

    /// An agent-coding-session graph with one step per payload, in order.
    fn acs_steps(kind: &str, appends: &[serde_json::Value]) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = appends
            .iter()
            .enumerate()
            .map(|(i, append)| {
                json!({
                    "step": {
                        "id": format!("s{i}"),
                        "actor": "agent:claude-code",
                        "timestamp": "2026-01-29T10:00:00Z"
                    },
                    "change": {"agent://claude-code/s1": {"structural": append}}
                })
            })
            .collect();
        json!({
            "graph": {"id": "g1"},
            "paths": [{
                "path": {"id": "p1", "head": format!("s{}", appends.len() - 1)},
                "meta": {"kind": kind},
                "steps": steps
            }]
        })
    }

    fn assistant(group: Option<&str>, usage: Option<serde_json::Value>) -> serde_json::Value {
        let mut v = json!({"type": "conversation.append", "role": "assistant", "text": "ok"});
        if let Some(g) = group {
            v["group_id"] = json!(g);
        }
        if let Some(u) = usage {
            v["token_usage"] = u;
        }
        v
    }

    fn usage(input: u64, output: u64) -> serde_json::Value {
        json!({"input_tokens": input, "output_tokens": output})
    }

    #[test]
    fn group_total_on_last_step_of_run_is_valid() {
        let doc = acs_steps(
            ACS_KIND,
            &[
                assistant(Some("msg_1"), None),
                assistant(Some("msg_1"), Some(usage(10, 5))),
                assistant(None, Some(usage(3, 1))),
            ],
        );
        validate(&doc).expect("one total per group, on the run's last step");
    }

    #[test]
    fn group_total_on_a_non_final_step_is_rejected() {
        let doc = acs_steps(
            ACS_KIND,
            &[
                assistant(Some("msg_1"), Some(usage(10, 5))),
                assistant(Some("msg_1"), Some(usage(10, 5))),
            ],
        );
        let msg = validate(&doc)
            .expect_err("a repeated group total double-counts the group")
            .to_string();
        assert!(msg.contains("/paths/0/steps/0"), "names the step: {msg}");
        assert!(msg.contains("msg_1"), "names the group: {msg}");
        assert!(msg.contains(ACS_KIND), "attributes the kind: {msg}");
        assert!(
            !msg.contains("/paths/0/steps/1"),
            "the last step is fine: {msg}"
        );
    }

    #[test]
    fn a_group_split_by_another_step_is_two_runs() {
        let doc = acs_steps(
            ACS_KIND,
            &[
                assistant(Some("msg_1"), Some(usage(10, 5))),
                assistant(None, None),
                assistant(Some("msg_1"), Some(usage(4, 2))),
            ],
        );
        validate(&doc).expect("each run of a group carries its own total");
    }

    #[test]
    fn breakdown_within_its_parent_is_valid() {
        let mut u = usage(10, 500);
        u["breakdowns"] = json!({"output": {"reasoning": 450, "text": 50}});
        validate(&acs_steps(ACS_KIND, &[assistant(None, Some(u))]))
            .expect("Σ(inner) may equal the parent");
    }

    #[test]
    fn breakdown_above_its_parent_is_rejected() {
        let mut u = usage(10, 400);
        u["breakdowns"] = json!({"output": {"reasoning": 450}});
        let msg = validate(&acs_steps(ACS_KIND, &[assistant(None, Some(u))]))
            .expect_err("a breakdown cannot exceed its parent class")
            .to_string();
        assert!(
            msg.contains("token_usage/breakdowns/output"),
            "points at it: {msg}"
        );
        assert!(
            msg.contains("450") && msg.contains("400"),
            "gives both sums: {msg}"
        );
    }

    #[test]
    fn attributed_breakdown_above_its_parent_is_rejected() {
        let mut a = assistant(None, Some(usage(10, 400)));
        a["attributed_token_usage"] = json!({"input_tokens": 10, "output_tokens": 100, "breakdowns": {"output": {"reasoning": 101}}});
        let msg = validate(&acs_steps(ACS_KIND, &[a]))
            .expect_err("the bound applies on attributed_token_usage too")
            .to_string();
        assert!(
            msg.contains("attributed_token_usage/breakdowns/output"),
            "points at it: {msg}"
        );
    }

    #[test]
    fn negative_counts_are_rejected_from_v1_2_0() {
        let doc = acs_steps(
            ACS_KIND,
            &[assistant(
                None,
                Some(json!({"input_tokens": -3, "output_tokens": 1})),
            )],
        );
        let msg = validate(&doc)
            .expect_err("counts are non-negative")
            .to_string();
        assert!(msg.contains("input_tokens"), "names the field: {msg}");
    }

    #[test]
    fn accounting_rules_apply_to_v1_1_0() {
        let doc = acs_steps(
            toolpath::v1::PATH_KIND_AGENT_CODING_SESSION_V1_1_0,
            &[
                assistant(Some("msg_1"), Some(usage(10, 5))),
                assistant(Some("msg_1"), Some(usage(10, 5))),
            ],
        );
        validate(&doc).expect_err("v1.1.0 introduced the once-per-group rule");
    }

    #[test]
    fn accounting_rules_do_not_apply_to_v1_0_0() {
        let doc = acs_steps(
            toolpath::v1::PATH_KIND_AGENT_CODING_SESSION_V1_0_0,
            &[
                assistant(Some("msg_1"), Some(usage(10, 5))),
                assistant(Some("msg_1"), Some(usage(10, 5))),
            ],
        );
        validate(&doc).expect("v1.0.0 left accounting unspecified");
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
        let path = toolpath_claude::derive::derive_path(&convo, &Default::default());
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
