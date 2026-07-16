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

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use toolpath_codex::provider::to_view;
use toolpath_codex::{ResponseItem, RolloutItem, RolloutReader, derive};
use toolpath_convo::{ConversationView, DeriveConfig, Role, derive_path, extract_conversation};

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

/// Ground-truth invariant for #124 (relativized file-change keys) run
/// against a real recorded session: derive `view`, then for every
/// pre-derive `FileMutation::path` (Codex populates these from
/// `patch_apply_end` events) assert (a) it produced a relativized key
/// iff it actually sat under `path.base` on a path-component boundary --
/// no absolute-under-base leak, and no wrongly-relativized outside-base
/// key -- and (b) extracting and re-deriving reproduces the identical
/// `file.write` key set (idempotency).
///
/// "Under base" is independently recomputed here via `std::path::Path`
/// component stripping rather than by calling `toolpath_convo`'s own
/// (private) `relativize_key`, so this exercises its output rather than
/// re-asserting its internals.
fn assert_file_write_keys_match_base(view: &ConversationView) {
    let path = derive_path(view, &DeriveConfig::default());
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|s| s.trim_end_matches('/').to_string());

    let ground_truth: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|t| t.file_mutations.iter().map(|fm| fm.path.as_str()))
        .collect();
    assert!(
        !ground_truth.is_empty(),
        "fixture must exercise at least one file mutation for this test to be meaningful"
    );

    let file_write_keys = |p: &toolpath::v1::Path| -> BTreeSet<String> {
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
    let derived_keys = file_write_keys(&path);

    // Track that the under-base branch -- the exact case #124 relativizes --
    // actually fires at least once, so a fixture (or a regression) with no
    // absolute-under-base path can't let this test silently no-op the
    // invariant it exists to guard.
    let mut saw_under_base = false;
    for gt in &ground_truth {
        let under_base = base_root.as_deref().is_some_and(|root| {
            std::path::Path::new(gt)
                .strip_prefix(root)
                .is_ok_and(|rest| rest != std::path::Path::new(""))
        });
        if under_base {
            saw_under_base = true;
            let root = base_root.as_deref().unwrap();
            let expected_relative = gt.strip_prefix(root).unwrap().trim_start_matches('/');
            assert!(
                derived_keys.contains(expected_relative),
                "expected relativized key {expected_relative:?} for {gt:?} under base {root:?}, got {derived_keys:?}"
            );
            assert!(
                !derived_keys.contains(*gt),
                "absolute-under-base leak: {gt:?} should have been relativized but the absolute form is still a key"
            );
        } else {
            assert!(
                derived_keys.contains(*gt),
                "expected {gt:?} to remain an absolute (or opaque) key outside the base, got {derived_keys:?}"
            );
        }
    }
    assert!(
        saw_under_base,
        "test must exercise at least one absolute-under-base key -- the invariant #124 changed"
    );

    let view2 = extract_conversation(&path);
    let path2 = derive_path(&view2, &DeriveConfig::default());
    assert_eq!(
        derived_keys,
        file_write_keys(&path2),
        "re-derive must reproduce the identical file.write key set"
    );
}

/// Extends `patch_apply_files_all_surface_as_artifacts` with the two
/// invariants that test doesn't cover: no absolute-under-base leak, and
/// extract -> re-derive idempotency of the `file.write` key set. Codex's
/// real fixture records absolute patch paths under the session's recorded
/// cwd, so the under-base branch fires on the real data with no synthetic
/// injection needed.
#[test]
fn patch_apply_file_write_keys_no_leak_and_stable_on_re_derive() {
    let view = to_view(&session());
    assert_file_write_keys_match_base(&view);
}
