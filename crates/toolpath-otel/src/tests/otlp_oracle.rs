//! The borrowed OTLP readers against the serde-derived types they replaced
//! (kept here verbatim as the oracle): every fixture body and a seeded set
//! of mutations read alike.

#![allow(dead_code)]

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

mod oracle {
    use super::*;

    /// `null`, a missing field, or a value of the wrong type → `T::default()`.
    fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: Default + for<'a> Deserialize<'a>,
    {
        Ok(T::deserialize(Value::deserialize(d)?).unwrap_or_default())
    }

    /// A list whose unreadable elements are dropped; `null` or a non-list
    /// value reads as empty.
    fn lenient_list<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: for<'a> Deserialize<'a>,
    {
        Ok(match Value::deserialize(d)? {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|v| T::deserialize(v).ok())
                .collect(),
            _ => Vec::new(),
        })
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct Delivery {
        #[serde(rename = "resourceSpans", default, deserialize_with = "lenient_list")]
        pub resource_spans: Vec<ResourceSpans>,
        #[serde(rename = "resourceLogs", default, deserialize_with = "lenient_list")]
        pub resource_logs: Vec<ResourceLogs>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct ResourceSpans {
        #[serde(default, deserialize_with = "lenient")]
        pub resource: Resource,
        #[serde(rename = "scopeSpans", default, deserialize_with = "lenient_list")]
        pub scope_spans: Vec<ScopeSpans>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct ScopeSpans {
        #[serde(default, deserialize_with = "lenient")]
        pub scope: Scope,
        #[serde(default, deserialize_with = "lenient_list")]
        pub spans: Vec<Span>,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct Resource {
        #[serde(default, deserialize_with = "lenient_list")]
        pub attributes: Vec<KeyValue>,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct Scope {
        #[serde(default, deserialize_with = "lenient")]
        pub name: String,
        #[serde(default, deserialize_with = "lenient")]
        pub version: String,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct Span {
        #[serde(rename = "traceId", default, deserialize_with = "lenient")]
        pub trace_id: String,
        #[serde(rename = "spanId", default, deserialize_with = "lenient")]
        pub span_id: String,
        /// Empty for a root span.
        #[serde(rename = "parentSpanId", default, deserialize_with = "lenient")]
        pub parent_span_id: String,
        #[serde(default, deserialize_with = "lenient")]
        pub name: String,
        #[serde(rename = "startTimeUnixNano", default)]
        pub start_time_unix_nano: Value,
        #[serde(rename = "endTimeUnixNano", default)]
        pub end_time_unix_nano: Value,
        #[serde(default, deserialize_with = "lenient_list")]
        pub attributes: Vec<KeyValue>,
        #[serde(default, deserialize_with = "lenient_list")]
        pub events: Vec<SpanEvent>,
        /// `None` when absent or `null` (proto3: unset). Any other value is
        /// kept, so an unreadable status fails closed (see [`Status::is_error`]).
        #[serde(default, deserialize_with = "status")]
        pub status: Option<Status>,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct SpanEvent {
        #[serde(rename = "timeUnixNano", default)]
        pub time_unix_nano: Value,
        #[serde(default, deserialize_with = "lenient")]
        pub name: String,
        #[serde(default, deserialize_with = "lenient_list")]
        pub attributes: Vec<KeyValue>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct ResourceLogs {
        #[serde(default, deserialize_with = "lenient")]
        pub resource: Resource,
        #[serde(rename = "scopeLogs", default, deserialize_with = "lenient_list")]
        pub scope_logs: Vec<ScopeLogs>,
    }

    #[derive(Debug, Default, Deserialize)]
    pub struct ScopeLogs {
        #[serde(default, deserialize_with = "lenient")]
        pub scope: Scope,
        #[serde(rename = "logRecords", default, deserialize_with = "lenient_list")]
        pub log_records: Vec<LogRecord>,
    }

    /// One OTLP log record.
    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct LogRecord {
        #[serde(rename = "timeUnixNano", default)]
        pub time_unix_nano: Value,
        #[serde(rename = "observedTimeUnixNano", default)]
        pub observed_time_unix_nano: Value,
        #[serde(default)]
        pub body: Value,
        #[serde(default, deserialize_with = "lenient_list")]
        pub attributes: Vec<KeyValue>,
        #[serde(rename = "traceId", default, deserialize_with = "lenient")]
        pub trace_id: String,
        #[serde(rename = "spanId", default, deserialize_with = "lenient")]
        pub span_id: String,
        #[serde(rename = "eventName", default, deserialize_with = "lenient")]
        pub event_name: String,
    }

    /// A span's status as delivered.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Status {
        /// A status object; `code` is its `code` field (`Null` when absent).
        Object { code: Value },
        /// Present, not `null`, and not an object.
        Unreadable(Value),
    }

    fn status<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Status>, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Null => None,
            Value::Object(mut map) => Some(Status::Object {
                code: map.remove("code").unwrap_or(Value::Null),
            }),
            other => Some(Status::Unreadable(other)),
        })
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct KeyValue {
        /// Empty when missing, `null`, or not a string; such an entry matches
        /// no lookup.
        #[serde(default, deserialize_with = "lenient")]
        pub key: String,
        #[serde(default)]
        pub value: Value,
    }
}

macro_rules! projector {
    ($name:ident, $($m:ident)::+, $life:lifetime) => {
        fn $name(d: &$($m)::+::Delivery) -> Value {
            use $($m)::+ as m;
            fn kvs(a: &[m::KeyValue]) -> Value {
                a.iter().map(|kv| json!([kv.key, kv.value])).collect()
            }
            fn resource(r: &m::Resource) -> Value {
                kvs(&r.attributes)
            }
            fn scope(s: &m::Scope) -> Value {
                json!([s.name, s.version])
            }
            fn status(s: &Option<m::Status>) -> Value {
                match s {
                    None => json!("none"),
                    Some(m::Status::Object { code }) => json!({"object": code}),
                    Some(m::Status::Unreadable(v)) => json!({"unreadable": v}),
                }
            }
            fn span(s: &m::Span) -> Value {
                json!([
                    s.trace_id, s.span_id, s.parent_span_id, s.name,
                    s.start_time_unix_nano, s.end_time_unix_nano, kvs(&s.attributes),
                    s.events.iter().map(|e| json!([e.time_unix_nano, e.name, kvs(&e.attributes)])).collect::<Value>(),
                    status(&s.status)
                ])
            }
            fn record(r: &m::LogRecord) -> Value {
                json!([
                    r.time_unix_nano, r.observed_time_unix_nano, r.body, kvs(&r.attributes),
                    r.trace_id, r.span_id, r.event_name
                ])
            }
            json!({
                "spans": d.resource_spans.iter().map(|rs| json!([
                    resource(&rs.resource),
                    rs.scope_spans.iter().map(|ss| json!([
                        scope(&ss.scope),
                        ss.spans.iter().map(span).collect::<Value>()
                    ])).collect::<Value>()
                ])).collect::<Value>(),
                "logs": d.resource_logs.iter().map(|rl| json!([
                    resource(&rl.resource),
                    rl.scope_logs.iter().map(|sl| json!([
                        scope(&sl.scope),
                        sl.log_records.iter().map(record).collect::<Value>()
                    ])).collect::<Value>()
                ])).collect::<Value>()
            })
        }
    };
}

projector!(project_old, oracle, 'static);
projector!(project_new, crate::otlp, 'static);

fn old(v: &Value) -> Value {
    project_old(&oracle::Delivery::deserialize(v).unwrap_or_default())
}

fn new(v: &Value) -> Value {
    project_new(&crate::otlp::Delivery::read(v))
}

pub(crate) fn fixture_values() -> Vec<Value> {
    fn walk(dir: &std::path::Path, out: &mut Vec<Value>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "json" || x == "ndjson") {
                let text = std::fs::read_to_string(&p).unwrap();
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => out.push(v),
                    Err(_) => out.extend(text.lines().filter_map(|l| serde_json::from_str(l).ok())),
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel"),
        &mut out,
    );
    out
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Every node of `v` (pre-order count).
fn count(v: &Value) -> usize {
    1 + match v {
        Value::Array(a) => a.iter().map(count).sum(),
        Value::Object(o) => o.values().map(count).sum(),
        _ => 0,
    }
}

fn nth_mut(v: &mut Value, n: &mut usize) -> Option<*mut Value> {
    if *n == 0 {
        return Some(v as *mut Value);
    }
    *n -= 1;
    match v {
        Value::Array(a) => a.iter_mut().find_map(|x| nth_mut(x, n)),
        Value::Object(o) => o.values_mut().find_map(|x| nth_mut(x, n)),
        _ => None,
    }
}

/// Replace one node: null, a scalar, an object turned into its positional
/// array form (sometimes too long or short), or an array wrapped.
fn mutate(v: &mut Value, rng: &mut Rng) {
    let total = count(v);
    let mut n = rng.below(total);
    let Some(p) = nth_mut(v, &mut n) else { return };
    // SAFETY: `p` points into `v`, which is not otherwise borrowed here.
    let node = unsafe { &mut *p };
    *node = match rng.below(8) {
        0 => Value::Null,
        1 => json!(7),
        2 => json!("s"),
        3 => json!([]),
        4 => json!({}),
        5 | 6 => match node.take() {
            Value::Object(o) => {
                let mut items: Vec<Value> = o.into_iter().map(|(_, x)| x).collect();
                if rng.below(3) == 0 {
                    items.truncate(rng.below(items.len() + 1));
                }
                if rng.below(4) == 0 {
                    items.extend([
                        json!(1),
                        json!(2),
                        json!(3),
                        json!(4),
                        json!(5),
                        json!(6),
                        json!(7),
                        json!(8),
                        json!(9),
                        json!(10),
                    ]);
                }
                Value::Array(items)
            }
            other => json!([other]),
        },
        _ => json!({"code": 2}),
    };
}

#[test]
fn borrowed_readers_match_the_serde_types_on_every_fixture() {
    let values = fixture_values();
    assert!(values.len() > 40, "{}", values.len());
    for v in &values {
        assert_eq!(new(v), old(v), "{v}");
    }
}

#[test]
fn borrowed_readers_match_the_serde_types_on_mutations() {
    let values: Vec<Value> = fixture_values()
        .into_iter()
        .filter(|v| crate::otlp::is_otlp(v) && count(v) < 20_000)
        .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut checked = 0;
    for round in 0..40 {
        for v in &values {
            let mut m = v.clone();
            for _ in 0..1 + round % 4 {
                mutate(&mut m, &mut rng);
            }
            assert_eq!(new(&m), old(&m), "{m}");
            checked += 1;
        }
    }
    assert!(checked > 1000);
}

#[test]
fn positional_and_odd_shapes_read_alike() {
    for v in [
        json!({"resourceSpans": [[[[]], [[["n", "v"], [["t", "s", "p", "LLM Generation", "1", "2", [["k", {"stringValue": "x"}]], [], {"code": 1}]]]]]]}),
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [[]]}]}]}),
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [["t", "s", "p", "n", 1, 2, [], [], null, "extra"]]}]}]}),
        json!({"resourceSpans": [{"resource": ["a"], "scopeSpans": [{"scope": [1, 2, 3]}]}]}),
        json!({"resourceLogs": [[{"attributes": [["k"]]}, [[["n"], [[1, 2, {"stringValue": "b"}, [], "t", "s", "e"]]]]]]}),
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [{"status": [1]}, {"status": "x"}, {"status": {}}, {"attributes": [["k", 1, 2]]}]}]}]}),
        json!({"resourceSpans": null, "resourceLogs": [null, 1, "x", {"scopeLogs": [{"logRecords": [{"traceId": 5}]}]}]}),
    ] {
        assert_eq!(new(&v), old(&v), "{v}");
    }
}
