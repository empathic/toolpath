//! Profile resolution with semconv in the auto list (docs/agents/formats/otel.md:
//! Walker, step 1, and Profile `semconv`).

use super::common::*;
use serde_json::{Value, json};
// Explicit import: shadows common's one-arg `read_deliveries` helper.
use crate::tests::otel::{ProfileSelection, ReadOutcome, SkipReason, read_deliveries};

fn read(values: &[Value], sel: ProfileSelection) -> ReadOutcome {
    read_deliveries(values, sel).unwrap()
}

fn semconv() -> ProfileSelection {
    ProfileSelection::Semconv
}

#[test]
fn m0_under_auto_is_still_all_openrouter() {
    for (file, key) in REAL {
        let out = read(&deliveries(file), ProfileSelection::Auto);
        assert!(
            out.generations.iter().all(|g| g.profile == "openrouter"),
            "{file}"
        );
        assert_eq!(
            out.generations.len(),
            strings(&expected()["sessions"][key]["generation_ids"]).len()
        );
        assert_eq!(out.unclaimed, 0, "{file}");
        assert!(out.skipped.is_empty(), "{file}");
    }
}

#[test]
fn m0_under_semconv_is_one_skeleton_per_request_with_children_absorbed() {
    let exp = expected();
    for (file, key) in REAL {
        let out = read(&deliveries(file), semconv());
        let ids: Vec<String> = out.generations.iter().map(|g| g.id.clone()).collect();
        assert_eq!(
            ids,
            strings(&exp["sessions"][key]["generation_ids"]),
            "{file}"
        );
        assert_eq!(
            out.unclaimed, 0,
            "{file}: children must be absorbed by the ancestor rule"
        );
        for g in &out.generations {
            assert_eq!(g.profile, "semconv");
            assert!(
                g.absent.prompt && g.absent.completion,
                "semconv never reads gen_ai.prompt"
            );
        }
    }
    let err = read(&deliveries("codex-error-span.json"), semconv());
    assert_eq!(err.skipped[0].reason, SkipReason::ErrorStatus);
    assert!(err.skipped[0].generation_id.is_some());
    let conn = read(&deliveries("connection-test.json"), semconv());
    assert_eq!((conn.generations.len(), conn.unclaimed), (0, 1));
}

fn kv(k: &str, v: &str) -> Value {
    json!({"key": k, "value": {"stringValue": v}})
}

fn rs(service: &str, scope: &str, spans: Vec<Value>) -> Value {
    json!({"resource": {"attributes": [kv("service.name", service)]},
           "scopeSpans": [{"scope": {"name": scope}, "spans": spans}]})
}

fn sp(id: &str, parent: Option<&str>, name: &str, attrs: Vec<Value>) -> Value {
    json!({"traceId": "tt", "spanId": id, "parentSpanId": parent, "name": name,
           "startTimeUnixNano": "1", "endTimeUnixNano": "2", "attributes": attrs, "status": {}})
}

fn openrouter_root(parent: Option<&str>) -> Value {
    sp(
        "bbbbbbbbbbbbbbbb",
        parent,
        "LLM Generation",
        vec![
            kv("gen_ai.operation.name", "chat"),
            kv("gen_ai.response.id", "gen-or"),
            kv(
                "gen_ai.prompt",
                "{\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
            ),
            kv("gen_ai.completion", "{\"completion\":\"ok\"}"),
        ],
    )
}

#[test]
fn openrouter_root_under_semconv_chat_span_keeps_both_generations() {
    let app = sp(
        "aaaaaaaaaaaaaaaa",
        None,
        "chat gpt",
        vec![
            kv("gen_ai.operation.name", "chat"),
            kv("gen_ai.response.id", "chatcmpl-app"),
        ],
    );
    let child = sp(
        "cccccccccccccccc",
        Some("bbbbbbbbbbbbbbbb"),
        "generation",
        vec![
            kv("gen_ai.operation.name", "chat"),
            kv("gen_ai.response.id", "gen-or:generation"),
        ],
    );
    let d = json!({"resourceSpans": [
        rs("app", "opentelemetry.instrumentation.openai_v2", vec![app]),
        rs("openrouter", "openrouter", vec![openrouter_root(Some("aaaaaaaaaaaaaaaa")), child]),
    ]});
    let out = read(&[d], ProfileSelection::Auto);
    let mut got: Vec<(String, String)> = out
        .generations
        .iter()
        .map(|g| (g.id.clone(), g.profile.clone()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("chatcmpl-app".to_string(), "semconv".to_string()),
            ("gen-or".to_string(), "openrouter".to_string())
        ]
    );
    assert_eq!(out.unclaimed, 0);
}

#[test]
fn a_markerless_child_claimed_by_semconv_is_absorbed_under_openrouter() {
    let child = sp(
        "cccccccccccccccc",
        Some("bbbbbbbbbbbbbbbb"),
        "generation",
        vec![
            kv("gen_ai.operation.name", "chat"),
            kv("gen_ai.response.id", "gen-or:generation"),
        ],
    );
    let d = json!({"resourceSpans": [
        rs("openrouter", "openrouter", vec![openrouter_root(None)]),
        rs("collector", "rewritten", vec![child]),
    ]});
    let out = read(&[d], ProfileSelection::Auto);
    let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["gen-or"]);
    assert_eq!(out.unclaimed, 0);
}

#[test]
fn named_openrouter_leaves_semconv_spans_unclaimed() {
    let d = json!({"resourceSpans": [rs("app", "x", vec![sp("aaaaaaaaaaaaaaaa", None, "chat m",
        vec![kv("gen_ai.operation.name", "chat")])])]});
    let out = read(&[d], ProfileSelection::OpenRouter);
    assert_eq!((out.generations.len(), out.unclaimed), (0, 1));
}

/// OpenRouter returns its `gen-…` id to the caller, so an app-side semconv
/// `chat` span and the Broadcast root for the same call share
/// `gen_ai.response.id`. The better-ranked openrouter generation survives
/// whichever delivery comes first; the semconv one is a Duplicate skip.
#[test]
fn shared_id_keeps_the_better_ranked_profile_in_either_order() {
    let app = || {
        rs(
            "app",
            "opentelemetry.instrumentation.openai_v2",
            vec![sp(
                "aaaaaaaaaaaaaaaa",
                None,
                "chat gpt",
                vec![
                    kv("gen_ai.operation.name", "chat"),
                    kv("gen_ai.response.id", "gen-or"),
                ],
            )],
        )
    };
    let broadcast = || rs("openrouter", "openrouter", vec![openrouter_root(None)]);
    for (name, spans) in [
        ("app first", vec![app(), broadcast()]),
        ("broadcast first", vec![broadcast(), app()]),
    ] {
        let out = read(&[json!({"resourceSpans": spans})], ProfileSelection::Auto);
        let got: Vec<(&str, &str)> = out
            .generations
            .iter()
            .map(|g| (g.id.as_str(), g.profile.as_str()))
            .collect();
        assert_eq!(got, [("gen-or", "openrouter")], "{name}");
        assert_eq!(out.skipped.len(), 1, "{name}");
        assert_eq!(out.skipped[0].reason, SkipReason::Duplicate, "{name}");
        assert_eq!(
            out.skipped[0].generation_id.as_deref(),
            Some("gen-or"),
            "{name}"
        );
    }
}

#[test]
fn same_profile_duplicates_keep_the_first() {
    let chat = |span: &str, model: &str| {
        sp(
            span,
            None,
            "chat gpt",
            vec![
                kv("gen_ai.operation.name", "chat"),
                kv("gen_ai.response.id", "chatcmpl-1"),
                kv("gen_ai.request.model", model),
            ],
        )
    };
    let d = json!({"resourceSpans": [rs(
        "app",
        "opentelemetry.instrumentation.openai_v2",
        vec![chat("aaaaaaaaaaaaaaaa", "first"), chat("dddddddddddddddd", "second")],
    )]});
    let out = read(&[d], ProfileSelection::Auto);
    assert_eq!(out.generations.len(), 1);
    assert_eq!(out.generations[0].request_model.as_deref(), Some("first"));
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].reason, SkipReason::Duplicate);
}
