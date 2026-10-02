//! semconv over the logs signal: the details record and the v1.36 per-role
//! events as log records give the same generation as span content;
//! orphans form their own units; log records win over legacy span events.

use crate::tests::otel::{Generation, ProfileSelection, ReadOutcome, read_deliveries};
use serde_json::{Map, Value, json};

const T: &str = "0102030405060708090a0b0c0d0e0f10";
const S: &str = "a1a2a3a4a5a6a7a8";

/// Plain JSON → OTLP `AnyValue` JSON (structured form, as on log records).
fn to_any(v: &Value) -> Value {
    match v {
        Value::String(s) => json!({"stringValue": s}),
        Value::Bool(b) => json!({"boolValue": b}),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!({"intValue": n.to_string()}),
        Value::Number(n) => json!({"doubleValue": n.as_f64()}),
        Value::Array(a) => {
            json!({"arrayValue": {"values": a.iter().map(to_any).collect::<Vec<_>>()}})
        }
        Value::Object(o) => json!({"kvlistValue": {"values": o.iter()
            .map(|(k, v)| json!({"key": k, "value": to_any(v)})).collect::<Vec<_>>()}}),
        Value::Null => json!({}),
    }
}

fn kv(key: &str, value: Value) -> Value {
    json!({"key": key, "value": value})
}

fn s(v: &str) -> Value {
    json!({"stringValue": v})
}

fn input() -> Value {
    json!([
        {"role": "user", "parts": [{"type": "text", "content": "weather?"}]},
        {"role": "assistant", "parts": [{"type": "tool_call", "id": "call_1",
            "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}]},
        {"role": "tool", "parts": [{"type": "tool_call_response", "id": "call_1", "response": "sunny"}]}
    ])
}

fn output() -> Value {
    json!([{"role": "assistant", "parts": [{"type": "text", "content": "It is sunny."}],
            "finish_reason": "stop"}])
}

fn base_attrs() -> Vec<Value> {
    vec![
        kv("gen_ai.operation.name", s("chat")),
        kv("gen_ai.request.model", s("m")),
        kv("gen_ai.response.id", s("resp-1")),
    ]
}

fn spans(attributes: Vec<Value>, events: Vec<Value>) -> Value {
    json!({"resourceSpans": [{
        "resource": {"attributes": [kv("service.name", s("app"))]},
        "scopeSpans": [{"scope": {"name": "test.scope"}, "spans": [{
            "traceId": T, "spanId": S, "name": "chat m", "kind": 3,
            "startTimeUnixNano": "100", "endTimeUnixNano": "200",
            "attributes": attributes, "events": events}]}]}]})
}

fn logs(records: Vec<Value>) -> Value {
    json!({"resourceLogs": [{
        "resource": {"attributes": [kv("service.name", s("app"))]},
        "scopeLogs": [{"scope": {"name": "test.scope"}, "logRecords": records}]}]})
}

fn record(event: &str, time: u64, attributes: Vec<Value>, body: Option<Value>) -> Value {
    let mut r = json!({"timeUnixNano": time.to_string(), "traceId": T, "spanId": S,
                       "eventName": event, "attributes": attributes});
    if let Some(b) = body {
        r["body"] = to_any(&b);
    }
    r
}

/// A legacy per-role event on a span: the whole body as one
/// kvlist-valued `body` attribute.
fn legacy_span_event(name: &str, time: u64, body: &Value) -> Value {
    json!({"timeUnixNano": time.to_string(), "name": name, "attributes": [kv("body", to_any(body))]})
}

fn read(values: &[Value]) -> ReadOutcome {
    read_deliveries(values.iter(), ProfileSelection::Semconv).unwrap()
}

/// The fields a content source decides; `source_meta` and `profile` are
/// compared elsewhere.
fn content(g: &Generation) -> Value {
    json!({
        "id": g.id, "session_id": g.session_id, "trace_id": g.trace_id,
        "start_ns": g.start_ns, "end_ns": g.end_ns, "messages": g.messages,
        "completion": g.completion, "finish_reason": g.finish_reason,
        "request_model": g.request_model, "client_key": g.client_key,
    })
}

fn only(out: &ReadOutcome) -> &Generation {
    assert_eq!(out.generations.len(), 1, "skipped: {:?}", out.skipped);
    &out.generations[0]
}

fn span_content() -> Value {
    let mut attrs = base_attrs();
    attrs.push(kv("gen_ai.input.messages", s(&input().to_string())));
    attrs.push(kv("gen_ai.output.messages", s(&output().to_string())));
    content(only(&read(&[spans(attrs, vec![])])))
}

fn legacy_bodies() -> Vec<(&'static str, Value)> {
    vec![
        (
            "gen_ai.user.message",
            json!({"role": "user", "content": "weather?"}),
        ),
        (
            "gen_ai.assistant.message",
            json!({"role": "assistant", "tool_calls": [
            {"id": "call_1", "type": "function",
             "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]}),
        ),
        (
            "gen_ai.tool.message",
            json!({"role": "tool", "id": "call_1", "content": "sunny"}),
        ),
        (
            "gen_ai.choice",
            json!({"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "It is sunny."}}),
        ),
    ]
}

fn legacy_records(bodies: &[(&str, Value)]) -> Vec<Value> {
    bodies
        .iter()
        .enumerate()
        .map(|(i, (name, body))| record(name, 10 + i as u64, vec![], Some(body.clone())))
        .collect()
}

#[test]
fn details_record_attributes_equal_span_content() {
    let details = record(
        "gen_ai.client.inference.operation.details",
        150,
        vec![
            kv("gen_ai.input.messages", to_any(&input())),
            kv("gen_ai.output.messages", to_any(&output())),
        ],
        None,
    );
    // Logs first: file order must not matter.
    let out = read(&[logs(vec![details]), spans(base_attrs(), vec![])]);
    assert_eq!(content(only(&out)), span_content());
    assert_eq!(out.unclaimed, 0);
}

#[test]
fn details_record_body_equals_span_content() {
    let mut body = Map::new();
    body.insert("gen_ai.input.messages".into(), input());
    body.insert("gen_ai.output.messages".into(), output());
    let details = record(
        "gen_ai.client.inference.operation.details",
        150,
        vec![],
        Some(Value::Object(body)),
    );
    let out = read(&[spans(base_attrs(), vec![]), logs(vec![details])]);
    assert_eq!(content(only(&out)), span_content());
}

#[test]
fn span_attributes_win_over_the_details_record() {
    let mut attrs = base_attrs();
    attrs.push(kv("gen_ai.input.messages", s(&input().to_string())));
    attrs.push(kv("gen_ai.output.messages", s(&output().to_string())));
    let other = json!([{"role": "assistant", "parts": [{"type": "text", "content": "OTHER"}]}]);
    let details = record(
        "gen_ai.client.inference.operation.details",
        150,
        vec![kv("gen_ai.output.messages", to_any(&other))],
        None,
    );
    let out = read(&[spans(attrs, vec![]), logs(vec![details])]);
    assert_eq!(content(only(&out)), span_content());
}

#[test]
fn legacy_log_records_equal_span_content() {
    let out = read(&[
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
    ]);
    let g = content(only(&out));
    let want = span_content();
    for k in ["id", "messages", "completion", "finish_reason"] {
        assert_eq!(g[k], want[k], "{k}");
    }
}

#[test]
fn legacy_flat_tool_calls_equal_nested_ones() {
    let mut flat = legacy_bodies();
    flat[1].1 = json!({"role": "assistant", "tool_calls": [
        {"id": "call_1", "type": "function", "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}]});
    let nested = read(&[
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
    ]);
    let flat = read(&[spans(base_attrs(), vec![]), logs(legacy_records(&flat))]);
    assert_eq!(content(only(&flat)), content(only(&nested)));
}

#[test]
fn legacy_events_on_span_and_logs_are_read_once_logs_winning() {
    let stale: Vec<Value> = legacy_bodies()
        .iter()
        .enumerate()
        .map(|(i, (name, body))| {
            let mut b = body.clone();
            if let Some(c) = b.get_mut("content") {
                *c = json!("STALE");
            }
            legacy_span_event(name, 10 + i as u64, &b)
        })
        .collect();
    // Control: span events alone are read, so the test is not vacuous.
    let events_only = read(&[spans(base_attrs(), stale.clone())]);
    assert!(
        serde_json::to_string(&only(&events_only).messages)
            .unwrap()
            .contains("STALE")
    );
    let both = read(&[
        spans(base_attrs(), stale),
        logs(legacy_records(&legacy_bodies())),
    ]);
    let logs_only = read(&[
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
    ]);
    assert_eq!(content(only(&both)), content(only(&logs_only)));
}

#[test]
fn the_same_records_in_two_encodings_are_read_once() {
    let once = read(&[
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
    ]);
    // The same records as protobuf decoding writes them: ids uppercase in
    // the source here, integers as strings (to_any already writes them so),
    // times as strings.
    let mut again = logs(legacy_records(&legacy_bodies()));
    for r in again["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
        .as_array_mut()
        .unwrap()
    {
        r["traceId"] = json!(T.to_uppercase());
        r["spanId"] = json!(S.to_uppercase());
    }
    let twice = read(&[
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
        again,
    ]);
    assert_eq!(content(only(&twice)), content(only(&once)));
}

#[test]
fn an_orphan_details_record_is_its_own_generation() {
    let mut attrs = vec![
        kv("gen_ai.response.id", s("resp-9")),
        kv("gen_ai.request.model", s("m")),
    ];
    attrs.push(kv("gen_ai.input.messages", to_any(&input())));
    attrs.push(kv("gen_ai.output.messages", to_any(&output())));
    let mut details = record("gen_ai.client.inference.operation.details", 50, attrs, None);
    details["traceId"] = json!(T.to_uppercase());
    let out = read(&[logs(vec![details])]);
    let g = only(&out);
    assert_eq!(g.id, "resp-9");
    assert_eq!(g.trace_id, T);
    assert_eq!((g.start_ns, g.end_ns), (50, 50));
    assert_eq!(g.client_key.as_deref(), Some("app"));
    assert_eq!(content(g)["messages"], span_content()["messages"]);
    assert_eq!(content(g)["completion"], span_content()["completion"]);
}

#[test]
fn an_orphan_without_a_response_id_is_keyed_by_its_record_ids() {
    let mut details = record(
        "gen_ai.client.inference.operation.details",
        50,
        vec![
            kv("gen_ai.input.messages", to_any(&input())),
            kv("gen_ai.output.messages", to_any(&output())),
        ],
        None,
    );
    details["spanId"] = json!(S.to_uppercase());
    let out = read(&[logs(vec![details])]);
    assert_eq!(only(&out).id, format!("log-{T}-{S}"));
}

#[test]
fn an_orphan_legacy_group_is_one_generation_spanning_its_records() {
    let mut records = legacy_records(&legacy_bodies());
    records[3]["timeUnixNano"] = json!("0");
    records[3]["observedTimeUnixNano"] = json!("99");
    let out = read(&[logs(records)]);
    let g = only(&out);
    assert_eq!(g.id, format!("log-{T}-{S}"));
    assert_eq!((g.start_ns, g.end_ns), (10, 99));
    let want = span_content();
    assert_eq!(content(g)["messages"], want["messages"]);
    assert_eq!(content(g)["completion"], want["completion"]);
}

#[test]
fn an_id_less_orphan_without_a_response_id_is_missing_payload() {
    let mut r = record(
        "gen_ai.user.message",
        1,
        vec![],
        Some(json!({"content": "hi"})),
    );
    r["traceId"] = json!("");
    r["spanId"] = json!("");
    let out = read(&[logs(vec![r])]);
    assert!(out.generations.is_empty());
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(format!("{:?}", out.skipped[0].reason), "MissingPayload");
}

#[test]
fn other_events_stay_unclaimed() {
    let out = read(&[logs(vec![
        record(
            "gen_ai.evaluation.result",
            1,
            vec![],
            Some(json!({"score": 1})),
        ),
        record("app.custom", 2, vec![], Some(json!({"x": 1}))),
    ])]);
    assert!(out.generations.is_empty());
    assert_eq!(out.unclaimed, 2);
}

#[test]
fn auto_reads_logs_like_named_semconv() {
    let values = [
        spans(base_attrs(), vec![]),
        logs(legacy_records(&legacy_bodies())),
    ];
    let auto = read_deliveries(values.iter(), ProfileSelection::Auto).unwrap();
    assert_eq!(content(only(&auto)), content(only(&read(&values))));
}

/// Event mode: a chat span carrying no content, its completion (calling
/// `call_id`) only in a correlated details log record.
fn event_turn(span_id: &str, response_id: &str, start: u64, call_id: &str) -> (Value, Value) {
    let span = json!({"traceId": T, "spanId": span_id, "name": "chat m", "kind": 3,
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 100).to_string(),
        "attributes": [kv("gen_ai.operation.name", s("chat")),
                       kv("gen_ai.response.id", s(response_id))]});
    let out = json!([{"role": "assistant", "parts": [{"type": "tool_call", "id": call_id,
        "name": "read_file", "arguments": "{}"}]}]);
    let mut details = record(
        "gen_ai.client.inference.operation.details",
        start + 50,
        vec![kv("gen_ai.output.messages", to_any(&out))],
        None,
    );
    details["spanId"] = json!(span_id);
    (span, details)
}

fn event_tool_span(span_id: &str, parent: &str, call_id: &str, result: &str, start: u64) -> Value {
    json!({"traceId": T, "spanId": span_id, "parentSpanId": parent, "name": "execute_tool read",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 10).to_string(),
        "attributes": [kv("gen_ai.operation.name", s("execute_tool")),
                       kv("gen_ai.tool.call.id", s(call_id)),
                       kv("gen_ai.tool.call.result", s(result))]})
}

fn span_delivery(spans: Vec<Value>) -> Value {
    json!({"resourceSpans": [{
        "resource": {"attributes": [kv("service.name", s("app"))]},
        "scopeSpans": [{"scope": {"name": "test.scope"}, "spans": spans}]}]})
}

fn result_of<'a>(out: &'a ReadOutcome, id: &str) -> Option<&'a str> {
    let g = out.generations.iter().find(|g| g.id == id).unwrap();
    g.tool_results
        .get("read_file_0")
        .map(|r| r.content.as_str())
}

#[test]
fn event_mode_repeated_synthesized_ids_take_each_turns_own_result() {
    let (s1, d1) = event_turn("b1b2b3b4b5b6b7b1", "resp-1", 1000, "read_file_0");
    let (s2, d2) = event_turn("b1b2b3b4b5b6b7b2", "resp-2", 3000, "read_file_0");
    let out = read(&[
        span_delivery(vec![
            event_tool_span(
                "c1c2c3c4c5c6c7c2",
                "b1b2b3b4b5b6b7b2",
                "read_file_0",
                "second",
                4200,
            ),
            s1,
            event_tool_span(
                "c1c2c3c4c5c6c7c1",
                "b1b2b3b4b5b6b7b1",
                "read_file_0",
                "first",
                1200,
            ),
            s2,
        ]),
        logs(vec![d1, d2]),
    ]);
    assert_eq!(out.generations.len(), 2, "skipped: {:?}", out.skipped);
    assert_eq!(result_of(&out, "resp-1"), Some("first"));
    assert_eq!(result_of(&out, "resp-2"), Some("second"));
}

#[test]
fn event_mode_a_result_after_a_later_turn_reusing_the_id_is_not_taken() {
    // Turn 2 produces `read_file_0` only in its log record; turn 1 must
    // still see that and leave turn 2's result alone.
    let (s1, d1) = event_turn("b1b2b3b4b5b6b7b1", "resp-1", 1000, "read_file_0");
    let (s2, d2) = event_turn("b1b2b3b4b5b6b7b2", "resp-2", 3000, "read_file_0");
    let out = read(&[
        span_delivery(vec![
            s1,
            s2,
            event_tool_span(
                "c1c2c3c4c5c6c7c2",
                "b1b2b3b4b5b6b7b2",
                "read_file_0",
                "second",
                4200,
            ),
        ]),
        logs(vec![d1, d2]),
    ]);
    assert_eq!(out.generations.len(), 2, "skipped: {:?}", out.skipped);
    assert_eq!(result_of(&out, "resp-1"), None);
    assert_eq!(result_of(&out, "resp-2"), Some("second"));
}

#[test]
fn a_later_span_without_a_span_id_claims_no_id_less_record() {
    // A later inference span with an empty spanId must not read the
    // trace's id-less records as its own: an id-less record calling
    // `read_file_0` would otherwise close turn 1's result.
    let (s1, d1) = event_turn("b1b2b3b4b5b6b7b1", "resp-1", 1000, "read_file_0");
    let (mut s2, mut d2) = event_turn("", "resp-2", 1100, "read_file_0");
    s2["spanId"] = json!("");
    d2.as_object_mut().unwrap().remove("spanId");
    let out = read(&[
        span_delivery(vec![
            s1,
            s2,
            event_tool_span(
                "c1c2c3c4c5c6c7c1",
                "b1b2b3b4b5b6b7b1",
                "read_file_0",
                "first",
                1200,
            ),
        ]),
        logs(vec![d1, d2]),
    ]);
    assert_eq!(
        result_of(&out, "resp-1"),
        Some("first"),
        "{:?}",
        out.skipped
    );
}
