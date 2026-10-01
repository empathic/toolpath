//! Session grouping: generations by layer, and request bodies cut per session.

use crate::generation::{Completion, Generation, History, Message};
use crate::group::{SessionRequests, derive_session, group_generations, group_sessions};
use crate::normalize::kept_prompt;
use crate::session::{Session, cluster_key, trace_key};
use crate::{DeriveConfig, OtelError, ProfileSelection, derive_path, walk};
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path as FsPath, PathBuf};
use toolpath::v1::Path;

fn generation(id: &str, start: u64, session: Option<&str>, msgs: &[(&str, &str)]) -> Generation {
    Generation {
        id: id.into(),
        trace_id: String::new(),
        start_ns: start,
        end_ns: start + 1,
        session_id: session.map(str::to_string),
        client_key: Some("k".into()),
        messages: msgs
            .iter()
            .map(|(r, t)| Message {
                role: r.to_string(),
                content: json!(t),
                ..Default::default()
            })
            .collect(),
        completion: Completion::default(),
        ..Default::default()
    }
}

fn ids(s: &Session) -> Vec<&str> {
    s.generations.iter().map(|g| g.id.as_str()).collect()
}

#[test]
fn session_id_groups_and_sorts_by_start() {
    let s = group_generations(vec![
        generation("b", 2, Some("s1"), &[("user", "x")]),
        generation("a", 1, Some("s1"), &[("user", "x")]),
        generation("c", 3, Some("s2"), &[("user", "y")]),
    ]);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].key, "s1");
    assert_eq!(ids(&s[0]), ["a", "b"]);
}

#[test]
fn sessionless_prefix_extensions_cluster_together() {
    let s = group_generations(vec![
        generation("1", 1, None, &[("system", "S"), ("user", "task")]),
        generation(
            "2",
            2,
            None,
            &[
                ("system", "S"),
                ("user", "task"),
                ("assistant", "a"),
                ("user", "more"),
            ],
        ),
    ]);
    assert_eq!(s.len(), 1);
    assert!(s[0].key.starts_with("otel-cluster:"));
    assert_eq!(s[0].session_id, None);
}

#[test]
fn unrelated_sessionless_conversations_do_not_merge() {
    let s = group_generations(vec![
        generation("1", 1, None, &[("system", "S"), ("user", "one")]),
        generation("2", 2, None, &[("system", "S"), ("user", "two")]),
    ]);
    assert_eq!(s.len(), 2);
    assert_ne!(s[0].key, s[1].key);
}

#[test]
fn the_same_task_run_twice_gets_two_sessions_with_distinct_keys() {
    let run = [("system", "S"), ("user", "task")];
    let s = group_generations(vec![
        generation("1", 1, None, &run),
        generation(
            "2",
            2,
            None,
            &[run[0], run[1], ("assistant", "a"), ("user", "go")],
        ),
        generation("3", 3, None, &run),
        generation(
            "4",
            4,
            None,
            &[run[0], run[1], ("assistant", "b"), ("user", "go")],
        ),
    ]);
    assert_eq!(s.len(), 2);
    assert_ne!(s[0].key, s[1].key);
    for session in &s {
        let alone = Session::from_generations(session.generations.clone()).unwrap();
        assert_eq!(session.key, alone.key, "no batch suffix");
    }
}

#[test]
fn a_session_id_equal_to_a_cluster_key_stays_a_separate_session() {
    let cluster = generation("1", 1, None, &[("system", "S"), ("user", "task")]);
    let k = cluster_key(
        cluster.client_key.as_deref(),
        &kept_prompt(&cluster.messages),
        &cluster.id,
    );
    let s = group_generations(vec![
        cluster,
        generation("2", 2, Some(&k), &[("user", "other")]),
    ]);
    assert_eq!(s.len(), 2);
    let with_id = s.iter().find(|s| s.session_id.is_some()).unwrap();
    let clustered = s.iter().find(|s| s.session_id.is_none()).unwrap();
    assert_eq!(with_id.key, k);
    assert_eq!(with_id.generations.len(), 1);
    assert_eq!(clustered.generations.len(), 1);
    assert_ne!(clustered.key, with_id.key);
}

#[test]
fn a_cluster_key_skips_a_client_session_id_it_would_equal() {
    let cluster = generation("2", 2, None, &[("system", "S"), ("user", "task")]);
    let k = cluster_key(
        cluster.client_key.as_deref(),
        &kept_prompt(&cluster.messages),
        &cluster.id,
    );
    let s = group_generations(vec![
        generation("1", 1, Some(&k), &[("user", "other")]),
        cluster,
    ]);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].key, k);
    assert_eq!(s[1].key, format!("{k}-2"));
}

#[test]
fn a_generation_joins_the_cluster_with_the_longest_matching_tip() {
    let base = [("system", "S"), ("user", "t")];
    let s = group_generations(vec![
        generation("1", 1, None, &base),
        generation(
            "2",
            2,
            None,
            &[base[0], base[1], ("assistant", "a"), ("user", "go")],
        ),
        // Not an extension of the first cluster's tip: a second cluster.
        generation("3", 3, None, &base),
        generation(
            "4",
            4,
            None,
            &[
                base[0],
                base[1],
                ("assistant", "a"),
                ("user", "go"),
                ("assistant", "x"),
                ("user", "y"),
            ],
        ),
    ]);
    assert_eq!(s.len(), 2);
    assert_eq!(ids(&s[0]), ["1", "2", "4"]);
    assert_eq!(ids(&s[1]), ["3"]);
}

#[test]
fn identical_sessionless_prompts_under_different_client_keys_do_not_merge() {
    let mut a = generation("1", 1, None, &[("system", "S"), ("user", "task")]);
    let mut b = generation("2", 2, None, &[("system", "S"), ("user", "task")]);
    a.client_key = Some("k1".into());
    b.client_key = Some("k2".into());
    let s = group_generations(vec![a, b]);
    assert_eq!(s.len(), 2);
    assert_ne!(s[0].key, s[1].key);
}

#[test]
fn a_retry_with_the_same_prompt_joins_the_cluster() {
    let s = group_generations(vec![
        generation("1", 1, None, &[("system", "S"), ("user", "task")]),
        generation("2", 2, None, &[("system", "S"), ("user", "task")]),
    ]);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].generations.len(), 2);
}

#[test]
fn keys_match_from_generations_without_a_collision() {
    let gens = vec![
        generation("b", 2, None, &[("user", "x")]),
        generation("a", 1, None, &[("user", "x")]),
    ];
    let grouped = group_generations(gens.clone()).remove(0);
    assert_eq!(grouped, Session::from_generations(gens).unwrap());
}

fn g(id: &str, start: u64) -> Generation {
    Generation {
        id: id.into(),
        trace_id: format!("trace-{id}"),
        start_ns: start,
        end_ns: start + 1,
        client_key: Some("k".into()),
        ..Default::default()
    }
}

fn user(text: &str) -> Message {
    Message {
        role: "user".into(),
        content: json!(text),
        ..Default::default()
    }
}

#[test]
fn layer_c_joins_the_continued_generations_session() {
    let mut first = g("g1", 1);
    first.messages = vec![user("a")].into();
    let mut next = g("g2", 2);
    next.history = History::Delta;
    next.continues = Some("g1".into());
    next.messages = vec![user("b")].into();
    let s = group_generations(vec![next, first]);
    assert_eq!(s.len(), 1);
    assert!(s[0].key.starts_with("otel-cluster:"), "{}", s[0].key);
    assert_eq!(ids(&s[0]), ["g1", "g2"]);
}

#[test]
fn layer_c_precedes_layer_two_for_full_generations() {
    let mut first = g("g1", 1);
    first.messages = vec![user("a")].into();
    let mut next = g("g2", 2);
    next.continues = Some("g1".into());
    next.messages = vec![user("unrelated")].into();
    let s = group_generations(vec![first, next]);
    assert_eq!(s.len(), 1);
    assert_eq!(ids(&s[0]), ["g1", "g2"]);
}

#[test]
fn layer_c_needs_an_already_grouped_target() {
    let mut early = g("g1", 1);
    early.history = History::Delta;
    early.continues = Some("g2".into());
    let mut later = g("g2", 2);
    later.messages = vec![user("x")].into();
    let s = group_generations(vec![early, later]);
    assert_eq!(s.len(), 2);
    assert_eq!(
        s[0].key,
        trace_key(Some("k"), "trace-g1"),
        "falls to Layer T"
    );
}

#[test]
fn layer_t_joins_deltas_of_one_trace_and_splits_traces() {
    let mut a = g("a", 1);
    a.absent.prompt = true;
    a.trace_id = "t-1".into();
    let mut b = g("b", 2);
    b.history = History::Delta;
    b.trace_id = "t-1".into();
    let mut c = g("c", 3);
    c.absent.prompt = true;
    c.trace_id = "t-2".into();
    let s = group_generations(vec![a, b, c]);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].key, "otel-trace:8462f8d801d1bffd");
    assert_eq!(ids(&s[0]), ["a", "b"]);
    assert_eq!(s[1].key, trace_key(Some("k"), "t-2"));
}

#[test]
fn session_id_wins_over_continues() {
    let first = g("g1", 1);
    let mut next = g("g2", 2);
    next.history = History::Delta;
    next.continues = Some("g1".into());
    next.session_id = Some("sess".into());
    let s = group_generations(vec![first, next]);
    assert_eq!(s.len(), 2);
    assert_eq!(s[1].key, "sess");
}

#[test]
fn trace_keys_avoid_client_session_ids() {
    let clash = trace_key(Some("k"), "t-1");
    let mut owner = g("s", 1);
    owner.session_id = Some(clash.clone());
    let mut delta = g("d", 2);
    delta.absent.prompt = true;
    delta.trace_id = "t-1".into();
    let s = group_generations(vec![owner, delta]);
    assert_eq!(s[0].key, clash);
    assert_eq!(s[1].key, format!("{clash}-2"));
}

// ── request bodies ──────────────────────────────────────────────────────

fn attr(k: &str, v: &str) -> Value {
    json!({"key": k, "value": {"stringValue": v}})
}

/// One OpenRouter `LLM Generation` span.
fn span(id: &str, start: u64, session: Option<&str>, msgs: &[(&str, &str)]) -> Value {
    let prompt = json!({"messages": msgs
        .iter()
        .map(|(r, t)| json!({"role": r, "content": t}))
        .collect::<Vec<_>>()});
    let mut attributes = vec![
        attr("gen_ai.response.id", id),
        attr("gen_ai.prompt", &prompt.to_string()),
        attr("gen_ai.completion", r#"{"completion":"ok"}"#),
    ];
    if let Some(s) = session {
        attributes.push(attr("session.id", s));
    }
    json!({
        "traceId": format!("t-{id}"), "spanId": "r", "name": "LLM Generation",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 1).to_string(),
        "attributes": attributes
    })
}

fn all_spans(bodies: &[Value]) -> impl Iterator<Item = &Value> {
    bodies
        .iter()
        .flat_map(|b| b["resourceSpans"].as_array().unwrap())
        .flat_map(|rs| rs["scopeSpans"].as_array().unwrap())
        .flat_map(|ss| ss["spans"].as_array().unwrap())
}

fn body(spans: Vec<Value>) -> Value {
    json!({"resourceSpans": [{"scopeSpans": [{"spans": spans}]}]})
}

fn group(requests: &[Value]) -> Vec<SessionRequests> {
    group_sessions(requests, ProfileSelection::Auto)
        .unwrap()
        .output
}

fn path_json(p: &Path) -> Value {
    serde_json::to_value(p).unwrap()
}

#[test]
fn one_body_holding_two_sessions_is_cut_in_two() {
    let both = body(vec![
        span("g1", 1, Some("s1"), &[("user", "a")]),
        span("g2", 2, None, &[("system", "S"), ("user", "b")]),
        span(
            "g3",
            3,
            Some("s1"),
            &[("user", "a"), ("assistant", "ok"), ("user", "c")],
        ),
    ]);
    let sessions = group(std::slice::from_ref(&both));
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].key, "s1");
    assert_eq!(sessions[0].session_id.as_deref(), Some("s1"));
    assert!(sessions[1].key.starts_with("otel-cluster:"));
    let span_ids = |s: &SessionRequests| -> Vec<String> {
        all_spans(&s.requests)
            .map(|v| {
                v["attributes"][0]["value"]["stringValue"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    assert_eq!(span_ids(&sessions[0]), ["g1", "g3"]);
    assert_eq!(span_ids(&sessions[1]), ["g2"]);
    for s in &sessions {
        assert_eq!(s.requests.len(), 1);
        let d = derive_path(&s.requests, &crate::tests::otel::classified()).unwrap();
        assert_eq!(
            d.output.meta.as_ref().unwrap().extra["otel"]["derived_session_id"],
            s.derived_session_id()
        );
    }
}

#[test]
fn a_session_id_mixed_batch_derives_without_mixed_sessions() {
    let requests = vec![
        body(vec![span("g1", 1, Some("s1"), &[("user", "a")])]),
        body(vec![span("g2", 2, Some("s2"), &[("user", "a")])]),
        body(vec![span("g3", 3, None, &[("user", "z")])]),
    ];
    assert!(matches!(
        derive_path(&requests, &crate::tests::otel::classified()),
        Err(OtelError::MixedSessions(_))
    ));
    let sessions = group(&requests);
    assert_eq!(sessions.len(), 3);
    for s in &sessions {
        derive_session(s, &crate::tests::otel::classified()).unwrap();
    }
}

#[test]
fn same_opening_id_less_sessions_derive_alike_through_derive_path() {
    let run = [("system", "S"), ("user", "task")];
    let requests: Vec<Value> = [
        span("g1", 1, None, &run),
        span(
            "g2",
            2,
            None,
            &[run[0], run[1], ("assistant", "a"), ("user", "go")],
        ),
        span("g3", 3, None, &run),
        span(
            "g4",
            4,
            None,
            &[run[0], run[1], ("assistant", "b"), ("user", "go")],
        ),
    ]
    .into_iter()
    .map(|s| body(vec![s]))
    .collect();
    let sessions = group(&requests);
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[0].key, sessions[1].key);
    assert!(!sessions.iter().any(suffixed), "{sessions:?}");
    let config = crate::tests::otel::classified();
    for s in &sessions {
        let keyed = derive_session(s, &config).unwrap().output;
        let plain = derive_path(&s.requests, &config).unwrap().output;
        assert_eq!(plain.path.id, keyed.path.id);
        assert_eq!(path_json(&plain), path_json(&keyed));
        assert_eq!(
            plain.meta.as_ref().unwrap().extra["otel"]["derived_session_id"],
            s.derived_session_id()
        );
    }
}

#[test]
fn a_body_that_is_not_otlp_fails() {
    assert!(matches!(
        group_sessions(&[json!({"sessions": {}})], ProfileSelection::Auto),
        Err(OtelError::NotOtlp)
    ));
}

#[test]
fn unreadable_elements_do_not_shift_the_cut() {
    let mut b = body(vec![
        json!("not a span"),
        span("g1", 1, Some("s1"), &[("user", "a")]),
        span("g2", 2, Some("s2"), &[("user", "b")]),
    ]);
    b["resourceSpans"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!(7));
    let sessions = group(&[b]);
    assert_eq!(
        sessions.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
        ["s1", "s2"]
    );
    assert_eq!(all_spans(&sessions[1].requests).count(), 1);
}

#[test]
fn skips_are_the_batchs() {
    let mut err = span("g9", 9, Some("s1"), &[("user", "a")]);
    err["status"] = json!({"code": 2});
    let requests = vec![
        body(vec![span("g1", 1, Some("s1"), &[("user", "a")])]),
        body(vec![err]),
        body(vec![json!({"name": "openrouter-connection-test"})]),
    ];
    let grouped = group_sessions(&requests, ProfileSelection::Auto).unwrap();
    assert_eq!(grouped.skipped.error_status, 1);
    assert_eq!(grouped.skipped.connection_test, 1);
    assert_eq!(grouped.output.len(), 1);
    // The failed generation of s1 travels with s1; the test goes nowhere.
    assert_eq!(all_spans(&grouped.output[0].requests).count(), 2);
}

// ── equivalence with grouping the generations of the whole batch ────────

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

/// Every OTLP body of the JSON files in `dir`, in file-name order.
fn bodies_in(dir: &FsPath) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("json" | "ndjson")
            )
        })
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|f| {
            crate::decode_input(
                &std::fs::read(f).unwrap(),
                f.file_name().and_then(|n| n.to_str()),
            )
            .ok()
        })
        .flatten()
        .collect()
}

/// The paths of the whole batch's generations grouped as one, as the
/// grouping layer defines them.
fn reference(requests: &[Value], sel: ProfileSelection) -> Vec<Value> {
    let out = walk::read_deliveries(requests, sel).unwrap();
    let truncated: BTreeSet<&str> = out
        .skipped
        .iter()
        .filter(|s| s.reason == walk::SkipReason::Truncated)
        .filter_map(|s| s.session_id.as_deref())
        .collect();
    group_generations(out.generations.clone())
        .into_iter()
        .map(|mut s| {
            s.truncated = s
                .session_id
                .as_deref()
                .is_some_and(|id| truncated.contains(id));
            path_json(&crate::tests::otel::derive_path(&s, &Default::default()))
        })
        .collect()
}

/// A derived key that took a `-<n>` suffix in its batch.
fn suffixed(s: &SessionRequests) -> bool {
    s.session_id.is_none()
        && s.key
            .rsplit_once('-')
            .is_some_and(|(_, n)| n.parse::<u32>().is_ok())
}

fn assert_cuts_derive_alike(name: &str, requests: &[Value], sel: ProfileSelection) {
    let want = reference(requests, sel);
    let grouped = group_sessions(requests, sel).unwrap();
    assert_eq!(grouped.output.len(), want.len(), "{name}");
    let config = DeriveConfig {
        profile: sel,
        ..crate::tests::otel::classified()
    };
    for (s, want) in grouped.output.iter().zip(&want) {
        let got = derive_session(s, &config).unwrap().output;
        assert_eq!(&path_json(&got), want, "{name}: {}", s.key);
        if !suffixed(s) {
            let plain = derive_path(&s.requests, &config).unwrap().output;
            assert_eq!(
                &path_json(&plain),
                want,
                "{name}: {} via derive_path",
                s.key
            );
        }
    }
}

#[test]
fn every_capture_cut_derives_like_the_grouped_batch() {
    let openrouter = bodies_in(&fixtures().join("openrouter"));
    assert_cuts_derive_alike("openrouter", &openrouter, ProfileSelection::Auto);
    for name in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        for mode in ["span", "event"] {
            let dir = fixtures().join("semconv").join(name).join(mode);
            assert_cuts_derive_alike(
                &format!("{name}/{mode}"),
                &bodies_in(&dir),
                ProfileSelection::Auto,
            );
        }
    }
    let cont = fixtures().join("semconv/openai-responses/span-continuation");
    assert_cuts_derive_alike("continuation", &bodies_in(&cont), ProfileSelection::Semconv);
    let oi = fixtures().join("openinference/openai-chat");
    assert_cuts_derive_alike(
        "openinference",
        &bodies_in(&oi),
        ProfileSelection::OpenInference,
    );
}

#[test]
fn every_capture_mixed_into_one_batch_cuts_alike() {
    let mut all = bodies_in(&fixtures().join("openrouter"));
    for name in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        for mode in ["span", "event"] {
            all.extend(bodies_in(&fixtures().join("semconv").join(name).join(mode)));
        }
    }
    assert_cuts_derive_alike("all", &all, ProfileSelection::Auto);
}

#[test]
fn id_and_id_less_sessions_keep_their_keys() {
    let requests = bodies_in(&fixtures().join("openrouter"));
    let keys: Vec<String> = group(&requests).into_iter().map(|s| s.key).collect();
    assert!(
        keys.contains(&"177b923f-8cf6-42fc-9f30-9a7b86236265".to_string()),
        "{keys:?}"
    );
    assert!(
        keys.contains(&"otel-cluster:a5e9812ec99e6966".to_string()),
        "{keys:?}"
    );
}
