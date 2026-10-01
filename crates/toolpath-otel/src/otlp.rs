//! Lenient OTLP/JSON types, borrowed from the parsed request bodies so no
//! payload is copied: `null` (proto3's default) or a wrong-typed field reads
//! as the default and an unreadable list element is dropped, so one odd span
//! never costs its neighbours. The span status is the exception: an
//! unreadable status fails closed ([`Status::is_error`]).
//!
//! The readers follow serde's derive over a `Value`: a struct reads from an
//! object by field name or from an array by field position (no longer than
//! the field list); anything else does not read.

use serde_json::{Map, Value};
use std::borrow::Cow;

static NULL: Value = Value::Null;

/// A struct's fields, by name from an object or by position from an array;
/// `None` when the value does not read as a struct.
fn fields<'a, const N: usize>(v: &'a Value, names: [&str; N]) -> Option<[Option<&'a Value>; N]> {
    match v {
        Value::Object(m) => Some(names.map(|n| m.get(n))),
        Value::Array(items) if items.len() <= N => {
            let mut out = [None; N];
            for (slot, item) in out.iter_mut().zip(items) {
                *slot = Some(item);
            }
            Some(out)
        }
        _ => None,
    }
}

/// A string field; anything else reads as empty.
fn string(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

/// A hex id field (`traceId`, `spanId`, `parentSpanId`), lowercased:
/// OTLP/JSON hex ids are case-insensitive, so every comparison, key and
/// derived id downstream sees one spelling. Borrowed unless it had to change.
fn hex_id(v: Option<&Value>) -> Cow<'_, str> {
    let s = string(v);
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(s.to_ascii_lowercase())
    } else {
        Cow::Borrowed(s)
    }
}

/// A struct field; missing, `null` or unreadable reads as the default.
fn lenient<'a, T: Default>(v: Option<&'a Value>, read: fn(&'a Value) -> Option<T>) -> T {
    v.and_then(read).unwrap_or_default()
}

/// A list whose unreadable elements are dropped; a non-list reads as empty.
fn lenient_list<'a, T>(v: Option<&'a Value>, read: fn(&'a Value) -> Option<T>) -> Vec<T> {
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(read).collect(),
        _ => Vec::new(),
    }
}

/// A JSON object holding `resourceSpans`, `resourceLogs` or
/// `resourceMetrics` as an array or `null`.
pub fn is_otlp(v: &Value) -> bool {
    ["resourceSpans", "resourceLogs", "resourceMetrics"]
        .iter()
        .any(|k| matches!(v.get(*k), Some(Value::Array(_) | Value::Null)))
}

#[derive(Debug, Default)]
pub struct Delivery<'a> {
    pub resource_spans: Vec<ResourceSpans<'a>>,
    pub resource_logs: Vec<ResourceLogs<'a>>,
}

impl<'a> Delivery<'a> {
    /// Never fails: an unreadable body reads as empty.
    pub fn read(v: &'a Value) -> Self {
        Self::try_read(v).unwrap_or_default()
    }

    fn try_read(v: &'a Value) -> Option<Self> {
        let [spans, logs] = fields(v, ["resourceSpans", "resourceLogs"])?;
        Some(Delivery {
            resource_spans: lenient_list(spans, ResourceSpans::read),
            resource_logs: lenient_list(logs, ResourceLogs::read),
        })
    }
}

#[derive(Debug, Default)]
pub struct ResourceSpans<'a> {
    pub resource: Resource<'a>,
    pub scope_spans: Vec<ScopeSpans<'a>>,
}

impl<'a> ResourceSpans<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [resource, scope_spans] = fields(v, ["resource", "scopeSpans"])?;
        Some(ResourceSpans {
            resource: lenient(resource, Resource::read),
            scope_spans: lenient_list(scope_spans, ScopeSpans::read),
        })
    }
}

#[derive(Debug, Default)]
pub struct ScopeSpans<'a> {
    pub scope: Scope<'a>,
    pub spans: Vec<Span<'a>>,
}

impl<'a> ScopeSpans<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [scope, spans] = fields(v, ["scope", "spans"])?;
        Some(ScopeSpans {
            scope: lenient(scope, Scope::read),
            spans: lenient_list(spans, Span::read),
        })
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Resource<'a> {
    pub attributes: Vec<KeyValue<'a>>,
}

impl<'a> Resource<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [attributes] = fields(v, ["attributes"])?;
        Some(Resource {
            attributes: lenient_list(attributes, KeyValue::read),
        })
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Scope<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

impl<'a> Scope<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [name, version] = fields(v, ["name", "version"])?;
        Some(Scope {
            name: string(name),
            version: string(version),
        })
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Span<'a> {
    /// Lowercased, as are the other ids (see [`hex_id`]).
    pub trace_id: Cow<'a, str>,
    pub span_id: Cow<'a, str>,
    /// Empty for a root span.
    pub parent_span_id: Cow<'a, str>,
    pub name: &'a str,
    pub start_time_unix_nano: &'a Value,
    pub end_time_unix_nano: &'a Value,
    pub attributes: Vec<KeyValue<'a>>,
    pub events: Vec<SpanEvent<'a>>,
    /// `None` when absent or `null` (proto3: unset). Any other value is
    /// kept, so an unreadable status fails closed (see [`Status::is_error`]).
    pub status: Option<Status<'a>>,
}

impl<'a> Span<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [
            trace_id,
            span_id,
            parent_span_id,
            name,
            start,
            end,
            attributes,
            events,
            status,
        ] = fields(
            v,
            [
                "traceId",
                "spanId",
                "parentSpanId",
                "name",
                "startTimeUnixNano",
                "endTimeUnixNano",
                "attributes",
                "events",
                "status",
            ],
        )?;
        Some(Span {
            trace_id: hex_id(trace_id),
            span_id: hex_id(span_id),
            parent_span_id: hex_id(parent_span_id),
            name: string(name),
            start_time_unix_nano: start.unwrap_or(&NULL),
            end_time_unix_nano: end.unwrap_or(&NULL),
            attributes: lenient_list(attributes, KeyValue::read),
            events: lenient_list(events, SpanEvent::read),
            status: status.and_then(Status::read),
        })
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpanEvent<'a> {
    pub time_unix_nano: &'a Value,
    pub name: &'a str,
    pub attributes: Vec<KeyValue<'a>>,
}

impl<'a> SpanEvent<'a> {
    fn read(v: &'a Value) -> Option<Self> {
        let [time, name, attributes] = fields(v, ["timeUnixNano", "name", "attributes"])?;
        Some(SpanEvent {
            time_unix_nano: time.unwrap_or(&NULL),
            name: string(name),
            attributes: lenient_list(attributes, KeyValue::read),
        })
    }
}

#[derive(Debug, Default)]
pub struct ResourceLogs<'a> {
    pub resource: Resource<'a>,
    pub scope_logs: Vec<ScopeLogs<'a>>,
}

impl<'a> ResourceLogs<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [resource, scope_logs] = fields(v, ["resource", "scopeLogs"])?;
        Some(ResourceLogs {
            resource: lenient(resource, Resource::read),
            scope_logs: lenient_list(scope_logs, ScopeLogs::read),
        })
    }
}

#[derive(Debug, Default)]
pub struct ScopeLogs<'a> {
    pub scope: Scope<'a>,
    pub log_records: Vec<LogRecord<'a>>,
}

impl<'a> ScopeLogs<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [scope, records] = fields(v, ["scope", "logRecords"])?;
        Some(ScopeLogs {
            scope: lenient(scope, Scope::read),
            log_records: lenient_list(records, LogRecord::read),
        })
    }
}

/// One OTLP log record.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LogRecord<'a> {
    pub time_unix_nano: &'a Value,
    pub observed_time_unix_nano: &'a Value,
    pub body: &'a Value,
    pub attributes: Vec<KeyValue<'a>>,
    /// Lowercased, as is `span_id` (see [`hex_id`]).
    pub trace_id: Cow<'a, str>,
    pub span_id: Cow<'a, str>,
    pub event_name: &'a str,
}

#[cfg(test)]
impl Default for LogRecord<'_> {
    fn default() -> Self {
        LogRecord {
            time_unix_nano: &NULL,
            observed_time_unix_nano: &NULL,
            body: &NULL,
            attributes: Vec::new(),
            trace_id: Cow::Borrowed(""),
            span_id: Cow::Borrowed(""),
            event_name: "",
        }
    }
}

impl<'a> LogRecord<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [
            time,
            observed,
            body,
            attributes,
            trace_id,
            span_id,
            event_name,
        ] = fields(
            v,
            [
                "timeUnixNano",
                "observedTimeUnixNano",
                "body",
                "attributes",
                "traceId",
                "spanId",
                "eventName",
            ],
        )?;
        Some(LogRecord {
            time_unix_nano: time.unwrap_or(&NULL),
            observed_time_unix_nano: observed.unwrap_or(&NULL),
            body: body.unwrap_or(&NULL),
            attributes: lenient_list(attributes, KeyValue::read),
            trace_id: hex_id(trace_id),
            span_id: hex_id(span_id),
            event_name: string(event_name),
        })
    }

    /// `eventName`, else the older `event.name` string attribute.
    pub fn event(&self) -> Option<&str> {
        if !self.event_name.is_empty() {
            return Some(self.event_name);
        }
        Attrs(&self.attributes).str("event.name")
    }
}

/// A span's status as delivered.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Status<'a> {
    /// A status object; `code` is its `code` field (`Null` when absent).
    Object { code: &'a Value },
    /// Present, not `null`, and not an object.
    Unreadable(&'a Value),
}

impl<'a> Status<'a> {
    fn read(v: &'a Value) -> Option<Self> {
        match v {
            Value::Null => None,
            Value::Object(map) => Some(Status::Object {
                code: map.get("code").unwrap_or(&NULL),
            }),
            other => Some(Status::Unreadable(other)),
        }
    }

    /// Fails closed: only an absent or `null` code, `0`/`1` (number or
    /// string), or `STATUS_CODE_UNSET`/`STATUS_CODE_OK` (any case) is not an error.
    pub fn is_error(&self) -> bool {
        match self {
            Status::Unreadable(_) => true,
            Status::Object { code } => !match code {
                Value::Null => true,
                Value::Number(n) => matches!(n.as_u64(), Some(0 | 1)),
                Value::String(s) => {
                    matches!(s.as_str(), "0" | "1")
                        || s.eq_ignore_ascii_case("STATUS_CODE_UNSET")
                        || s.eq_ignore_ascii_case("STATUS_CODE_OK")
                }
                _ => false,
            },
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct KeyValue<'a> {
    /// Empty when missing, `null`, or not a string; such an entry matches
    /// no lookup.
    pub key: &'a str,
    pub value: &'a Value,
}

impl<'a> KeyValue<'a> {
    pub fn read(v: &'a Value) -> Option<Self> {
        let [key, value] = fields(v, ["key", "value"])?;
        Some(KeyValue {
            key: string(key),
            value: value.unwrap_or(&NULL),
        })
    }
}

/// Nanosecond timestamps arrive as strings (or, from some encoders, numbers).
pub fn nanos(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Typed lookups over a span's attribute list.
pub struct Attrs<'a>(pub &'a [KeyValue<'a>]);

impl<'a> Attrs<'a> {
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.iter().find(|kv| kv.key == key).map(|kv| kv.value)
    }
    pub fn str(&self, key: &str) -> Option<&'a str> {
        self.get(key)?.get("stringValue")?.as_str()
    }
    /// `intValue` (string or number); otherwise a `doubleValue` with no
    /// fractional part in `0..2^64`. A present but unreadable `intValue`
    /// is `None`.
    pub fn u64(&self, key: &str) -> Option<u64> {
        let v = self.get(key)?;
        if let Some(i) = v.get("intValue") {
            return int(i).and_then(|i| u64::try_from(i).ok());
        }
        let d = v.get("doubleValue")?.as_f64()?;
        (d.is_finite() && d >= 0.0 && d.fract() == 0.0 && d < 18_446_744_073_709_551_616.0)
            .then_some(d as u64)
    }
    pub fn f64(&self, key: &str) -> Option<f64> {
        let v = self.get(key)?;
        v.get("doubleValue")
            .and_then(Value::as_f64)
            .or_else(|| v.get("intValue").and_then(int).map(|i| i as f64))
    }
}

/// An OTLP `AnyValue` as plain JSON (`bytesValue` stays its base64
/// string); an unreadable value is `null`.
pub fn any_value_to_json(v: &Value) -> Value {
    let Some(obj) = v.as_object() else {
        return Value::Null;
    };
    if let Some(s) = obj.get("stringValue").and_then(Value::as_str) {
        return Value::String(s.to_string());
    }
    if let Some(b) = obj.get("boolValue").and_then(Value::as_bool) {
        return Value::Bool(b);
    }
    if let Some(i) = obj.get("intValue").and_then(int) {
        return Value::from(i);
    }
    if let Some(d) = obj.get("doubleValue") {
        return double_to_json(d);
    }
    if let Some(a) = obj.get("arrayValue") {
        return Value::Array(
            a.get("values")
                .and_then(Value::as_array)
                .map(|xs| xs.iter().map(any_value_to_json).collect())
                .unwrap_or_default(),
        );
    }
    if let Some(kv) = obj.get("kvlistValue") {
        let mut m = Map::new();
        for e in kv
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(k) = e.get("key").and_then(Value::as_str) {
                m.insert(
                    k.to_string(),
                    any_value_to_json(e.get("value").unwrap_or(&Value::Null)),
                );
            }
        }
        return Value::Object(m);
    }
    if let Some(b) = obj.get("bytesValue").and_then(Value::as_str) {
        return Value::String(b.to_string());
    }
    Value::Null
}

/// Always an `f64` number, so `1` and `1.0` from two encoders convert
/// alike; proto3 JSON's non-finite strings stay strings.
fn double_to_json(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or(Value::Null),
        Value::String(s) if matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity") => {
            Value::String(s.clone())
        }
        _ => Value::Null,
    }
}

/// Test-only `from_value` over the borrowed readers: the value is leaked.
#[cfg(test)]
pub(crate) mod test_read {
    use super::*;

    pub(crate) trait Read: Sized {
        fn read_value(v: &'static Value) -> Option<Self>;
    }

    impl Read for Delivery<'static> {
        fn read_value(v: &'static Value) -> Option<Self> {
            Some(Delivery::read(v))
        }
    }

    impl Read for Span<'static> {
        fn read_value(v: &'static Value) -> Option<Self> {
            Span::read(v)
        }
    }

    impl Read for LogRecord<'static> {
        fn read_value(v: &'static Value) -> Option<Self> {
            LogRecord::read(v)
        }
    }

    impl Read for Vec<KeyValue<'static>> {
        fn read_value(v: &'static Value) -> Option<Self> {
            v.as_array()?.iter().map(KeyValue::read).collect()
        }
    }

    pub(crate) fn from_value<T: Read>(v: Value) -> Result<T, ()> {
        T::read_value(Box::leak(Box::new(v))).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::test_read::from_value;
    use super::*;

    #[test]
    fn int_values_parse_from_strings_and_numbers() {
        let kvs: Vec<KeyValue> = from_value(serde_json::json!([
            {"key": "a", "value": {"intValue": "42"}},
            {"key": "b", "value": {"intValue": 7}},
            {"key": "c", "value": {"doubleValue": 0.5}},
            {"key": "d", "value": {"stringValue": "x"}}
        ]))
        .unwrap();
        let a = Attrs(&kvs);
        assert_eq!(a.u64("a"), Some(42));
        assert_eq!(a.u64("b"), Some(7));
        assert_eq!(a.f64("c"), Some(0.5));
        assert_eq!(a.f64("b"), Some(7.0));
        assert_eq!(a.str("d"), Some("x"));
        assert_eq!(a.str("missing"), None);
    }

    #[test]
    fn proto3_nulls_read_as_defaults() {
        let span: Span = from_value(serde_json::json!({
            "traceId": null, "name": null, "attributes": null, "status": null
        }))
        .unwrap();
        assert_eq!(span.trace_id, "");
        assert_eq!(span.name, "");
        assert!(span.attributes.is_empty());
        assert!(span.status.is_none());
        let d: Delivery = from_value(serde_json::json!({
            "resourceSpans": [{"scopeSpans": null}, {"scopeSpans": [{"spans": null}]}]
        }))
        .unwrap();
        assert_eq!(d.resource_spans.len(), 2);
    }

    #[test]
    fn malformed_elements_are_dropped_not_fatal() {
        let kvs: Vec<KeyValue> = from_value::<Span>(serde_json::json!({
            "attributes": [
                {"value": {"stringValue": "no key"}},
                {"key": null, "value": null},
                "not a kv",
                {"key": "ok", "value": {"stringValue": "v"}}
            ]
        }))
        .unwrap()
        .attributes;
        assert_eq!(kvs.len(), 3);
        assert_eq!(Attrs(&kvs).str("ok"), Some("v"));
    }

    #[test]
    fn status_error_is_fail_closed() {
        let span = |v: Value| -> Span { from_value(serde_json::json!({"status": v})).unwrap() };
        let is_err = |v: Value| span(v).status.as_ref().is_some_and(Status::is_error);
        for ok in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"code": 0}),
            serde_json::json!({"code": 1}),
            serde_json::json!({"code": "STATUS_CODE_UNSET"}),
            serde_json::json!({"code": "STATUS_CODE_OK"}),
            serde_json::json!({"code": "0"}),
            serde_json::json!({"code": "1"}),
            serde_json::json!({"code": "status_code_ok"}),
            serde_json::json!({"code": "Status_Code_Unset"}),
        ] {
            assert!(!is_err(ok.clone()), "{ok}");
        }
        for bad in [
            serde_json::json!({"code": 2}),
            serde_json::json!({"code": "STATUS_CODE_ERROR"}),
            serde_json::json!({"code": "ERROR"}),
            serde_json::json!({"code": "2"}),
            serde_json::json!(2),
            serde_json::json!("weird"),
            serde_json::json!([]),
            serde_json::json!({"code": "status_code_error"}),
            serde_json::json!({"code": " STATUS_CODE_OK"}),
            serde_json::json!({"code": "01"}),
            serde_json::json!({"code": 1.0}),
        ] {
            assert!(is_err(bad.clone()), "{bad}");
        }
        assert!(span(Value::Null).status.is_none());
    }

    #[test]
    fn resource_scope_ids_and_events_are_parsed() {
        let d: Delivery = from_value(serde_json::json!({"resourceSpans": [{
            "resource": {"attributes": [{"key": "k", "value": {"stringValue": "v"}}]},
            "scopeSpans": [{"scope": {"name": "s", "version": "1"}, "spans": [{
                "traceId": "t", "spanId": "a", "parentSpanId": "p", "name": "n",
                "events": [{"timeUnixNano": "5", "name": "e", "attributes": []}, "junk"]
            }]}]
        }]}))
        .unwrap();
        let rs = &d.resource_spans[0];
        assert_eq!(Attrs(&rs.resource.attributes).str("k"), Some("v"));
        let ss = &rs.scope_spans[0];
        assert_eq!((ss.scope.name, ss.scope.version), ("s", "1"));
        let span = &ss.spans[0];
        assert_eq!((&*span.span_id, &*span.parent_span_id), ("a", "p"));
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].name, "e");
    }

    #[test]
    fn span_hex_ids_read_lowercased() {
        let span: Span = from_value(serde_json::json!({
            "traceId": "AB01", "spanId": "Cd02", "parentSpanId": "ef03"
        }))
        .unwrap();
        assert_eq!(
            (&*span.trace_id, &*span.span_id, &*span.parent_span_id),
            ("ab01", "cd02", "ef03")
        );
        assert!(matches!(span.parent_span_id, Cow::Borrowed(_)));
    }

    #[test]
    fn log_records_are_parsed_leniently() {
        let d: Delivery = from_value(serde_json::json!({"resourceLogs": [{
            "scopeLogs": [{"logRecords": [
                {"traceId": "t", "spanId": "s", "eventName": "x", "body": {"stringValue": "b"}},
                {"traceId": null},
                7
            ]}]
        }]}))
        .unwrap();
        let recs = &d.resource_logs[0].scope_logs[0].log_records;
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].event_name, "x");
        assert_eq!(recs[1].trace_id, "");
    }

    #[test]
    fn is_otlp_accepts_any_signal_array_or_null() {
        for yes in [
            serde_json::json!({"resourceSpans": []}),
            serde_json::json!({"resourceSpans": null}),
            serde_json::json!({"resourceLogs": []}),
            serde_json::json!({"resourceMetrics": [{}]}),
        ] {
            assert!(is_otlp(&yes), "{yes}");
        }
        for no in [
            serde_json::json!({"resourceSpans": {}}),
            serde_json::json!({"sessions": {}}),
            serde_json::json!([]),
            serde_json::json!("resourceSpans"),
            serde_json::json!(null),
        ] {
            assert!(!is_otlp(&no), "{no}");
        }
    }

    #[test]
    fn any_values_become_plain_json() {
        let v = serde_json::json!({"kvlistValue": {"values": [
            {"key": "s", "value": {"stringValue": "x"}},
            {"key": "i", "value": {"intValue": "12"}},
            {"key": "d", "value": {"doubleValue": 0.5}},
            {"key": "b", "value": {"boolValue": true}},
            {"key": "a", "value": {"arrayValue": {"values": [{"intValue": 1}, {}]}}}
        ]}});
        assert_eq!(
            any_value_to_json(&v),
            serde_json::json!({"s": "x", "i": 12, "d": 0.5, "b": true, "a": [1, null]})
        );
    }

    #[test]
    fn u64_reads_integral_doubles_only() {
        let kvs: Vec<KeyValue> = from_value(serde_json::json!([
            {"key": "whole", "value": {"doubleValue": 42.0}},
            {"key": "frac", "value": {"doubleValue": 42.5}},
            {"key": "neg", "value": {"doubleValue": -1.0}},
            {"key": "huge", "value": {"doubleValue": 1e20}},
            {"key": "bad_int", "value": {"intValue": "12x", "doubleValue": 3.0}}
        ]))
        .unwrap();
        let a = Attrs(&kvs);
        assert_eq!(a.u64("whole"), Some(42));
        assert_eq!(a.u64("frac"), None);
        assert_eq!(a.u64("neg"), None);
        assert_eq!(a.u64("huge"), None);
        // A present intValue decides; an unreadable one is not rescued.
        assert_eq!(a.u64("bad_int"), None);
    }

    use serde_json::json;

    #[test]
    fn log_record_reads_every_field_leniently() {
        let r: LogRecord = from_value(json!({
            "timeUnixNano": "5", "observedTimeUnixNano": 6, "severityNumber": 9,
            "body": {"stringValue": "hi"},
            "attributes": [{"key": "a", "value": {"boolValue": true}}],
            "traceId": "AB01", "spanId": "cd02", "eventName": "e"
        }))
        .unwrap();
        assert_eq!(nanos(r.time_unix_nano), Some(5));
        assert_eq!(nanos(r.observed_time_unix_nano), Some(6));
        assert_eq!(*r.body, json!({"stringValue": "hi"}));
        assert_eq!(r.attributes.len(), 1);
        assert_eq!((&*r.trace_id, &*r.span_id), ("ab01", "cd02"));
        assert!(matches!(r.span_id, Cow::Borrowed(_)));
        assert_eq!(r.event_name, "e");

        let odd: LogRecord = from_value(json!({
            "traceId": null, "spanId": 7, "eventName": ["x"], "attributes": "nope", "body": null
        }))
        .unwrap();
        assert_eq!(
            (&*odd.trace_id, &*odd.span_id, odd.event_name),
            ("", "", "")
        );
        assert!(odd.attributes.is_empty());
        assert!(odd.body.is_null());
    }

    #[test]
    fn event_prefers_event_name_then_the_event_name_attribute() {
        let both: LogRecord = from_value(json!({
            "eventName": "field",
            "attributes": [{"key": "event.name", "value": {"stringValue": "attr"}}]
        }))
        .unwrap();
        assert_eq!(both.event(), Some("field"));
        let attr: LogRecord = from_value(json!({
            "attributes": [{"key": "event.name", "value": {"stringValue": "attr"}}]
        }))
        .unwrap();
        assert_eq!(attr.event(), Some("attr"));
        let none: LogRecord = from_value(json!({
            "attributes": [{"key": "event.name", "value": {"intValue": "3"}}]
        }))
        .unwrap();
        assert_eq!(none.event(), None);
    }

    #[test]
    fn double_values_convert_to_f64_whatever_their_json_spelling() {
        let one = any_value_to_json(&json!({"doubleValue": 1}));
        assert_eq!(one, any_value_to_json(&json!({"doubleValue": 1.0})));
        assert_eq!(serde_json::to_string(&one).unwrap(), "1.0");
        assert_eq!(
            any_value_to_json(&json!({"doubleValue": "NaN"})),
            json!("NaN")
        );
        assert_eq!(
            any_value_to_json(&json!({"doubleValue": "-Infinity"})),
            json!("-Infinity")
        );
        let nested = any_value_to_json(&json!({"kvlistValue": {"values": [
            {"key": "t", "value": {"doubleValue": 0}}]}}));
        assert_eq!(serde_json::to_string(&nested).unwrap(), r#"{"t":0.0}"#);
    }
}
