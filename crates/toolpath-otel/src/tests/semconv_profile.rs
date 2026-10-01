//! The semconv profile, through the public API only (read_deliveries with
//! `--profile semconv`): current-form content, v1.36 legacy events, and
//! `execute_tool` results. Resolution across profiles: tests/resolution.rs.

use crate::tests::otel::{
    CacheBasis, History, ProfileSelection, ReadOutcome, SkipReason, read_deliveries,
};
use serde_json::{Value, json};

fn s(v: &str) -> Value {
    json!({"stringValue": v})
}
fn int(v: u64) -> Value {
    json!({"intValue": v.to_string()})
}
fn kv(k: &str, v: Value) -> Value {
    json!({"key": k, "value": v})
}
/// Plain JSON → a structured OTLP AnyValue (kvlist / array), as on events.
fn any(v: &Value) -> Value {
    match v {
        Value::String(x) => json!({"stringValue": x}),
        Value::Bool(b) => json!({"boolValue": b}),
        Value::Number(n) if n.is_u64() || n.is_i64() => json!({"intValue": n.to_string()}),
        Value::Number(n) => json!({"doubleValue": n.as_f64()}),
        Value::Array(a) => json!({"arrayValue": {"values": a.iter().map(any).collect::<Vec<_>>()}}),
        Value::Object(m) => json!({"kvlistValue": {"values":
            m.iter().map(|(k, v)| json!({"key": k, "value": any(v)})).collect::<Vec<_>>()}}),
        Value::Null => json!({}),
    }
}
fn json_attr(k: &str, v: &Value) -> Value {
    kv(k, s(&v.to_string()))
}

fn span(span_id: &str, attrs: Vec<Value>) -> Value {
    json!({"traceId": "t1", "spanId": span_id, "name": "chat m", "kind": 3,
           "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
           "attributes": attrs, "status": {}})
}

fn delivery_in(scope: &str, spans: Vec<Value>) -> Value {
    json!({"resourceSpans": [{
        "resource": {"attributes": [kv("service.name", s("app"))]},
        "scopeSpans": [{"scope": {"name": scope, "version": "1.2"}, "spans": spans}]}]})
}

fn delivery(spans: Vec<Value>) -> Value {
    delivery_in("test.scope", spans)
}

fn read(d: &[Value]) -> ReadOutcome {
    read_deliveries(d, ProfileSelection::Semconv).unwrap()
}

fn chat(extra: Vec<Value>) -> Vec<Value> {
    let mut a = vec![
        kv("gen_ai.operation.name", s("chat")),
        kv("gen_ai.response.id", s("resp-1")),
    ];
    a.extend(extra);
    a
}

fn one(extra: Vec<Value>) -> crate::tests::otel::Generation {
    let out = read(&[delivery(vec![span("s1", chat(extra))])]);
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    out.generations.into_iter().next().unwrap()
}

#[test]
fn claims_inference_ops_absorbs_other_genai_ops() {
    let spans = vec![
        span(
            "a",
            vec![
                kv("gen_ai.operation.name", s("chat")),
                kv("gen_ai.response.id", s("r1")),
            ],
        ),
        span(
            "b",
            vec![kv("gen_ai.operation.name", s("generate_content"))],
        ),
        span("c", vec![kv("gen_ai.operation.name", s("text_completion"))]),
        span("d", vec![kv("gen_ai.operation.name", s("execute_tool"))]),
        span("e", vec![kv("gen_ai.operation.name", s("invoke_agent"))]),
        span("f", vec![kv("http.method", s("POST"))]),
    ];
    let out = read(&[delivery(spans)]);
    let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["r1", "span-b", "span-c"]);
    assert_eq!(out.unclaimed, 1, "only the non-GenAI span is unclaimed");
    assert!(out.generations.iter().all(|g| g.profile == "semconv"));
}

#[test]
fn text_parts_join_into_string_content() {
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "user", "parts": [{"type": "text", "content": "a"}, {"type": "text", "content": "b"}],
             "name": "dropped"}
        ]),
    )]);
    assert_eq!(g.messages[0].role, "user");
    assert_eq!(g.messages[0].content, json!("a\nb"));
    assert_eq!(g.messages[0].name, None);
}

#[test]
fn mixed_parts_keep_a_parts_list() {
    let blob = json!({"type": "blob", "modality": "image", "content": "AAAA"});
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "user", "parts": [{"type": "text", "content": "see"}, blob.clone()]}
        ]),
    )]);
    assert_eq!(
        g.messages[0].content,
        json!([{"type": "text", "text": "see"}, blob])
    );
}

#[test]
fn history_reasoning_goes_to_reasoning_details() {
    let r = json!({"type": "reasoning", "content": "think"});
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "assistant", "parts": [r.clone(), {"type": "text", "content": "ok"}]}
        ]),
    )]);
    assert_eq!(g.messages[0].content, json!("ok"));
    assert_eq!(g.messages[0].reasoning_details, vec![r]);
}

#[test]
fn tool_call_parts_become_tool_calls_with_arguments_as_received() {
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "assistant", "parts": [
                {"type": "tool_call", "id": "c1", "name": "read", "arguments": "{\"p\":1}"},
                {"type": "tool_call", "name": "read", "arguments": {"p": 2}}
            ]}
        ]),
    )]);
    let calls = &g.messages[0].tool_calls;
    assert_eq!(g.messages[0].content, Value::Null);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].function.arguments.clone()),
        ("c1", json!("{\"p\":1}"))
    );
    assert_eq!(
        (calls[1].id.as_str(), calls[1].function.arguments.clone()),
        ("", json!({"p": 2}))
    );
}

#[test]
fn tool_call_responses_become_one_tool_message_each() {
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "tool", "parts": [
                {"type": "tool_call_response", "id": "c1", "response": "plain"},
                {"type": "tool_call_response", "response": {"b": 1, "a": [true]}}
            ]}
        ]),
    )]);
    assert_eq!(g.messages.len(), 2);
    assert_eq!(g.messages[0].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(g.messages[0].content, json!("plain"));
    assert_eq!(
        g.messages[1].tool_call_id.as_deref(),
        Some(""),
        "absent id reads as \"\""
    );
    assert_eq!(
        g.messages[1].content,
        json!("{\"a\":[true],\"b\":1}"),
        "canonical compact JSON"
    );
    assert!(g.messages.iter().all(|m| m.role == "tool"));
}

#[test]
fn other_parts_are_kept_verbatim_and_compaction_marks_the_generation() {
    let parts = [
        json!({"type": "file", "modality": "document", "file_id": "f1"}),
        json!({"type": "uri", "modality": "image", "uri": "https://x"}),
        json!({"type": "server_tool_call", "name": "web_search", "server_tool_call": {}}),
        json!({"type": "compaction", "content": "summary"}),
        json!({"type": "custom_thing", "x": 1}),
    ];
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([{"role": "user", "parts": parts}]),
    )]);
    assert_eq!(g.messages[0].content, Value::Array(parts.to_vec()));
    assert!(g.compacted);
    assert!(!one(vec![json_attr("gen_ai.input.messages", &json!([]))]).compacted);
    assert!(
        one(vec![kv(
            "gen_ai.conversation.compacted",
            json!({"boolValue": true})
        )])
        .compacted
    );
}

#[test]
fn system_instructions_are_prepended() {
    let g = one(vec![
        json_attr(
            "gen_ai.system_instructions",
            &json!([{"type": "text", "content": "S1"}, {"type": "text", "content": "S2"}]),
        ),
        json_attr(
            "gen_ai.input.messages",
            &json!([
                {"role": "system", "parts": [{"type": "text", "content": "own"}]},
                {"role": "user", "parts": [{"type": "text", "content": "u"}]}
            ]),
        ),
    ]);
    let roles: Vec<(&str, Value)> = g
        .messages
        .iter()
        .map(|m| (m.role.as_str(), m.content.clone()))
        .collect();
    assert_eq!(
        roles,
        [
            ("system", json!("S1\nS2")),
            ("system", json!("own")),
            ("user", json!("u"))
        ]
    );
}

#[test]
fn string_and_structured_content_decode_equal() {
    let input = json!([{"role": "user", "parts": [{"type": "text", "content": "hi"}]}]);
    let output = json!([{"role": "assistant", "parts": [
        {"type": "text", "content": "yo"},
        {"type": "tool_call", "id": "c1", "name": "read", "arguments": {"p": "x"}}]}]);
    let a = one(vec![
        json_attr("gen_ai.input.messages", &input),
        json_attr("gen_ai.output.messages", &output),
    ]);
    let b = one(vec![
        kv("gen_ai.input.messages", any(&input)),
        kv("gen_ai.output.messages", any(&output)),
    ]);
    assert_eq!(a.messages, b.messages);
    assert_eq!(a.completion, b.completion);
}

#[test]
fn unparseable_json_string_content_is_truncated() {
    for key in [
        "gen_ai.input.messages",
        "gen_ai.output.messages",
        "gen_ai.system_instructions",
    ] {
        let out = read(&[delivery(vec![span(
            "s1",
            chat(vec![kv(key, s("[{\"role\":\"user\",\"par"))]),
        )])]);
        assert!(out.generations.is_empty(), "{key}");
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated, "{key}");
        assert_eq!(out.skipped[0].generation_id.as_deref(), Some("resp-1"));
    }
}

#[test]
fn completion_from_the_first_output_message_others_to_choices() {
    let g = one(vec![json_attr(
        "gen_ai.output.messages",
        &json!([
            {"role": "assistant", "parts": [
                {"type": "reasoning", "content": "r1"}, {"type": "reasoning", "content": "r2"},
                {"type": "text", "content": "answer"},
                {"type": "tool_call", "id": "c9", "name": "Bash", "arguments": "{}"}],
             "finish_reason": "tool_call"},
            {"role": "assistant", "parts": [{"type": "text", "content": "alt"}]}
        ]),
    )]);
    assert_eq!(g.completion.text, "answer");
    assert_eq!(g.completion.reasoning.as_deref(), Some("r1\nr2"));
    assert_eq!(g.completion.reasoning_details.len(), 2);
    assert_eq!(g.completion.tool_calls[0].id, "c9");
    assert_eq!(
        g.finish_reason.as_deref(),
        Some("tool_call"),
        "per-message fallback"
    );
    assert_eq!(g.source_meta["choices"][0]["parts"][0]["content"], "alt");
    assert!(!g.absent.completion);
}

#[test]
fn empty_output_messages_is_an_empty_completion() {
    let g = one(vec![json_attr("gen_ai.output.messages", &json!([]))]);
    assert_eq!(g.completion.text, "");
    assert!(g.completion.tool_calls.is_empty());
    assert!(!g.absent.completion, "present but empty is not a skeleton");
}

#[test]
fn finish_reasons_first_element_array_or_json_string() {
    let arr = one(vec![kv(
        "gen_ai.response.finish_reasons",
        any(&json!(["stop", "length"])),
    )]);
    assert_eq!(arr.finish_reason.as_deref(), Some("stop"));
    assert_eq!(arr.source_meta["finish_reasons"], json!(["stop", "length"]));
    // The OpenRouter M0 shape: a JSON array inside a string.
    let text = one(vec![kv(
        "gen_ai.response.finish_reasons",
        s("[\"tool_calls\"]"),
    )]);
    assert_eq!(text.finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn content_absent_gives_a_delta_skeleton() {
    let g = one(vec![kv("gen_ai.usage.input_tokens", int(5))]);
    assert!(g.absent.prompt && g.absent.completion);
    assert_eq!(g.history, History::Delta);
    assert!(g.messages.is_empty());
    assert_eq!(g.usage.input_tokens, Some(5));
}

#[test]
fn previous_response_id_sets_continues_and_delta() {
    // SYNTHETIC input: the spec-defined attribute; no pinned instrumentation emits it.
    let g = one(vec![
        kv("gen_ai.request.previous_response.id", s("resp-0")),
        json_attr(
            "gen_ai.input.messages",
            &json!([{"role": "user", "parts": [{"type": "text", "content": "more"}]}]),
        ),
    ]);
    assert_eq!(g.continues.as_deref(), Some("resp-0"));
    assert_eq!(g.history, History::Delta);
    assert!(!g.absent.prompt);
}

#[test]
fn ids_sessions_models_provider_client() {
    let out = read(&[delivery(vec![span(
        "abc",
        vec![
            kv("gen_ai.operation.name", s("chat")),
            kv("gen_ai.conversation.id", s("conv")),
            kv("session.id", s("sess")),
            kv("user.id", s("u")),
            kv("gen_ai.request.model", s("m-req")),
            kv("gen_ai.response.model", s("m-resp")),
            kv("gen_ai.system", s("openai")),
        ],
    )])]);
    let g = &out.generations[0];
    assert_eq!(g.id, "span-abc");
    assert_eq!(
        g.session_id.as_deref(),
        Some("conv"),
        "conversation.id wins over session.id"
    );
    assert_eq!(g.user_id.as_deref(), Some("u"));
    assert_eq!(g.client_key.as_deref(), Some("app"));
    assert_eq!(
        (g.request_model.as_deref(), g.response_model.as_deref()),
        (Some("m-req"), Some("m-resp"))
    );
    assert_eq!(
        g.provider.as_deref(),
        Some("openai"),
        "deprecated gen_ai.system fallback"
    );
    let with_new = one(vec![
        kv("gen_ai.provider.name", s("anthropic")),
        kv("gen_ai.system", s("old")),
    ]);
    assert_eq!(with_new.provider.as_deref(), Some("anthropic"));
    let only_session = one(vec![kv("session.id", s("sess"))]);
    assert_eq!(only_session.session_id.as_deref(), Some("sess"));
}

#[test]
fn empty_provider_name_and_user_id_count_as_absent() {
    let g = one(vec![
        kv("gen_ai.provider.name", s("")),
        kv("gen_ai.system", s("openai")),
        kv("user.id", s("")),
    ]);
    assert_eq!(
        g.provider.as_deref(),
        Some("openai"),
        "an empty provider.name must not shadow gen_ai.system"
    );
    assert_eq!(g.user_id, None);
}

#[test]
fn usage_spellings_coalesce_and_reasoning_is_clamped() {
    let current = one(vec![
        kv("gen_ai.usage.input_tokens", int(10)),
        kv("gen_ai.usage.prompt_tokens", int(99)),
        kv("gen_ai.usage.output_tokens", int(4)),
        kv("gen_ai.usage.cache_read.input_tokens", int(3)),
        kv("gen_ai.usage.cache_write.input_tokens", int(2)),
        kv("gen_ai.usage.reasoning.output_tokens", int(9)),
    ]);
    assert_eq!(
        current.usage.input_tokens,
        Some(5),
        "current spelling wins; cache reads and writes are subtracted"
    );
    assert_eq!(current.usage.cached_input_tokens, Some(3));
    assert_eq!(current.usage.cache_write_tokens, Some(2));
    assert_eq!(current.usage.reasoning_tokens, Some(4), "clamped to output");
    let legacy = one(vec![
        kv("gen_ai.usage.prompt_tokens", int(17)),
        kv("gen_ai.usage.completion_tokens", int(8)),
        kv("gen_ai.usage.cache_read_input_tokens", int(1)),
        kv("gen_ai.usage.cache_creation_input_tokens", int(6)),
    ]);
    assert_eq!(legacy.usage.input_tokens, Some(10));
    assert_eq!(legacy.usage.output_tokens, Some(8));
    assert_eq!(legacy.usage.cached_input_tokens, Some(1));
    assert_eq!(legacy.usage.cache_write_tokens, Some(6));
    let modal = one(vec![
        kv("gen_ai.usage.input_tokens", int(10)),
        kv("gen_ai.usage.image.input_tokens", int(4)),
    ]);
    assert_eq!(
        modal.usage.input_tokens,
        Some(10),
        "modality counts are inside the total"
    );
    assert_eq!(
        modal.source_meta["usage_raw"]["gen_ai.usage.image.input_tokens"],
        json!(4)
    );
}

#[test]
fn usage_basis_is_keyed_on_scope_not_provider() {
    let out = read(&[delivery_in(
        "opentelemetry.instrumentation.genai.anthropic",
        vec![span(
            "s1",
            chat(vec![
                kv("gen_ai.provider.name", s("anthropic")),
                kv("gen_ai.usage.input_tokens", int(23)),
                kv("gen_ai.usage.cache_read.input_tokens", int(7)),
            ]),
        )],
    )]);
    let g = &out.generations[0];
    assert_eq!(g.usage.cache_basis, Some(CacheBasis::Inclusive));
    assert_eq!(
        g.usage.input_tokens,
        Some(16),
        "inclusive (the table is empty): the cache read is subtracted"
    );
}

#[test]
fn source_meta_carries_request_params_scope_and_tools_digest() {
    let g = one(vec![
        kv("gen_ai.request.temperature", json!({"doubleValue": 0.5})),
        kv("gen_ai.request.max_tokens", int(100)),
        kv("gen_ai.request.stop_sequences", any(&json!(["\n\n"]))),
        kv("gen_ai.request.reasoning.level", s("high")),
        kv("gen_ai.request.choice.count", int(2)),
        kv("gen_ai.request.stream", json!({"boolValue": false})),
        kv("gen_ai.output.type", s("text")),
        kv("server.address", s("127.0.0.1")),
        kv(
            "gen_ai.response.time_to_first_chunk",
            json!({"doubleValue": 0.25}),
        ),
        json_attr(
            "gen_ai.tool.definitions",
            &json!([{"type": "function", "name": "read_file"}]),
        ),
    ]);
    let m = &g.source_meta;
    assert_eq!(m["operation"], "chat");
    assert_eq!(m["scope"], json!({"name": "test.scope", "version": "1.2"}));
    assert_eq!(
        m["request_params"],
        json!({"temperature": 0.5, "max_tokens": 100,
        "stop_sequences": ["\n\n"], "reasoning_level": "high", "choice_count": 2,
        "stream": false, "output_type": "text"})
    );
    assert_eq!(m["server_address"], "127.0.0.1");
    assert_eq!(m["time_to_first_chunk_s"], 0.25);
    // python3: sha256(json.dumps(defs, sort_keys=True, separators=(",",":")))
    assert_eq!(
        m["tools_digest"],
        "962493ce952704526d24cde7d97dce0619790ddf160a3197b4a9877ec3cc5ff7"
    );
}

#[test]
fn details_span_event_is_read_after_span_attributes() {
    let mut sp = span("s1", chat(vec![kv("gen_ai.request.model", s("on-span"))]));
    sp["events"] = json!([{"timeUnixNano": "1500", "name": "gen_ai.client.inference.operation.details",
        "attributes": [
            kv("gen_ai.request.model", s("on-event")),
            kv("gen_ai.input.messages", any(&json!([{"role": "user", "parts": [{"type": "text", "content": "from event"}]}]))),
        ]},
        {"timeUnixNano": "1600", "name": "other.event",
         "attributes": [kv("gen_ai.response.model", s("ignored"))]}]);
    let out = read(&[delivery(vec![sp])]);
    let g = &out.generations[0];
    assert_eq!(
        g.request_model.as_deref(),
        Some("on-span"),
        "span attributes first"
    );
    assert_eq!(g.messages[0].content, json!("from event"));
    assert_eq!(g.response_model, None, "only the details event is read");
}

#[test]
fn semconv_joins_auto_after_openrouter() {
    let out = read_deliveries(
        &[delivery(vec![span("s1", chat(vec![]))])],
        ProfileSelection::Auto,
    )
    .unwrap();
    assert_eq!(out.generations.len(), 1);
    assert_eq!(out.generations[0].profile, "semconv");
    assert_eq!(out.unclaimed, 0);
}

fn event(name: &str, time: &str, attrs: Vec<Value>) -> Value {
    json!({"timeUnixNano": time, "name": name, "attributes": attrs})
}

#[test]
fn legacy_span_events_equal_the_current_form() {
    let current = one(vec![
        json_attr(
            "gen_ai.input.messages",
            &json!([
                {"role": "system", "parts": [{"type": "text", "content": "S"}]},
                {"role": "user", "parts": [{"type": "text", "content": "hi"}]},
                {"role": "assistant", "parts": [{"type": "tool_call", "id": "c1", "name": "read", "arguments": "{\"p\":1}"}]},
                {"role": "tool", "parts": [{"type": "tool_call_response", "id": "c1", "response": "r"}]}
            ]),
        ),
        json_attr(
            "gen_ai.output.messages",
            &json!([{"role": "assistant", "parts": [{"type": "text", "content": "done"}]}]),
        ),
    ]);
    let mut sp = span("s1", chat(vec![]));
    // Deliberately out of order in the array: ordering is by timeUnixNano.
    sp["events"] = json!([
        event(
            "gen_ai.user.message",
            "20",
            vec![kv("content", s("hi")), kv("role", s("user"))]
        ),
        event(
            "gen_ai.system.message",
            "10",
            vec![kv("content", s("S")), kv("role", s("system"))]
        ),
        event(
            "gen_ai.assistant.message",
            "30",
            vec![kv(
                "body",
                any(&json!({"role": "assistant",
                    "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"p\":1}"}}]}))
            )]
        ),
        event(
            "gen_ai.tool.message",
            "40",
            vec![kv("content", s("r")), kv("id", s("c1"))]
        ),
        event(
            "gen_ai.choice",
            "50",
            vec![kv(
                "body",
                any(&json!({"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "done"}}))
            )]
        ),
        event(
            "gen_ai.choice",
            "51",
            vec![kv(
                "body",
                any(&json!({"index": 1, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "alt"}}))
            )]
        ),
    ]);
    let legacy = read(&[delivery(vec![sp])]).generations.remove(0);
    assert_eq!(legacy.messages, current.messages);
    assert_eq!(legacy.completion.text, current.completion.text);
    assert_eq!(legacy.completion.tool_calls, current.completion.tool_calls);
    assert!(!legacy.absent.prompt && !legacy.absent.completion);
    assert_eq!(legacy.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        legacy.source_meta["choices"][0]["message"]["content"],
        "alt"
    );
}

#[test]
fn legacy_events_are_ignored_when_the_current_form_is_present() {
    let mut sp = span(
        "s1",
        chat(vec![json_attr(
            "gen_ai.input.messages",
            &json!([{"role": "user", "parts": [{"type": "text", "content": "current"}]}]),
        )]),
    );
    sp["events"] = json!([event(
        "gen_ai.user.message",
        "1",
        vec![kv("content", s("legacy"))]
    )]);
    let g = read(&[delivery(vec![sp])]).generations.remove(0);
    assert_eq!(g.messages.len(), 1);
    assert_eq!(g.messages[0].content, json!("current"));
}

fn tool_span(
    span_id: &str,
    call_id: &str,
    result: Value,
    status: Value,
    extra: Vec<Value>,
) -> Value {
    let mut attrs = vec![
        kv("gen_ai.operation.name", s("execute_tool")),
        kv("gen_ai.tool.call.id", s(call_id)),
        kv("gen_ai.tool.call.result", result),
    ];
    attrs.extend(extra);
    json!({"traceId": "t1", "spanId": span_id, "parentSpanId": "s1", "name": "execute_tool read",
           "startTimeUnixNano": "2100", "endTimeUnixNano": "2200", "attributes": attrs, "status": status})
}

#[test]
fn execute_tool_spans_fill_tool_results() {
    let chat_span = span(
        "s1",
        chat(vec![json_attr(
            "gen_ai.output.messages",
            &json!([{"role": "assistant",
                "parts": [{"type": "tool_call", "id": "c1", "name": "read", "arguments": "{}"},
                          {"type": "tool_call", "id": "c2", "name": "read", "arguments": "{}"},
                          {"type": "tool_call", "id": "c3", "name": "read", "arguments": "{}"}]}]),
        )]),
    );
    let out = read(&[delivery(vec![
        chat_span,
        tool_span("t-a", "c1", s("text result"), json!({}), vec![]),
        tool_span(
            "t-b",
            "c2",
            any(&json!({"b": 1, "a": 2})),
            json!({"code": 2}),
            vec![],
        ),
        tool_span(
            "t-c",
            "c3",
            s("ok"),
            json!({}),
            vec![kv("error.type", s("Timeout"))],
        ),
    ])]);
    assert_eq!(out.unclaimed, 0, "execute_tool spans are absorbed");
    let r = &out.generations[0].tool_results;
    assert_eq!(
        (r["c1"].content.as_str(), r["c1"].is_error),
        ("text result", false)
    );
    assert_eq!(
        (r["c2"].content.as_str(), r["c2"].is_error),
        ("{\"a\":2,\"b\":1}", true)
    );
    assert!(r["c3"].is_error, "error.type marks an error");
}

#[test]
fn a_redelivered_execute_tool_span_is_read_once() {
    let chat_span = span(
        "s1",
        chat(vec![json_attr(
            "gen_ai.output.messages",
            &json!([{"role": "assistant",
                "parts": [{"type": "tool_call", "id": "c1", "name": "read", "arguments": "{}"}]}]),
        )]),
    );
    let first = tool_span("t-a", "c1", s("first"), json!({}), vec![]);
    let again = tool_span("t-a", "c1", s("second copy"), json!({}), vec![]);
    let out = read(&[delivery(vec![chat_span, first]), delivery(vec![again])]);
    assert_eq!(out.generations[0].tool_results["c1"].content, "first");
    assert_eq!(out.unclaimed, 0);
}

/// A chat span in trace t1 with its own response id and start time whose
/// completion calls `call_id` (Gemini-style synthesized ids repeat per turn).
fn turn(span_id: &str, response_id: &str, start: u64, call_id: &str) -> Value {
    let mut sp = span(
        span_id,
        vec![
            kv("gen_ai.operation.name", s("chat")),
            kv("gen_ai.response.id", s(response_id)),
            json_attr(
                "gen_ai.output.messages",
                &json!([{"role": "assistant",
                    "parts": [{"type": "tool_call", "id": call_id, "name": "read_file", "arguments": "{}"}]}]),
            ),
        ],
    );
    sp["startTimeUnixNano"] = json!(start.to_string());
    sp["endTimeUnixNano"] = json!((start + 100).to_string());
    sp
}

fn tool_span_at(span_id: &str, call_id: &str, result: &str, start: u64) -> Value {
    let mut t = tool_span(span_id, call_id, s(result), json!({}), vec![]);
    t["startTimeUnixNano"] = json!(start.to_string());
    t["endTimeUnixNano"] = json!((start + 10).to_string());
    t
}

#[test]
fn repeated_synthesized_call_ids_take_each_turns_own_result() {
    // Input order deliberately puts the second turn's tool span first.
    let out = read(&[delivery(vec![
        tool_span_at("x-2", "read_file_0", "second", 4200),
        turn("s1", "resp-1", 1000, "read_file_0"),
        tool_span_at("x-1", "read_file_0", "first", 1200),
        turn("s2", "resp-2", 3000, "read_file_0"),
    ])]);
    let by_id = |id: &str| {
        let g = out.generations.iter().find(|g| g.id == id).unwrap();
        g.tool_results["read_file_0"].content.clone()
    };
    assert_eq!(by_id("resp-1"), "first");
    assert_eq!(by_id("resp-2"), "second");
}

#[test]
fn a_result_after_a_later_turn_reusing_the_id_is_not_taken() {
    // Turn 1's own result is missing; the only span with the id belongs to
    // turn 2, which reuses the id, and before turn 1 nothing qualifies.
    let out = read(&[delivery(vec![
        tool_span_at("x-0", "read_file_0", "before", 500),
        turn("s1", "resp-1", 1000, "read_file_0"),
        turn("s2", "resp-2", 3000, "read_file_0"),
        tool_span_at("x-2", "read_file_0", "second", 4200),
    ])]);
    let g1 = out.generations.iter().find(|g| g.id == "resp-1").unwrap();
    assert!(g1.tool_results.is_empty(), "{:?}", g1.tool_results);
    let g2 = out.generations.iter().find(|g| g.id == "resp-2").unwrap();
    assert_eq!(g2.tool_results["read_file_0"].content, "second");
}

#[test]
fn a_mixed_message_splits_in_source_part_order() {
    // An id-less result before the user's next text: the tool message must
    // come first, as in the source, so positional pairing can attach it.
    let g = one(vec![json_attr(
        "gen_ai.input.messages",
        &json!([
            {"role": "assistant", "parts": [{"type": "tool_call", "name": "read", "arguments": "{}"}]},
            {"role": "user", "parts": [
                {"type": "tool_call_response", "response": "r"},
                {"type": "text", "content": "next"}
            ]}
        ]),
    )]);
    let shape: Vec<(&str, Value)> = g
        .messages
        .iter()
        .map(|m| (m.role.as_str(), m.content.clone()))
        .collect();
    assert_eq!(
        shape,
        [
            ("assistant", Value::Null),
            ("tool", json!("r")),
            ("user", json!("next"))
        ]
    );
    assert_eq!(g.messages[1].tool_call_id.as_deref(), Some(""));
}

/// End to end (semconv → group → stitch): a user message whose parts are
/// `[tool_call_response (id-less), text]` splits in source part order, so the
/// id-less result directly follows the assistant's id-less call and pairs
/// with it positionally. Stitch-level counterpart for an intervening user
/// turn: `user_turn_between_a_call_and_its_results` in src/stitch.rs.
#[test]
fn mixed_message_idless_result_before_text_pairs_end_to_end() {
    let at = |span_id: &str, start: u64, attrs: Vec<Value>| {
        let mut sp = span(span_id, attrs);
        sp["startTimeUnixNano"] = json!(start.to_string());
        sp["endTimeUnixNano"] = json!((start + 500).to_string());
        sp
    };
    let call = json!({"type": "tool_call", "name": "Bash", "arguments": "{}"});
    let go = json!({"role": "user", "parts": [{"type": "text", "content": "go"}]});
    let g1 = at(
        "s1",
        1000,
        vec![
            kv("gen_ai.operation.name", s("chat")),
            kv("gen_ai.conversation.id", s("conv")),
            json_attr("gen_ai.input.messages", &json!([go])),
            json_attr(
                "gen_ai.output.messages",
                &json!([{"role": "assistant", "parts": [call]}]),
            ),
        ],
    );
    let g2 = at(
        "s2",
        2000,
        vec![
            kv("gen_ai.operation.name", s("chat")),
            kv("gen_ai.conversation.id", s("conv")),
            json_attr(
                "gen_ai.input.messages",
                &json!([
                    go,
                    {"role": "assistant", "parts": [call]},
                    {"role": "user", "parts": [
                        {"type": "tool_call_response", "response": "r"},
                        {"type": "text", "content": "note"}
                    ]}
                ]),
            ),
            json_attr(
                "gen_ai.output.messages",
                &json!([{"role": "assistant", "parts": [{"type": "text", "content": "ok"}]}]),
            ),
        ],
    );
    let out = read(&[delivery(vec![g1, g2])]);
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    let roles: Vec<&str> = out.generations[1]
        .messages
        .iter()
        .map(|m| m.role.as_str())
        .collect();
    assert_eq!(
        roles,
        ["user", "assistant", "tool", "user"],
        "source part order"
    );
    let sessions = crate::tests::otel::group_sessions(out.generations);
    assert_eq!(sessions.len(), 1);
    let g = crate::tests::otel::stitch(&sessions[0]);
    let a = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
    let id = a.message.tool_calls[0].id.clone();
    assert!(id.ends_with(":0"), "positional id: {id}");
    assert_eq!(a.results[&id].content, "r");
}
