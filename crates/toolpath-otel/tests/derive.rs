//! The public API over the committed fixtures. Each M0 conversation's
//! derived Path must match its golden exactly.
//!
//! Bless: TOOLPATH_OTEL_BLESS=1 cargo test -p toolpath-otel --test derive

use serde_json::Value;
use std::path::PathBuf;
use toolpath_otel::{
    DeriveConfig, OtelError, ProfileSelection, SkipCounts, derive, derive_graph, derive_path,
};

const CONVERSATIONS: [&str; 5] = [
    "claude-code.ndjson",
    "codex.ndjson",
    "opencode.ndjson",
    "pi.ndjson",
    "synthetic-fork.ndjson",
];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

/// One request body per line (`.ndjson`), or one JSON document.
fn requests(rel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures().join(rel)).unwrap();
    if rel.ends_with(".ndjson") {
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    } else {
        vec![serde_json::from_str(&text).unwrap()]
    }
}

fn openrouter(file: &str) -> Vec<Value> {
    requests(&format!("openrouter/{file}"))
}

fn json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn golden(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/m0")
        .join(format!("{}.path.json", file.trim_end_matches(".ndjson")))
}

#[test]
fn m0_paths_match_their_goldens() {
    let bless = std::env::var_os("TOOLPATH_OTEL_BLESS").is_some();
    for file in CONVERSATIONS {
        let path = derive_path(&openrouter(file), &DeriveConfig::default())
            .unwrap()
            .output;
        let got = Value::Array(vec![serde_json::to_value(path).unwrap()]);
        if bless {
            std::fs::write(
                golden(file),
                serde_json::to_string_pretty(&got).unwrap() + "\n",
            )
            .unwrap();
            continue;
        }
        let text = std::fs::read_to_string(golden(file)).unwrap();
        let want: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(got, want, "{file}: derived Path drifted from its golden");
    }
}

#[test]
fn request_order_does_not_change_the_path() {
    let mut reversed = openrouter("claude-code.ndjson");
    reversed.reverse();
    let cfg = DeriveConfig::default();
    assert_eq!(
        json(derive_path(&reversed, &cfg).unwrap().output),
        json(
            derive_path(&openrouter("claude-code.ndjson"), &cfg)
                .unwrap()
                .output
        )
    );
}

#[test]
fn derive_wraps_one_session_and_derive_graph_keeps_one_path_per_session() {
    let cfg = DeriveConfig {
        title: Some("two".into()),
        ..Default::default()
    };
    let claude = openrouter("claude-code.ndjson");
    let codex = openrouter("codex.ndjson");
    let one = derive(&[&claude], &cfg).unwrap().output;
    assert_eq!(one.paths.len(), 1);
    assert_eq!(
        one.meta.as_ref().and_then(|m| m.title.as_deref()),
        Some("two")
    );

    let g = derive(&[&claude, &codex], &cfg).unwrap().output;
    assert_eq!(g.paths.len(), 2);
    assert_eq!(
        json(&g),
        json(derive_graph(&[&claude, &codex], &cfg).unwrap().output)
    );
    assert_eq!(
        g.meta.as_ref().and_then(|m| m.title.as_deref()),
        Some("two")
    );
    let first = derive_path(&claude, &cfg).unwrap().output;
    assert_eq!(
        g.graph.id,
        format!("graph-{}", first.path.id.trim_start_matches("path-"))
    );
    assert!(g.graph.id.starts_with("graph-otel-"));
}

#[test]
fn errors_name_non_otlp_input_and_empty_sessions() {
    let cfg = DeriveConfig::default();
    assert!(matches!(
        derive_path(&[serde_json::json!({"sessions": {}})], &cfg),
        Err(OtelError::NotOtlp)
    ));
    assert!(matches!(
        derive_path(&openrouter("connection-test.json"), &cfg),
        Err(OtelError::NoGenerations { .. })
    ));
    assert!(matches!(
        derive_path(&openrouter("codex-error-span.json"), &cfg),
        Err(OtelError::NoGenerations { .. })
    ));
}

#[test]
fn the_profile_selection_chooses_the_dialect() {
    let semconv = requests("semconv/openai-chat/span/traces.json");
    let only = |profile| DeriveConfig {
        profile,
        ..Default::default()
    };
    let auto = derive_path(&semconv, &DeriveConfig::default())
        .unwrap()
        .output;
    assert_eq!(
        json(auto),
        json(
            derive_path(&semconv, &only(ProfileSelection::Semconv))
                .unwrap()
                .output
        )
    );
    assert!(matches!(
        derive_path(&semconv, &only(ProfileSelection::OpenRouter)),
        Err(OtelError::NoGenerations { .. })
    ));
    let openinference = requests("openinference/openai-chat/traces.json");
    assert!(matches!(
        derive_path(&openinference, &DeriveConfig::default()),
        Err(OtelError::NoGenerations { .. })
    ));
    let p = derive_path(&openinference, &only(ProfileSelection::OpenInference))
        .unwrap()
        .output;
    assert_eq!(p.meta.unwrap().extra["otel"]["profile"], "openinference");
}

#[test]
fn event_mode_logs_derive_the_same_turns_as_span_mode() {
    let dir = "semconv/openai-chat";
    let span = derive_path(
        &requests(&format!("{dir}/span/traces.json")),
        &DeriveConfig::default(),
    )
    .unwrap()
    .output;
    let event = derive_path(
        &[
            requests(&format!("{dir}/event/traces.json")),
            requests(&format!("{dir}/event/logs.json")),
        ]
        .concat(),
        &DeriveConfig::default(),
    )
    .unwrap()
    .output;
    let ids = |p: &toolpath::v1::Path| {
        p.steps
            .iter()
            .map(|s| s.step.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&span), ids(&event));
}

fn session_id(p: &toolpath::v1::Path) -> Option<&str> {
    p.meta.as_ref().unwrap().extra["otel"]["session_id"].as_str()
}

#[test]
fn requests_that_mix_session_ids_are_an_error() {
    let cfg = DeriveConfig::default();
    let claude = openrouter("claude-code.ndjson");
    let codex = openrouter("codex.ndjson");
    let mut want = vec![
        session_id(&derive_path(&claude, &cfg).unwrap().output)
            .unwrap()
            .to_string(),
        session_id(&derive_path(&codex, &cfg).unwrap().output)
            .unwrap()
            .to_string(),
    ];
    want.sort();
    for mixed in [
        [&codex[..], &claude[..]].concat(),
        [&claude[..], &codex[..]].concat(),
    ] {
        match derive_path(&mixed, &cfg) {
            Err(OtelError::MixedSessions(ids)) => assert_eq!(ids, want),
            other => panic!("expected MixedSessions, got {other:?}"),
        }
    }
    assert!(matches!(
        derive_graph(&[&claude, &[codex.clone(), claude.clone()].concat()], &cfg),
        Err(OtelError::MixedSessions(_))
    ));
}

#[test]
fn one_session_id_or_none_derives() {
    let cfg = DeriveConfig::default();
    let claude = derive_path(&openrouter("claude-code.ndjson"), &cfg).unwrap();
    assert!(session_id(&claude.output).is_some());

    let pi = openrouter("pi.ndjson");
    let one_trace = derive_path(&pi[..1], &cfg).unwrap().output;
    assert_eq!(session_id(&one_trace), None);
    let key = &one_trace.meta.as_ref().unwrap().extra["otel"]["session_key"];
    assert!(key.as_str().unwrap().starts_with("otel-cluster:"));
    let whole = derive_path(&pi, &cfg).unwrap().output;
    assert_eq!(session_id(&whole), None);
}

fn attr(k: &str, v: &str) -> Value {
    serde_json::json!({"key": k, "value": {"stringValue": v}})
}

fn generation(id: &str, prompt: &str) -> Value {
    let mut attrs = vec![
        attr("session.id", "s1"),
        attr("gen_ai.prompt", prompt),
        attr("gen_ai.completion", r#"{"completion":"ok"}"#),
    ];
    if !id.is_empty() {
        attrs.push(attr("gen_ai.response.id", id));
    }
    serde_json::json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": format!("t-{id}"), "spanId": "01", "name": "LLM Generation",
        "status": {"code": 1}, "attributes": attrs
    }]}]}]})
}

const GOOD: &str = r#"{"messages":[{"role":"user","content":"hi"}]}"#;

fn skips(requests: &[Value]) -> SkipCounts {
    derive_path(requests, &DeriveConfig::default())
        .unwrap()
        .skipped
}

#[test]
fn skip_counts_count_each_reason() {
    let clean = skips(&openrouter("claude-code.ndjson"));
    assert_eq!(clean, SkipCounts::default());
    assert_eq!(clean.total(), 0);

    let conn = skips(
        &[
            openrouter("claude-code.ndjson"),
            openrouter("connection-test.json"),
        ]
        .concat(),
    );
    assert_eq!((conn.connection_test, conn.total()), (1, 1));

    let err = skips(
        &[
            openrouter("codex.ndjson"),
            openrouter("codex-error-span.json"),
        ]
        .concat(),
    );
    assert_eq!((err.error_status, err.total()), (1, 1));

    let claude = openrouter("claude-code.ndjson");
    let twice = derive_path(
        &[&claude[..], &claude[..]].concat(),
        &DeriveConfig::default(),
    )
    .unwrap();
    assert_eq!((twice.skipped.duplicate, twice.skipped.total()), (5, 5));
    assert_eq!(
        json(twice.output),
        json(
            derive_path(&claude, &DeriveConfig::default())
                .unwrap()
                .output
        )
    );

    let cut = skips(&[
        generation("g1", GOOD),
        generation("g2", r#"{"messages":[{"role""#),
    ]);
    assert_eq!((cut.truncated, cut.total()), (1, 1));

    let missing = skips(&[generation("g1", GOOD), generation("", GOOD)]);
    assert_eq!((missing.missing_payload, missing.total()), (1, 1));

    let other = serde_json::json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        {"traceId": "t9", "spanId": "02", "name": "http.request"}
    ]}]}]});
    let logs = serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{}]}]}]});
    let unclaimed = skips(&[generation("g1", GOOD), other, logs]);
    assert_eq!((unclaimed.unclaimed, unclaimed.total()), (2, 2));
}

#[test]
fn skip_counts_ride_on_no_generations_and_sum_over_a_graph() {
    let cfg = DeriveConfig::default();
    match derive_path(&openrouter("connection-test.json"), &cfg) {
        Err(OtelError::NoGenerations { skipped }) => assert_eq!(skipped.connection_test, 1),
        other => panic!("expected NoGenerations, got {other:?}"),
    }
    let claude = [
        openrouter("claude-code.ndjson"),
        openrouter("connection-test.json"),
    ]
    .concat();
    let codex = [
        openrouter("codex.ndjson"),
        openrouter("codex-error-span.json"),
    ]
    .concat();
    let g = derive_graph(&[&claude, &codex], &cfg).unwrap().skipped;
    assert_eq!((g.connection_test, g.error_status, g.total()), (1, 1, 2));
    assert_eq!(derive(&[&claude], &cfg).unwrap().skipped.connection_test, 1);
}
