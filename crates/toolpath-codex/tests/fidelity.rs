//! Source→derivation fidelity invariants.
//!
//! These tests walk the real recorded Codex fixture and assert that
//! facts in the source rollout (timestamps, actor roles, tool call_ids,
//! raw arguments, patched file paths, parent ordering) survive the
//! `Session → ConversationView → Path` pipeline unchanged.
//!
//! They exist to catch silent data-loss bugs. The motivating case:
//! `message_to_turn` originally hardcoded `timestamp: String::new()`,
//! so every derived step shipped with an empty timestamp even though
//! the source carried a real one. The old tests only asserted counts
//! and totals, so the drop went undetected.

use std::collections::HashSet;
use std::path::PathBuf;

use toolpath_codex::provider::to_view;
use toolpath_codex::{ResponseItem, RolloutItem, RolloutReader, derive};
use toolpath_convo::{DeriveConfig, Role, extract_conversation};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-codex-python.jsonl")
}

fn session() -> toolpath_codex::Session {
    RolloutReader::read_session(fixture_path()).unwrap()
}

fn derived() -> toolpath::v1::Path {
    derive::derive_path(&session(), &derive::DeriveConfig::default())
}

// ── Step-level invariants ──────────────────────────────────────────

#[test]
fn all_steps_have_non_empty_timestamp() {
    // The regression that motivated this suite: `message_to_turn`
    // was dropping the line timestamp, so every derived step had
    // `timestamp: ""`. Every step in a real session must carry a
    // non-empty ISO-8601 timestamp — including synthetic-carrier steps,
    // which are built from a source line and inherit its timestamp.
    let path = derived();
    for s in &path.steps {
        assert!(
            !s.step.timestamp.is_empty(),
            "step {} has empty timestamp",
            s.step.id
        );
    }
}

#[test]
fn step_timestamps_match_source_message_lines() {
    // For every `response_item.message` line in the source rollout,
    // at least one derived step must carry its exact timestamp.
    // This proves the line→turn→step pipeline doesn't silently
    // re-clock or zero out timestamps anywhere.
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());

    let step_timestamps: HashSet<&str> = path
        .steps
        .iter()
        .map(|st| st.step.timestamp.as_str())
        .collect();

    let mut missing: Vec<String> = Vec::new();
    for line in &s.lines {
        if let RolloutItem::ResponseItem(ResponseItem::Message(_)) = line.item()
            && !step_timestamps.contains(line.timestamp.as_str())
        {
            missing.push(line.timestamp.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "source message timestamps missing from derived path: {:?}",
        missing
    );
}

#[test]
fn turn_timestamps_match_source_message_lines() {
    // Same fidelity check, but at the ConversationView layer. If turns
    // lose the timestamp, `build_step` will too — isolating the check
    // here makes regressions attributable to the provider layer vs
    // the derive layer.
    let s = session();
    let view = to_view(&s);

    let turn_timestamps: HashSet<&str> = view.turns.iter().map(|t| t.timestamp.as_str()).collect();

    for line in &s.lines {
        if let RolloutItem::ResponseItem(ResponseItem::Message(_)) = line.item() {
            assert!(
                turn_timestamps.contains(line.timestamp.as_str()),
                "source message line {} has no matching Turn",
                line.timestamp
            );
        }
    }
}

// ── Parent chain invariants ────────────────────────────────────────

#[test]
fn parent_chain_is_linear_and_in_order() {
    // Codex derivation produces a linear DAG: each step has at most
    // one parent, and the parent is always a step that appeared
    // earlier in the list. No cycles, no forward references.
    let path = derived();
    let positions: std::collections::HashMap<&str, usize> = path
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.as_str(), i))
        .collect();

    for (i, step) in path.steps.iter().enumerate() {
        assert!(
            step.step.parents.len() <= 1,
            "step {} has {} parents — codex is expected to be linear",
            step.step.id,
            step.step.parents.len()
        );
        for parent in &step.step.parents {
            let pi = positions.get(parent.as_str()).unwrap_or_else(|| {
                panic!("step {} references missing parent {}", step.step.id, parent)
            });
            assert!(
                *pi < i,
                "step {} (index {}) references parent {} (index {}) — out of order",
                step.step.id,
                i,
                parent,
                pi
            );
        }
    }
}

#[test]
fn head_equals_last_step_id() {
    let path = derived();
    let last = path.steps.last().expect("path has steps");
    assert_eq!(path.path.head, last.step.id);
}

// ── Actor invariants ───────────────────────────────────────────────

#[test]
fn actor_scheme_matches_source_role() {
    // Source role → actor-prefix mapping must be consistent:
    //   "user"                  → "human:*"
    //   "assistant"             → "agent:*"
    //   "developer" | "system"  → "tool:*"
    // We can't assert a strict 1:1 turn→step mapping (carrier turns
    // may collapse), but we can assert every observed role in the
    // view reaches a step with the expected actor prefix.
    let s = session();
    let view = to_view(&s);
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());

    let user_seen = view.turns.iter().any(|t| t.role == Role::User);
    let assistant_seen = view.turns.iter().any(|t| t.role == Role::Assistant);
    let system_seen = view.turns.iter().any(|t| t.role == Role::System);

    let prefixes: HashSet<&str> = path
        .steps
        .iter()
        .map(|s| s.step.actor.split(':').next().unwrap_or(""))
        .collect();

    if user_seen {
        assert!(prefixes.contains("human"), "no step has a human:* actor");
    }
    if assistant_seen {
        assert!(prefixes.contains("agent"), "no step has an agent:* actor");
    }
    if system_seen {
        assert!(prefixes.contains("tool"), "no step has a tool:* actor");
    }
}

// ── Tool-call fidelity ─────────────────────────────────────────────

fn collect_derived_tool_call_ids(path: &toolpath::v1::Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    for step in &path.steps {
        for change in step.change.values() {
            let Some(struc) = change.structural.as_ref() else {
                continue;
            };
            // Canonical: `tool_uses` array entries carry `id` (= call_id).
            let Some(uses) = struc.extra.get("tool_uses") else {
                continue;
            };
            let Some(arr) = uses.as_array() else {
                continue;
            };
            for tu in arr {
                if let Some(id) = tu.get("id").and_then(|v| v.as_str()) {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids
}

#[test]
fn every_function_call_call_id_surfaces_in_steps() {
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());
    let derived_ids = collect_derived_tool_call_ids(&path);

    for line in &s.lines {
        match line.item() {
            RolloutItem::ResponseItem(ResponseItem::FunctionCall(fc)) => {
                assert!(
                    derived_ids.contains(&fc.call_id),
                    "function_call {} missing from derived path",
                    fc.call_id
                );
            }
            RolloutItem::ResponseItem(ResponseItem::CustomToolCall(ct)) => {
                assert!(
                    derived_ids.contains(&ct.call_id),
                    "custom_tool_call {} missing from derived path",
                    ct.call_id
                );
            }
            _ => {}
        }
    }
}

#[test]
fn function_call_arguments_preserved_in_view() {
    // Raw `arguments` strings are intentionally kept verbatim on the
    // ToolInvocation via `extra["raw_arguments"]`, so that downstream
    // consumers can reconstruct the exact byte sequence the model
    // emitted — even when the JSON is malformed or contains trailing
    // whitespace the parser would strip.
    let s = session();
    let view = to_view(&s);

    let mut tool_by_id: std::collections::HashMap<&str, &toolpath_convo::ToolInvocation> =
        std::collections::HashMap::new();
    for t in &view.turns {
        for tu in &t.tool_uses {
            tool_by_id.insert(tu.id.as_str(), tu);
        }
    }

    for line in &s.lines {
        if let RolloutItem::ResponseItem(ResponseItem::FunctionCall(fc)) = line.item() {
            let tu = tool_by_id
                .get(fc.call_id.as_str())
                .unwrap_or_else(|| panic!("function_call {} missing from view", fc.call_id));
            assert_eq!(
                tu.name, fc.name,
                "tool invocation {} has wrong name",
                fc.call_id
            );
            // The raw_arguments string should either be present in
            // extra, or the input should parse to the same JSON as the
            // source arguments (we accept either; what we're ruling
            // out is the arguments being dropped entirely).
            let raw_match = tu
                .input
                .get("raw_arguments")
                .and_then(|v| v.as_str())
                .map(|s| s == fc.arguments)
                .unwrap_or(false);
            let parsed_match = serde_json::from_str::<serde_json::Value>(&fc.arguments)
                .ok()
                .map(|v| v == tu.input)
                .unwrap_or(false);
            let raw_eq_input = tu
                .input
                .as_str()
                .map(|s| s == fc.arguments)
                .unwrap_or(false);
            assert!(
                raw_match || parsed_match || raw_eq_input,
                "function_call {} arguments not preserved: source={:?}, got input={:?}",
                fc.call_id,
                fc.arguments,
                tu.input
            );
        }
    }
}

// ── Patch-apply file artifact fidelity ─────────────────────────────

#[test]
fn patch_apply_files_all_surface_as_artifacts() {
    // Every file path listed under a successful `patch_apply_end`
    // event must appear as an artifact key on some derived step.
    // This catches any bug where we drop files because of a change
    // variant we didn't recognize.
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());

    let artifact_keys: HashSet<&str> = path
        .steps
        .iter()
        .flat_map(|s| s.change.keys().map(|k| k.as_str()))
        .collect();

    // Change keys are relativized against `path.base` (RFC: bare keys are
    // base-relative), so relativize each source path the same way before
    // asserting membership.
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|r| r.trim_end_matches('/').to_string());
    let relativize = |p: &str| -> String {
        match &base_root {
            Some(root) if !root.is_empty() && p.starts_with('/') => match p.strip_prefix(root) {
                Some(rest) if rest.starts_with('/') => rest[1..].to_string(),
                _ => p.to_string(),
            },
            _ => p.to_string(),
        }
    };

    for line in &s.lines {
        if let RolloutItem::EventMsg(toolpath_codex::EventMsg::PatchApplyEnd(patch)) = line.item() {
            if !patch.success {
                continue;
            }
            for file_path in patch.changes.keys() {
                let expected = relativize(file_path);
                assert!(
                    artifact_keys.contains(expected.as_str()),
                    "file {} (key {}) from successful patch_apply_end not found in derived artifacts",
                    file_path,
                    expected
                );
            }
        }
    }
}

/// Extends `patch_apply_files_all_surface_as_artifacts` with the two
/// invariants that test doesn't cover: (a) a path that DID get
/// relativized must not ALSO leave its absolute form as a key
/// (no absolute-under-base leak), and (b) extracting the derived path
/// and re-deriving from it reproduces the identical `file.write` key
/// set (idempotency).
#[test]
fn patch_apply_file_write_keys_no_leak_and_stable_on_re_derive() {
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());

    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|r| r.trim_end_matches('/').to_string());

    let file_write_keys = |p: &toolpath::v1::Path| -> HashSet<String> {
        p.steps
            .iter()
            .flat_map(|s| s.change.iter())
            .filter(|(_, ch)| {
                ch.structural
                    .as_ref()
                    .is_some_and(|sc| sc.change_type == "file.write")
            })
            .map(|(k, _)| k.clone())
            .collect()
    };
    let keys1 = file_write_keys(&path);

    let mut checked_any = false;
    for line in &s.lines {
        if let RolloutItem::EventMsg(toolpath_codex::EventMsg::PatchApplyEnd(patch)) = line.item() {
            if !patch.success {
                continue;
            }
            for file_path in patch.changes.keys() {
                checked_any = true;
                let under_base = base_root.as_deref().is_some_and(|root| {
                    std::path::Path::new(file_path)
                        .strip_prefix(root)
                        .is_ok_and(|rest| rest != std::path::Path::new(""))
                });
                if under_base {
                    assert!(
                        !keys1.contains(file_path.as_str()),
                        "absolute-under-base leak: {file_path:?} should have been relativized but its absolute form is still a key"
                    );
                } else {
                    assert!(
                        keys1.contains(file_path.as_str()),
                        "expected {file_path:?} to remain an absolute key outside the base"
                    );
                }
            }
        }
    }
    assert!(
        checked_any,
        "fixture must exercise at least one patch_apply_end file for this test to be meaningful"
    );

    let view2 = extract_conversation(&path);
    let path2 = toolpath_convo::derive_path(&view2, &DeriveConfig::default());
    assert_eq!(
        keys1,
        file_write_keys(&path2),
        "re-derive must reproduce the identical file.write key set"
    );
}
