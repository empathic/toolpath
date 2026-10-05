//! The minimal OpenInference profile (docs/agents/formats/otel.md: Profile
//! `openinference`).

use crate::tests::otel::{
    DeriveConfig, History, ProfileSelection, ReadOutcome, decode_input, derive_path,
    group_sessions, read_deliveries,
};
use serde_json::{Value, json};
use std::path::PathBuf;

fn s(v: &str) -> Value {
    json!({"stringValue": v})
}
fn kv(k: &str, v: Value) -> Value {
    json!({"key": k, "value": v})
}

fn llm_span(attrs: Vec<Value>) -> Value {
    let mut a = vec![
        kv("openinference.span.kind", s("LLM")),
        kv("session.id", s("oi-s")),
    ];
    a.extend(attrs);
    json!({"resourceSpans": [{"resource": {"attributes": [kv("service.name", s("app"))]},
        "scopeSpans": [{"scope": {"name": "openinference.instrumentation.openai"}, "spans": [
            {"traceId": "t", "spanId": "abcd", "name": "ChatCompletion", "startTimeUnixNano": "1",
             "endTimeUnixNano": "2", "attributes": a, "status": {}},
            {"traceId": "t", "spanId": "ef01", "parentSpanId": "abcd", "name": "tool",
             "attributes": [kv("openinference.span.kind", s("TOOL"))]}]}]}]})
}

fn read(d: Value, sel: ProfileSelection) -> ReadOutcome {
    read_deliveries(&[d], sel).unwrap()
}

fn named() -> ProfileSelection {
    ProfileSelection::OpenInference
}

#[test]
fn indices_sort_numerically_and_fields_map() {
    let mut attrs = Vec::new();
    for i in 0..=10 {
        attrs.push(kv(
            &format!("llm.input_messages.{i}.message.role"),
            s("user"),
        ));
        attrs.push(kv(
            &format!("llm.input_messages.{i}.message.content"),
            s(&format!("m{i}")),
        ));
    }
    attrs.extend([
        kv("llm.output_messages.0.message.role", s("assistant")),
        kv(
            "llm.output_messages.0.message.tool_calls.0.tool_call.id",
            s("call_1"),
        ),
        kv(
            "llm.output_messages.0.message.tool_calls.0.tool_call.function.name",
            s("read_file"),
        ),
        kv(
            "llm.output_messages.0.message.tool_calls.0.tool_call.function.arguments",
            s("{\"path\":\"a\"}"),
        ),
        kv("llm.output_messages.1.message.role", s("assistant")),
        kv("llm.output_messages.1.message.content", s("alt")),
        kv("llm.model_name", s("gpt-x")),
        kv("llm.system", s("openai")),
        kv("llm.token_count.prompt", json!({"intValue": "12"})),
        kv("llm.token_count.completion", json!({"intValue": "3"})),
        kv("llm.token_count.total", json!({"intValue": "15"})),
        kv(
            "llm.token_count.prompt_details.cache_read",
            json!({"intValue": "4"}),
        ),
        kv(
            "llm.token_count.completion_details.reasoning",
            json!({"intValue": "9"}),
        ),
        kv("user.id", s("u")),
    ]);
    let out = read(llm_span(attrs), named());
    assert_eq!(out.unclaimed, 0, "the TOOL span is absorbed");
    let g = &out.generations[0];
    let texts: Vec<Value> = g.messages.iter().map(|m| m.content.clone()).collect();
    assert_eq!(
        texts,
        (0..=10).map(|i| json!(format!("m{i}"))).collect::<Vec<_>>(),
        "10 sorts after 9"
    );
    assert_eq!(g.id, "span-abcd");
    assert_eq!(g.session_id.as_deref(), Some("oi-s"));
    assert_eq!(g.user_id.as_deref(), Some("u"));
    assert_eq!(g.completion.tool_calls[0].id, "call_1");
    assert_eq!(
        g.completion.tool_calls[0].function.arguments,
        json!("{\"path\":\"a\"}")
    );
    assert_eq!(g.source_meta["choices"][0]["message.content"], "alt");
    assert_eq!(
        (g.request_model.as_deref(), g.response_model.as_deref()),
        (Some("gpt-x"), Some("gpt-x"))
    );
    assert_eq!(g.provider.as_deref(), Some("openai"));
    assert_eq!(
        (
            g.usage.input_tokens,
            g.usage.output_tokens,
            g.usage.cached_input_tokens
        ),
        (Some(8), Some(3), Some(4))
    );
    assert_eq!(g.usage.reasoning_tokens, Some(3), "clamped to output");
    assert_eq!(
        g.usage.cache_basis,
        Some(crate::tests::otel::CacheBasis::Inclusive)
    );
    assert_eq!(g.profile, "openinference");
}

#[test]
fn tool_messages_carry_their_call_id() {
    let out = read(
        llm_span(vec![
            kv("llm.input_messages.0.message.role", s("tool")),
            kv("llm.input_messages.0.message.tool_call_id", s("call_1")),
            kv("llm.input_messages.0.message.content", s("result")),
            kv("llm.output_messages.0.message.role", s("assistant")),
            kv("llm.output_messages.0.message.content", s("ok")),
        ]),
        named(),
    );
    let m = &out.generations[0].messages[0];
    assert_eq!(
        (m.role.as_str(), m.tool_call_id.as_deref()),
        ("tool", Some("call_1"))
    );
}

#[test]
fn redacted_or_missing_families_make_a_skeleton_side() {
    let redacted = read(
        llm_span(vec![
            kv("llm.input_messages.0.message.role", s("user")),
            kv("llm.input_messages.0.message.content", s("__REDACTED__")),
            kv("llm.output_messages.0.message.role", s("assistant")),
            kv("llm.output_messages.0.message.content", s("visible")),
        ]),
        named(),
    );
    let g = &redacted.generations[0];
    assert!(g.absent.prompt && !g.absent.completion);
    assert!(g.messages.is_empty());
    assert_eq!(g.history, History::Delta);
    assert_eq!(g.completion.text, "visible");
    let missing = read(llm_span(vec![kv("llm.model_name", s("m"))]), named());
    assert!(missing.generations[0].absent.prompt && missing.generations[0].absent.completion);
}

#[test]
fn not_consulted_under_auto() {
    let out = read(llm_span(vec![]), ProfileSelection::Auto);
    assert!(out.generations.is_empty());
    assert_eq!(out.unclaimed, 2);
}

fn capture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-fixtures/otel/openinference/openai-chat")
}

#[test]
fn the_capture_imports_as_one_session() {
    let bytes = std::fs::read(capture_dir().join("traces.json")).unwrap();
    let values = decode_input(&bytes, Some("traces.json")).unwrap();
    let exp: Value =
        serde_json::from_slice(&std::fs::read(capture_dir().join("expected.json")).unwrap())
            .unwrap();
    let out = read_deliveries(&values, named()).unwrap();
    assert_eq!(out.generations.len(), 3);
    assert!(out.generations.iter().all(|g| g.id.starts_with("span-")));
    assert!(
        out.generations
            .iter()
            .all(|g| g.session_id.as_deref() == exp["session_id"].as_str())
    );
    let texts: Vec<&str> = out
        .generations
        .iter()
        .map(|g| g.completion.text.as_str())
        .collect();
    assert_eq!(json!(texts), exp["completion_texts"]);
    let sessions = group_sessions(out.generations);
    assert_eq!(sessions.len(), 1);
    let path = serde_json::to_value(derive_path(&sessions[0], &DeriveConfig::default())).unwrap();
    let tools: Vec<&Value> = path["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["change"].as_object().unwrap().values())
        .filter_map(|c| c["structural"]["tool_uses"].as_array())
        .flatten()
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["id"], exp["tool_calls"][0]["id"]);
    assert_eq!(tools[0]["input"], exp["tool_calls"][0]["input"]);
    assert_eq!(
        tools[0]["result"]["content"],
        exp["tool_calls"][0]["result"]
    );
}
