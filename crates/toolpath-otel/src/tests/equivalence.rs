//! Cross-profile equivalence: each M0 conversation read by `openrouter` and
//! its semconv re-encoding read under `auto` derive Paths that agree on
//! exactly the comparison set in common/equivalence.rs. Also the retention round-trip over the
//! re-encoded fixtures and the span-content captures, and the oracle's own
//! Delta and skeleton paths over inline sessions.

use super::common::equivalence::{comparison_set, conversation};
use super::common::retention::{
    Retained, assert_retains, assert_session_retains, otel_extra, rebuild,
};
use super::common::{captures, deliveries};
use crate::tests::otel::{
    Absent, DeriveConfig, Generation, History, Message, ProfileSelection, ReadOutcome, Session,
    SkipReason, ToolCall, derive_path, group_sessions,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use toolpath::v1::Path;

/// Conversation fixtures: (M0 file in test-fixtures/otel/openrouter,
/// re-encoding in test-fixtures/otel/equivalence).
const PAIRS: [(&str, &str); 5] = [
    ("claude-code.ndjson", "claude-code.ndjson"),
    ("codex.ndjson", "codex.ndjson"),
    ("opencode.ndjson", "opencode.ndjson"),
    ("pi.ndjson", "pi.ndjson"),
    ("synthetic-fork.ndjson", "synthetic-fork.ndjson"),
];

fn openrouter() -> ProfileSelection {
    ProfileSelection::OpenRouter
}

fn reencoded(name: &str) -> Vec<Value> {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel/equivalence");
    std::fs::read_to_string(dir.join(name))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The crate's `read_deliveries` (not common's Auto-only wrapper).
fn read(values: &[Value], sel: ProfileSelection) -> ReadOutcome {
    crate::tests::otel::read_deliveries(values, sel).unwrap()
}

fn paths_by_key(gens: Vec<Generation>) -> BTreeMap<String, Path> {
    group_sessions(gens)
        .iter()
        .map(|s| (s.key.clone(), derive_path(s, &DeriveConfig::default())))
        .collect()
}

#[test]
fn conversation_keeps_thinking_only_on_producer_turns_and_empty_is_absent() {
    let turn = |otel: Value, thinking: &str| json!({"role": "assistant", "text": "t", "thinking": thinking, "otel": otel});
    let producer = json!({"generation_id": "g1"});
    assert_eq!(
        conversation(&turn(producer.clone(), "why"))["thinking"],
        "why",
        "a producer turn keeps its thinking"
    );
    assert_eq!(
        conversation(&turn(json!({}), "why"))["thinking"],
        Value::Null,
        "a non-producer turn drops thinking"
    );
    assert_eq!(
        conversation(&turn(producer, ""))["thinking"],
        Value::Null,
        "\"\" counts as absent"
    );
    let none = json!({"role": "assistant", "text": "t", "otel": {"generation_id": "g1"}});
    assert_eq!(conversation(&none)["thinking"], Value::Null);
}

#[test]
fn reencoded_fixtures_are_read_by_semconv_under_auto() {
    for (m0, re) in PAIRS {
        let want = read(&deliveries(m0), openrouter());
        let got = read(&reencoded(re), ProfileSelection::Auto);
        assert!(got.skipped.is_empty(), "{re}: {:?}", got.skipped);
        assert_eq!(got.unclaimed, 0, "{re}");
        assert!(
            want.generations.iter().all(|g| g.profile == "openrouter"),
            "{m0}"
        );
        assert!(
            got.generations.iter().all(|g| g.profile == "semconv"),
            "{re}"
        );
        let ids = |o: &ReadOutcome| {
            o.generations
                .iter()
                .map(|g| g.id.clone())
                .collect::<Vec<_>>()
        };
        assert!(!ids(&want).is_empty(), "{m0}");
        assert_eq!(ids(&got), ids(&want), "{re}: same generations, same order");
    }
}

#[test]
fn m0_and_reencoded_agree_on_the_comparison_set() {
    for (m0, re) in PAIRS {
        let left = paths_by_key(read(&deliveries(m0), openrouter()).generations);
        let right = paths_by_key(read(&reencoded(re), ProfileSelection::Auto).generations);
        assert!(!left.is_empty(), "{m0}");
        assert_eq!(
            left.keys().collect::<Vec<_>>(),
            right.keys().collect::<Vec<_>>(),
            "{m0}: session keys"
        );
        for (key, lp) in &left {
            let (l, r) = (comparison_set(lp), comparison_set(&right[key]));
            assert_eq!(l["head"], r["head"], "{m0} {key}: head");
            assert_eq!(l["dead_ends"], r["dead_ends"], "{m0} {key}: dead ends");
            let (ls, rs) = (
                l["steps"].as_array().unwrap(),
                r["steps"].as_array().unwrap(),
            );
            assert_eq!(ls.len(), rs.len(), "{m0} {key}: step count");
            // Step by step, so a failure names the first differing step.
            for (a, b) in ls.iter().zip(rs) {
                assert_eq!(a, b, "{m0} {key}: step {}", a["id"]);
            }
        }
    }
}

#[test]
fn the_error_span_is_skipped_under_both() {
    let left = read(&deliveries("codex-error-span.json"), openrouter());
    let right = read(
        &reencoded("codex-error-span.ndjson"),
        ProfileSelection::Auto,
    );
    for (name, out) in [("openrouter", &left), ("semconv", &right)] {
        assert!(out.generations.is_empty(), "{name}");
        assert_eq!(out.skipped.len(), 1, "{name}: {:?}", out.skipped);
        assert!(
            matches!(out.skipped[0].reason, SkipReason::ErrorStatus),
            "{name}: {:?}",
            out.skipped
        );
    }
    assert_eq!(
        left.skipped[0].generation_id,
        right.skipped[0].generation_id
    );
}

#[test]
fn retention_round_trip_over_reencoded_fixtures() {
    for (m0, re) in PAIRS {
        let r = assert_retains(&reencoded(re), ProfileSelection::Auto);
        let want = read(&deliveries(m0), openrouter()).generations.len();
        assert_eq!(
            r,
            Retained {
                generations: want,
                continued: 0,
                skeletons: 0
            },
            "{re}"
        );
    }
}

#[test]
fn retention_round_trip_over_span_content_captures() {
    for name in captures::SEMCONV {
        let values = captures::traces_in(&captures::semconv_dir(name));
        // The pinned Responses instrumentation omits the continuation
        // attribute; read the SYNTHETIC copy so Delta is exercised.
        let (values, continued) = if name == "openai-responses" {
            (captures::traces_in(&captures::continuation_dir()), 2)
        } else {
            (values, 0)
        };
        let r = assert_retains(&values, ProfileSelection::Auto);
        assert_eq!(
            r,
            Retained {
                generations: 3,
                continued,
                skeletons: 0
            },
            "{name}"
        );
    }
    let r = assert_retains(
        &captures::traces_in(&captures::openinference_dir()),
        ProfileSelection::OpenInference,
    );
    assert_eq!(
        r,
        Retained {
            generations: 3,
            continued: 0,
            skeletons: 0
        },
        "openinference"
    );
}

fn msg(v: Value) -> Message {
    serde_json::from_value(v).unwrap()
}

fn gen_(id: &str, start: u64, messages: Vec<Value>, text: &str) -> Generation {
    let mut g = Generation {
        id: id.into(),
        start_ns: start,
        end_ns: start + 1,
        profile: "semconv".into(),
        messages: messages.into_iter().map(msg).collect(),
        response_model: Some("m".into()),
        ..Default::default()
    };
    g.completion.text = text.into();
    g
}

fn delta(mut g: Generation, continues: &str) -> Generation {
    g.history = History::Delta;
    g.continues = Some(continues.into());
    g
}

/// The step id of `gid`'s producer (its completion node).
fn producer_step(p: &Path, gid: &str) -> String {
    p.steps
        .iter()
        .find(|s| otel_extra(p, s).is_some_and(|x| x["generation_id"] == gid))
        .unwrap_or_else(|| panic!("no producer step for {gid}"))
        .step
        .id
        .clone()
}

/// Delta rebuilt from the continued tip: g2 continues g1 (its input is the
/// resent instructions plus g1's tool result), g3 continues g2, g4 names a
/// target that is not in the session (chains from the root).
#[test]
fn retention_rebuilds_a_delta_from_its_continued_tip() {
    let sys = json!({"role": "system", "content": "I"});
    let mut g1 = gen_(
        "g1",
        1,
        vec![sys.clone(), json!({"role": "user", "content": "hi"})],
        "",
    );
    g1.completion.tool_calls = vec![
        serde_json::from_value::<ToolCall>(
            json!({"id": "c1", "function": {"name": "read", "arguments": "{\"p\":\"a\"}"}}),
        )
        .unwrap(),
    ];
    let g2 = delta(
        gen_(
            "g2",
            2,
            vec![
                sys.clone(),
                json!({"role": "tool", "tool_call_id": "c1", "content": "out"}),
            ],
            "done",
        ),
        "g1",
    );
    let g3 = delta(
        gen_(
            "g3",
            3,
            vec![sys, json!({"role": "user", "content": "more"})],
            "bye",
        ),
        "g2",
    );
    let g4 = delta(
        gen_(
            "g4",
            4,
            vec![json!({"role": "user", "content": "again"})],
            "ok",
        ),
        "gone",
    );
    let s = Session::new("s".into(), None, vec![g1, g2, g3, g4]);
    let r = assert_session_retains(&s);
    assert_eq!(
        r,
        Retained {
            generations: 4,
            continued: 2,
            skeletons: 0
        }
    );

    let p = derive_path(&s, &DeriveConfig::default());
    // g2: the resent system message goes back at its raw index 0, ahead of
    // g1's tool result, and the chain starts at g1's completion step.
    let g2 = rebuild(&p, "g2");
    assert_eq!(g2.base_tip, Some(producer_step(&p, "g1")));
    let roles: Vec<&str> = g2
        .prompt
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "tool"]);
    assert_eq!(rebuild(&p, "g3").base_tip, Some(producer_step(&p, "g2")));
    // A missing target is never an ancestor: the walk runs to the root.
    assert_eq!(rebuild(&p, "g4").base_tip, None);
}

/// A skeleton (prompt absent) is skipped and counted; the Delta that
/// continues from it still rebuilds from the skeleton's completion.
#[test]
fn retention_skips_a_skeleton_and_rebuilds_past_it() {
    let g1 = gen_("g1", 1, vec![json!({"role": "user", "content": "hi"})], "a");
    let mut g2 = gen_("g2", 2, vec![], "b");
    g2.absent = Absent {
        prompt: true,
        completion: false,
    };
    g2.continues = Some("g1".into());
    let g3 = delta(
        gen_(
            "g3",
            3,
            vec![json!({"role": "user", "content": "more"})],
            "c",
        ),
        "g2",
    );
    let s = Session::new("s".into(), None, vec![g1, g2, g3]);
    let r = assert_session_retains(&s);
    assert_eq!(
        r,
        Retained {
            generations: 2,
            continued: 1,
            skeletons: 1
        }
    );
    let p = derive_path(&s, &DeriveConfig::default());
    assert_eq!(rebuild(&p, "g3").base_tip, Some(producer_step(&p, "g2")));
}
