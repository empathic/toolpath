//! OTLP protobuf ⇄ canonical OTLP/JSON (feature `protobuf`).

use crate::entries::{Entries, charge_protobuf};
use crate::error::{OtelError, Result};
use crate::hash::hex;
use crate::input::MAX_ENTRIES;
use crate::proto::opentelemetry::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
use crate::proto::opentelemetry::proto::common::v1 as common;
use crate::proto::opentelemetry::proto::logs::v1 as logs;
use crate::proto::opentelemetry::proto::resource::v1 as resource;
use crate::proto::opentelemetry::proto::trace::v1 as trace;
use common::any_value::Value as Av;
use prost::Message;
use serde_json::{Map, Value, json};

// `MAX_ANY_DEPTH` is sized so every body `encode_protobuf` accepts also
// decodes under prost's recursion limit, which counts message levels.

/// `prost::RECURSION_LIMIT`, private in prost 0.14.
const PROST_RECURSION_LIMIT: usize = 100;

/// Message level of the deepest top-level `AnyValue`: a span event or link
/// attribute.
const ENVELOPE_LEVELS: usize = 6;

/// Message levels one `kvlistValue` step costs (`arrayValue` costs 2).
const KVLIST_STEP_LEVELS: usize = 3;

/// A deepest `kvlistValue` holding a value-less `KeyValue`.
const KVLIST_TAIL_LEVELS: usize = 2;

/// Deepest `AnyValue` nesting `encode_protobuf` accepts (top level is 0).
const MAX_ANY_DEPTH: usize =
    (PROST_RECURSION_LIMIT - ENVELOPE_LEVELS - KVLIST_TAIL_LEVELS) / KVLIST_STEP_LEVELS;

/// Worst-case message level for `AnyValue` nesting `depth`.
const fn deepest_level(depth: usize) -> usize {
    ENVELOPE_LEVELS + KVLIST_STEP_LEVELS * depth + KVLIST_TAIL_LEVELS
}

const _: () = assert!(deepest_level(MAX_ANY_DEPTH) <= PROST_RECURSION_LIMIT);
const _: () = assert!(deepest_level(MAX_ANY_DEPTH + 1) > PROST_RECURSION_LIMIT);

/// Decode one OTLP/HTTP traces or logs request body into canonical OTLP/JSON.
///
/// The two requests are wire-compatible, so traces are tried first and
/// accepted only if every span has a 16-byte trace id and 8-byte span id.
/// A request with no span or log record is the same bytes for both, and
/// reads as traces, its resources and scopes kept.
///
/// # Errors
///
/// [`OtelError::NotOtlpBody`] when the bytes are neither request;
/// [`OtelError::Protobuf`] for a traces request with a bad span id;
/// [`OtelError::TooManyEntries`] for a request of more than 1,048,576
/// entries (see [`DecodeLimits`](crate::DecodeLimits)).
pub fn decode_protobuf(bytes: &[u8]) -> Result<Value> {
    decode_protobuf_within(bytes, &mut Entries::new(MAX_ENTRIES))
}

/// [`decode_protobuf`], charging `entries` before prost builds anything.
pub(crate) fn decode_protobuf_within(bytes: &[u8], entries: &mut Entries) -> Result<Value> {
    charge_protobuf(bytes, entries)?;
    let (traces_err, wire_valid_traces) = match ExportTraceServiceRequest::decode(bytes) {
        Ok(req) => match bad_span(&req) {
            None => return Ok(traces_json(&req)),
            Some(why) => (why, true),
        },
        Err(e) => (e.to_string(), false),
    };
    match ExportLogsServiceRequest::decode(bytes) {
        Ok(req) => Ok(logs_json(&req)),
        Err(e) if wire_valid_traces => Err(OtelError::Protobuf(format!(
            "not an OTLP traces request ({traces_err}); not an OTLP logs request ({e})"
        ))),
        Err(e) => Err(OtelError::NotOtlpBody(format!(
            "not an OTLP traces request ({traces_err}); not an OTLP logs request ({e})"
        ))),
    }
}

/// Encode one OTLP/JSON delivery as an OTLP/HTTP protobuf body. Id lengths
/// are not checked, so tests can build bodies `decode_protobuf` rejects.
///
/// # Errors
///
/// [`OtelError::Protobuf`], naming the JSON path, for any value that is
/// not strict proto3 JSON, or a delivery carrying both signals.
pub fn encode_protobuf(delivery: &Value) -> Result<Vec<u8>> {
    let o = object(delivery, "")?;
    match (field(o, "resourceSpans"), field(o, "resourceLogs")) {
        (Some(_), Some(_)) => Err(OtelError::Protobuf("one request carries one signal".into())),
        (None, Some(_)) => Ok(ExportLogsServiceRequest {
            resource_logs: list(o, "resourceLogs", "", resource_logs)?,
        }
        .encode_to_vec()),
        (Some(_), None) => Ok(ExportTraceServiceRequest {
            resource_spans: list(o, "resourceSpans", "", resource_spans)?,
        }
        .encode_to_vec()),
        (None, None) if o.contains_key("resourceSpans") || o.contains_key("resourceLogs") => {
            Ok(ExportTraceServiceRequest::default().encode_to_vec())
        }
        (None, None) => Err(OtelError::Protobuf(
            "nothing to encode: no resourceSpans or resourceLogs".into(),
        )),
    }
}

fn spans(req: &ExportTraceServiceRequest) -> impl Iterator<Item = &trace::Span> {
    req.resource_spans
        .iter()
        .flat_map(|rs| &rs.scope_spans)
        .flat_map(|ss| &ss.spans)
}

/// Span-name characters quoted in a bad-id error, so a hostile name cannot
/// bloat the message.
const ERROR_NAME_CHARS: usize = 64;

fn bad_span(req: &ExportTraceServiceRequest) -> Option<String> {
    spans(req)
        .find(|s| s.trace_id.len() != 16 || s.span_id.len() != 8)
        .map(|s| {
            format!(
                "span {:?} has a {}-byte trace_id and a {}-byte span_id, OTLP requires 16 and 8",
                s.name.chars().take(ERROR_NAME_CHARS).collect::<String>(),
                s.trace_id.len(),
                s.span_id.len()
            )
        })
}

/// An object builder that leaves out proto3 defaults.
struct Obj(Map<String, Value>);

impl Obj {
    fn new() -> Self {
        Obj(Map::new())
    }
    fn put(mut self, k: &str, v: Value) -> Self {
        self.0.insert(k.to_string(), v);
        self
    }
    fn str(self, k: &str, v: &str) -> Self {
        if v.is_empty() {
            self
        } else {
            self.put(k, Value::from(v))
        }
    }
    fn hex(self, k: &str, v: &[u8]) -> Self {
        if v.is_empty() {
            self
        } else {
            self.put(k, Value::from(hex(v)))
        }
    }
    /// `fixed64`/`uint64`: a decimal string.
    fn u64s(self, k: &str, v: u64) -> Self {
        if v == 0 {
            self
        } else {
            self.put(k, Value::from(v.to_string()))
        }
    }
    fn u32n(self, k: &str, v: u32) -> Self {
        if v == 0 {
            self
        } else {
            self.put(k, Value::from(v))
        }
    }
    fn i32n(self, k: &str, v: i32) -> Self {
        if v == 0 {
            self
        } else {
            self.put(k, Value::from(v))
        }
    }
    fn arr(self, k: &str, v: Vec<Value>) -> Self {
        if v.is_empty() {
            self
        } else {
            self.put(k, Value::Array(v))
        }
    }
    fn opt(self, k: &str, v: Option<Value>) -> Self {
        match v {
            Some(v) => self.put(k, v),
            None => self,
        }
    }
    fn done(self) -> Value {
        Value::Object(self.0)
    }
}

fn traces_json(req: &ExportTraceServiceRequest) -> Value {
    let rs: Vec<Value> = req
        .resource_spans
        .iter()
        .map(|rs| {
            Obj::new()
                .opt("resource", rs.resource.as_ref().map(resource_json))
                .arr(
                    "scopeSpans",
                    rs.scope_spans
                        .iter()
                        .map(|ss| {
                            Obj::new()
                                .opt("scope", ss.scope.as_ref().map(scope_json))
                                .arr("spans", ss.spans.iter().map(span_json).collect())
                                .str("schemaUrl", &ss.schema_url)
                                .done()
                        })
                        .collect(),
                )
                .str("schemaUrl", &rs.schema_url)
                .done()
        })
        .collect();
    json!({ "resourceSpans": rs })
}

fn logs_json(req: &ExportLogsServiceRequest) -> Value {
    let rl: Vec<Value> = req
        .resource_logs
        .iter()
        .map(|rl| {
            Obj::new()
                .opt("resource", rl.resource.as_ref().map(resource_json))
                .arr(
                    "scopeLogs",
                    rl.scope_logs
                        .iter()
                        .map(|sl| {
                            Obj::new()
                                .opt("scope", sl.scope.as_ref().map(scope_json))
                                .arr("logRecords", sl.log_records.iter().map(log_json).collect())
                                .str("schemaUrl", &sl.schema_url)
                                .done()
                        })
                        .collect(),
                )
                .str("schemaUrl", &rl.schema_url)
                .done()
        })
        .collect();
    json!({ "resourceLogs": rl })
}

fn resource_json(r: &resource::Resource) -> Value {
    Obj::new()
        .arr("attributes", kvs_json(&r.attributes))
        .u32n("droppedAttributesCount", r.dropped_attributes_count)
        .arr(
            "entityRefs",
            r.entity_refs
                .iter()
                .map(|e| {
                    Obj::new()
                        .str("schemaUrl", &e.schema_url)
                        .str("type", &e.r#type)
                        .arr(
                            "idKeys",
                            e.id_keys.iter().map(|k| Value::from(k.as_str())).collect(),
                        )
                        .arr(
                            "descriptionKeys",
                            e.description_keys
                                .iter()
                                .map(|k| Value::from(k.as_str()))
                                .collect(),
                        )
                        .done()
                })
                .collect(),
        )
        .done()
}

fn scope_json(s: &common::InstrumentationScope) -> Value {
    Obj::new()
        .str("name", &s.name)
        .str("version", &s.version)
        .arr("attributes", kvs_json(&s.attributes))
        .u32n("droppedAttributesCount", s.dropped_attributes_count)
        .done()
}

fn span_json(s: &trace::Span) -> Value {
    Obj::new()
        .hex("traceId", &s.trace_id)
        .hex("spanId", &s.span_id)
        .str("traceState", &s.trace_state)
        .hex("parentSpanId", &s.parent_span_id)
        .u32n("flags", s.flags)
        .str("name", &s.name)
        .i32n("kind", s.kind)
        .u64s("startTimeUnixNano", s.start_time_unix_nano)
        .u64s("endTimeUnixNano", s.end_time_unix_nano)
        .arr("attributes", kvs_json(&s.attributes))
        .u32n("droppedAttributesCount", s.dropped_attributes_count)
        .arr(
            "events",
            s.events
                .iter()
                .map(|e| {
                    Obj::new()
                        .u64s("timeUnixNano", e.time_unix_nano)
                        .str("name", &e.name)
                        .arr("attributes", kvs_json(&e.attributes))
                        .u32n("droppedAttributesCount", e.dropped_attributes_count)
                        .done()
                })
                .collect(),
        )
        .u32n("droppedEventsCount", s.dropped_events_count)
        .arr(
            "links",
            s.links
                .iter()
                .map(|l| {
                    Obj::new()
                        .hex("traceId", &l.trace_id)
                        .hex("spanId", &l.span_id)
                        .str("traceState", &l.trace_state)
                        .arr("attributes", kvs_json(&l.attributes))
                        .u32n("droppedAttributesCount", l.dropped_attributes_count)
                        .u32n("flags", l.flags)
                        .done()
                })
                .collect(),
        )
        .u32n("droppedLinksCount", s.dropped_links_count)
        .opt(
            "status",
            s.status.as_ref().map(|st| {
                Obj::new()
                    .str("message", &st.message)
                    .i32n("code", st.code)
                    .done()
            }),
        )
        .done()
}

fn log_json(r: &logs::LogRecord) -> Value {
    Obj::new()
        .u64s("timeUnixNano", r.time_unix_nano)
        .u64s("observedTimeUnixNano", r.observed_time_unix_nano)
        .i32n("severityNumber", r.severity_number)
        .str("severityText", &r.severity_text)
        .opt("body", r.body.as_ref().map(any_json))
        .arr("attributes", kvs_json(&r.attributes))
        .u32n("droppedAttributesCount", r.dropped_attributes_count)
        .u32n("flags", r.flags)
        .hex("traceId", &r.trace_id)
        .hex("spanId", &r.span_id)
        .str("eventName", &r.event_name)
        .done()
}

fn kvs_json(kvs: &[common::KeyValue]) -> Vec<Value> {
    kvs.iter()
        .map(|kv| {
            Obj::new()
                .put("key", Value::from(kv.key.as_str()))
                .opt("value", kv.value.as_ref().map(any_json))
                .i32n("keyStrindex", kv.key_strindex)
                .done()
        })
        .collect()
}

fn any_json(v: &common::AnyValue) -> Value {
    match &v.value {
        None => json!({}),
        Some(Av::StringValue(s)) => json!({ "stringValue": s }),
        Some(Av::BoolValue(b)) => json!({ "boolValue": b }),
        Some(Av::IntValue(i)) => json!({ "intValue": i.to_string() }),
        Some(Av::DoubleValue(d)) => json!({ "doubleValue": double_json(*d) }),
        Some(Av::ArrayValue(a)) => {
            json!({ "arrayValue": { "values": a.values.iter().map(any_json).collect::<Vec<_>>() } })
        }
        Some(Av::KvlistValue(l)) => json!({ "kvlistValue": { "values": kvs_json(&l.values) } }),
        Some(Av::BytesValue(b)) => json!({ "bytesValue": base64_encode(b) }),
        Some(Av::StringValueStrindex(i)) => json!({ "stringValueStrindex": i }),
    }
}

/// proto3 JSON spells the non-finite doubles as strings.
fn double_json(d: f64) -> Value {
    if d.is_nan() {
        Value::from("NaN")
    } else if d == f64::INFINITY {
        Value::from("Infinity")
    } else if d == f64::NEG_INFINITY {
        Value::from("-Infinity")
    } else {
        Value::from(d)
    }
}

fn bad(path: &str, what: &str) -> OtelError {
    let at = if path.is_empty() { "/" } else { path };
    OtelError::Protobuf(format!("{at}: {what}"))
}

fn field<'v>(o: &'v Map<String, Value>, k: &str) -> Option<&'v Value> {
    o.get(k).filter(|v| !v.is_null())
}

fn object<'v>(v: &'v Value, path: &str) -> Result<&'v Map<String, Value>> {
    v.as_object().ok_or_else(|| bad(path, "expected an object"))
}

fn list<'v, T>(
    o: &'v Map<String, Value>,
    k: &str,
    path: &str,
    item: impl Fn(&'v Value, &str) -> Result<T>,
) -> Result<Vec<T>> {
    let here = format!("{path}/{k}");
    match field(o, k) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, v)| item(v, &format!("{here}/{i}")))
            .collect(),
        Some(_) => Err(bad(&here, "expected an array")),
    }
}

fn message<'v, T>(
    o: &'v Map<String, Value>,
    k: &str,
    path: &str,
    item: impl Fn(&'v Value, &str) -> Result<T>,
) -> Result<Option<T>> {
    field(o, k)
        .map(|v| item(v, &format!("{path}/{k}")))
        .transpose()
}

fn string(o: &Map<String, Value>, k: &str, path: &str) -> Result<String> {
    match field(o, k) {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(bad(&format!("{path}/{k}"), "expected a string")),
    }
}

fn strings(o: &Map<String, Value>, k: &str, path: &str) -> Result<Vec<String>> {
    list(o, k, path, |v, p| {
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| bad(p, "expected a string"))
    })
}

fn id(o: &Map<String, Value>, k: &str, path: &str) -> Result<Vec<u8>> {
    let here = format!("{path}/{k}");
    let s = string(o, k, path)?;
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(&here, "expected a hex id"));
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("checked hex"))
        .collect())
}

fn int(v: &Value, path: &str) -> Result<i128> {
    match v {
        Value::String(s) => s
            .parse::<i128>()
            .map_err(|_| bad(path, "expected a decimal integer")),
        Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .ok_or_else(|| bad(path, "expected an integer")),
        _ => Err(bad(path, "expected an integer")),
    }
}

fn ranged<T: TryFrom<i128>>(o: &Map<String, Value>, k: &str, path: &str) -> Result<T> {
    let here = format!("{path}/{k}");
    match field(o, k) {
        None => T::try_from(0).map_err(|_| bad(&here, "out of range")),
        Some(v) => T::try_from(int(v, &here)?).map_err(|_| bad(&here, "out of range")),
    }
}

/// An enum given as its number or as its proto name (any case).
fn enumeration(
    o: &Map<String, Value>,
    k: &str,
    path: &str,
    by_name: fn(&str) -> Option<i32>,
) -> Result<i32> {
    let here = format!("{path}/{k}");
    match field(o, k) {
        Some(Value::String(s)) if s.parse::<i32>().is_err() => {
            by_name(&s.to_ascii_uppercase()).ok_or_else(|| bad(&here, "unknown enum name"))
        }
        _ => ranged(o, k, path),
    }
}

fn resource_spans(v: &Value, path: &str) -> Result<trace::ResourceSpans> {
    let o = object(v, path)?;
    Ok(trace::ResourceSpans {
        resource: message(o, "resource", path, resource_msg)?,
        scope_spans: list(o, "scopeSpans", path, |v, p| {
            let o = object(v, p)?;
            Ok(trace::ScopeSpans {
                scope: message(o, "scope", p, scope_msg)?,
                spans: list(o, "spans", p, span_msg)?,
                schema_url: string(o, "schemaUrl", p)?,
            })
        })?,
        schema_url: string(o, "schemaUrl", path)?,
    })
}

fn resource_logs(v: &Value, path: &str) -> Result<logs::ResourceLogs> {
    let o = object(v, path)?;
    Ok(logs::ResourceLogs {
        resource: message(o, "resource", path, resource_msg)?,
        scope_logs: list(o, "scopeLogs", path, |v, p| {
            let o = object(v, p)?;
            Ok(logs::ScopeLogs {
                scope: message(o, "scope", p, scope_msg)?,
                log_records: list(o, "logRecords", p, log_msg)?,
                schema_url: string(o, "schemaUrl", p)?,
            })
        })?,
        schema_url: string(o, "schemaUrl", path)?,
    })
}

fn resource_msg(v: &Value, path: &str) -> Result<resource::Resource> {
    let o = object(v, path)?;
    Ok(resource::Resource {
        attributes: list(o, "attributes", path, |v, p| key_value(v, p, 0))?,
        dropped_attributes_count: ranged(o, "droppedAttributesCount", path)?,
        entity_refs: list(o, "entityRefs", path, |v, p| {
            let o = object(v, p)?;
            Ok(common::EntityRef {
                schema_url: string(o, "schemaUrl", p)?,
                r#type: string(o, "type", p)?,
                id_keys: strings(o, "idKeys", p)?,
                description_keys: strings(o, "descriptionKeys", p)?,
            })
        })?,
    })
}

fn scope_msg(v: &Value, path: &str) -> Result<common::InstrumentationScope> {
    let o = object(v, path)?;
    Ok(common::InstrumentationScope {
        name: string(o, "name", path)?,
        version: string(o, "version", path)?,
        attributes: list(o, "attributes", path, |v, p| key_value(v, p, 0))?,
        dropped_attributes_count: ranged(o, "droppedAttributesCount", path)?,
    })
}

fn span_msg(v: &Value, path: &str) -> Result<trace::Span> {
    let o = object(v, path)?;
    Ok(trace::Span {
        trace_id: id(o, "traceId", path)?,
        span_id: id(o, "spanId", path)?,
        trace_state: string(o, "traceState", path)?,
        parent_span_id: id(o, "parentSpanId", path)?,
        flags: ranged(o, "flags", path)?,
        name: string(o, "name", path)?,
        kind: enumeration(o, "kind", path, |s| {
            trace::span::SpanKind::from_str_name(s).map(|k| k as i32)
        })?,
        start_time_unix_nano: ranged(o, "startTimeUnixNano", path)?,
        end_time_unix_nano: ranged(o, "endTimeUnixNano", path)?,
        attributes: list(o, "attributes", path, |v, p| key_value(v, p, 0))?,
        dropped_attributes_count: ranged(o, "droppedAttributesCount", path)?,
        events: list(o, "events", path, |v, p| {
            let o = object(v, p)?;
            Ok(trace::span::Event {
                time_unix_nano: ranged(o, "timeUnixNano", p)?,
                name: string(o, "name", p)?,
                attributes: list(o, "attributes", p, |v, p| key_value(v, p, 0))?,
                dropped_attributes_count: ranged(o, "droppedAttributesCount", p)?,
            })
        })?,
        dropped_events_count: ranged(o, "droppedEventsCount", path)?,
        links: list(o, "links", path, |v, p| {
            let o = object(v, p)?;
            Ok(trace::span::Link {
                trace_id: id(o, "traceId", p)?,
                span_id: id(o, "spanId", p)?,
                trace_state: string(o, "traceState", p)?,
                attributes: list(o, "attributes", p, |v, p| key_value(v, p, 0))?,
                dropped_attributes_count: ranged(o, "droppedAttributesCount", p)?,
                flags: ranged(o, "flags", p)?,
            })
        })?,
        dropped_links_count: ranged(o, "droppedLinksCount", path)?,
        status: message(o, "status", path, |v, p| {
            let o = object(v, p)?;
            Ok(trace::Status {
                message: string(o, "message", p)?,
                code: enumeration(o, "code", p, |s| {
                    trace::status::StatusCode::from_str_name(s).map(|c| c as i32)
                })?,
            })
        })?,
    })
}

fn log_msg(v: &Value, path: &str) -> Result<logs::LogRecord> {
    let o = object(v, path)?;
    Ok(logs::LogRecord {
        time_unix_nano: ranged(o, "timeUnixNano", path)?,
        observed_time_unix_nano: ranged(o, "observedTimeUnixNano", path)?,
        severity_number: enumeration(o, "severityNumber", path, |s| {
            logs::SeverityNumber::from_str_name(s).map(|n| n as i32)
        })?,
        severity_text: string(o, "severityText", path)?,
        body: message(o, "body", path, |v, p| any_value(v, p, 0))?,
        attributes: list(o, "attributes", path, |v, p| key_value(v, p, 0))?,
        dropped_attributes_count: ranged(o, "droppedAttributesCount", path)?,
        flags: ranged(o, "flags", path)?,
        trace_id: id(o, "traceId", path)?,
        span_id: id(o, "spanId", path)?,
        event_name: string(o, "eventName", path)?,
    })
}

fn key_value(v: &Value, path: &str, depth: usize) -> Result<common::KeyValue> {
    let o = object(v, path)?;
    Ok(common::KeyValue {
        key: string(o, "key", path)?,
        value: message(o, "value", path, |v, p| any_value(v, p, depth))?,
        key_strindex: ranged(o, "keyStrindex", path)?,
    })
}

fn any_value(v: &Value, path: &str, depth: usize) -> Result<common::AnyValue> {
    if depth > MAX_ANY_DEPTH {
        return Err(bad(
            path,
            &format!("AnyValue nested deeper than {MAX_ANY_DEPTH} levels"),
        ));
    }
    let o = object(v, path)?;
    let mut found: Option<Av> = None;
    for (k, x) in o {
        if x.is_null() {
            continue;
        }
        let here = format!("{path}/{k}");
        let value = match k.as_str() {
            "stringValue" => Av::StringValue(
                x.as_str()
                    .ok_or_else(|| bad(&here, "expected a string"))?
                    .to_string(),
            ),
            "boolValue" => Av::BoolValue(x.as_bool().ok_or_else(|| bad(&here, "expected a bool"))?),
            "intValue" => {
                Av::IntValue(i64::try_from(int(x, &here)?).map_err(|_| bad(&here, "out of range"))?)
            }
            "doubleValue" => Av::DoubleValue(match x {
                Value::Number(n) => n.as_f64().ok_or_else(|| bad(&here, "expected a number"))?,
                Value::String(s) if s == "NaN" => f64::NAN,
                Value::String(s) if s == "Infinity" => f64::INFINITY,
                Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
                Value::String(s) => s.parse().map_err(|_| bad(&here, "expected a number"))?,
                _ => return Err(bad(&here, "expected a number")),
            }),
            "arrayValue" => Av::ArrayValue(common::ArrayValue {
                values: list(object(x, &here)?, "values", &here, |v, p| {
                    any_value(v, p, depth + 1)
                })?,
            }),
            "kvlistValue" => Av::KvlistValue(common::KeyValueList {
                values: list(object(x, &here)?, "values", &here, |v, p| {
                    key_value(v, p, depth + 1)
                })?,
            }),
            "bytesValue" => Av::BytesValue(base64_decode(
                x.as_str()
                    .ok_or_else(|| bad(&here, "expected a base64 string"))?,
                &here,
            )?),
            "stringValueStrindex" => Av::StringValueStrindex(
                i32::try_from(int(x, &here)?).map_err(|_| bad(&here, "out of range"))?,
            ),
            _ => continue,
        };
        if found.replace(value).is_some() {
            return Err(bad(path, "AnyValue carries more than one value"));
        }
    }
    Ok(common::AnyValue { value: found })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard alphabet, padded (proto3 JSON `bytes`).
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard or URL-safe alphabet, padding optional (proto3 JSON accepts all four).
fn base64_decode(s: &str, path: &str) -> Result<Vec<u8>> {
    let digits: Vec<u32> = s
        .trim_end_matches('=')
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' => Ok(u32::from(b - b'A')),
            b'a'..=b'z' => Ok(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Ok(u32::from(b - b'0') + 52),
            b'+' | b'-' => Ok(62),
            b'/' | b'_' => Ok(63),
            _ => Err(bad(path, "expected base64")),
        })
        .collect::<Result<_>>()?;
    if digits.len() % 4 == 1 {
        return Err(bad(path, "expected base64"));
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, d)| n | d << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE_ID: &str = "0102030405060708090a0b0c0d0e0f10";
    const SPAN_ID: &str = "a1a2a3a4a5a6a7a8";

    fn arrays(depth: usize) -> Value {
        let mut v = json!({"arrayValue": {"values": []}});
        for _ in 0..depth {
            v = json!({"arrayValue": {"values": [v]}});
        }
        v
    }

    /// Ends in the worst-case leaf: a kvlist holding a value-less `KeyValue`.
    fn kvlists(depth: usize) -> Value {
        let mut v = json!({"kvlistValue": {"values": [{"key": "k"}]}});
        for _ in 0..depth {
            v = json!({"kvlistValue": {"values": [{"key": "k", "value": v}]}});
        }
        v
    }

    /// The value as a span-event attribute: the deepest envelope.
    fn traces(v: Value) -> Value {
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
            "traceId": TRACE_ID, "spanId": SPAN_ID,
            "events": [{"attributes": [{"key": "a", "value": v}]}]}]}]}]})
    }

    fn logs(v: Value) -> Value {
        json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{
            "attributes": [{"key": "a", "value": v}]}]}]}]})
    }

    #[test]
    fn the_depth_bound_is_derived_from_prosts_limit() {
        assert_eq!(MAX_ANY_DEPTH, 30);
    }

    #[test]
    fn the_deepest_accepted_value_decodes_in_both_envelopes() {
        for nest in [arrays as fn(usize) -> Value, kvlists] {
            for wrap in [traces as fn(Value) -> Value, logs] {
                let d = wrap(nest(MAX_ANY_DEPTH));
                let bytes = encode_protobuf(&d).unwrap();
                assert_eq!(decode_protobuf(&bytes).unwrap(), d);
            }
        }
    }

    #[test]
    fn one_level_deeper_is_refused_by_encode() {
        for nest in [arrays as fn(usize) -> Value, kvlists] {
            for wrap in [traces as fn(Value) -> Value, logs] {
                let err = encode_protobuf(&wrap(nest(MAX_ANY_DEPTH + 1)))
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("nested deeper than"), "{err}");
                assert!(err.contains(&MAX_ANY_DEPTH.to_string()), "{err}");
            }
        }
    }
}
