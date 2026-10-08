//! Log records in the walker: dedupe, correlation to spans by
//! `(traceId, spanId)`, and orphan units. Reads no attribute key but
//! `event.name` (enforced by `tests/seam.rs`).

use crate::hash::{canonical_json, sha256_hex};
use crate::otlp::{Delivery, LogRecord, any_value_to_json, nanos};
use crate::profile::{LogRef, Profile};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// A log record and its position among every record of the input.
#[derive(Clone, Copy)]
pub(crate) struct Indexed<'a> {
    pub(crate) log: LogRef<'a>,
    pub(crate) order: usize,
}

/// `(traceId, spanId)`, already lowercased by the reader.
pub(crate) type SpanKey = (String, String);

pub(crate) fn span_key(trace_id: &str, span_id: &str) -> SpanKey {
    (trace_id.to_string(), span_id.to_string())
}

/// The record dedupe key. Values go through [`any_value_to_json`] first,
/// so one record read from protobuf and from JSON has one key.
pub(crate) fn dedupe_key(r: &LogRecord) -> (SpanKey, u64, String, String) {
    let attributes = Value::Array(
        r.attributes
            .iter()
            .map(|kv| json!([kv.key, any_value_to_json(kv.value)]))
            .collect(),
    );
    let body = canonical_json(&any_value_to_json(r.body));
    let attributes = canonical_json(&attributes);
    (
        span_key(&r.trace_id, &r.span_id),
        nanos(r.time_unix_nano).unwrap_or(0),
        r.event().unwrap_or_default().to_string(),
        sha256_hex(&[body.as_bytes(), b"\0", attributes.as_bytes()]),
    )
}

/// Every log record in input order, keeping the first copy of each
/// [`dedupe_key`].
pub(crate) fn index_logs<'a>(deliveries: &'a [Delivery<'a>]) -> Vec<Indexed<'a>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut order = 0;
    for delivery in deliveries {
        for rl in &delivery.resource_logs {
            for sl in &rl.scope_logs {
                for record in &sl.log_records {
                    if seen.insert(dedupe_key(record)) {
                        out.push(Indexed {
                            log: LogRef {
                                resource: &rl.resource,
                                scope: &sl.scope,
                                record,
                            },
                            order,
                        });
                    }
                    order += 1;
                }
            }
        }
    }
    out
}

/// Every record with a trace id, by trace id, in
/// `(timeUnixNano, input order)`: what [`TraceView::all_logs`] shows.
///
/// [`TraceView::all_logs`]: crate::profile::TraceView::all_logs
pub(crate) fn by_trace<'a>(logs: &[Indexed<'a>]) -> HashMap<String, Vec<LogRef<'a>>> {
    let mut grouped: HashMap<String, Vec<Indexed<'a>>> = HashMap::new();
    for ix in logs {
        let trace_id = &ix.log.record.trace_id;
        if !trace_id.is_empty() {
            grouped.entry(trace_id.to_string()).or_default().push(*ix);
        }
    }
    grouped
        .into_iter()
        .map(|(trace, mut records)| {
            records.sort_by_key(time_order);
            (trace, records.into_iter().map(|ix| ix.log).collect())
        })
        .collect()
}

/// What a `(traceId, spanId)` names in the batch.
pub(crate) enum Role {
    /// Candidate units by index; a span delivered twice is two units, and
    /// both get the records.
    Units(Vec<usize>),
    /// An absorbed span: its records belong to no unit and are no orphans.
    Absorbed,
}

#[derive(Default)]
pub(crate) struct Partition<'a> {
    /// Records correlated to each span unit, in `(timeUnixNano, input order)`.
    pub(crate) unit_logs: HashMap<usize, Vec<LogRef<'a>>>,
    /// Records correlated to no unit and no absorbed span, in input order.
    pub(crate) orphans: Vec<Indexed<'a>>,
}

fn time_order(ix: &Indexed<'_>) -> (u64, usize) {
    (nanos(ix.log.record.time_unix_nano).unwrap_or(0), ix.order)
}

/// Route each record by the role of the span it names.
pub(crate) fn partition<'a>(
    logs: Vec<Indexed<'a>>,
    roles: &HashMap<SpanKey, Role>,
) -> Partition<'a> {
    let mut by_unit: HashMap<usize, Vec<Indexed<'a>>> = HashMap::new();
    let mut out = Partition::default();
    for ix in logs {
        let r = ix.log.record;
        let role = if r.span_id.is_empty() {
            None
        } else {
            roles.get(&span_key(&r.trace_id, &r.span_id))
        };
        match role {
            Some(Role::Units(units)) => {
                for u in units {
                    by_unit.entry(*u).or_default().push(ix);
                }
            }
            Some(Role::Absorbed) => {}
            None => out.orphans.push(ix),
        }
    }
    for (unit, mut records) in by_unit {
        records.sort_by_key(time_order);
        out.unit_logs
            .insert(unit, records.into_iter().map(|ix| ix.log).collect());
    }
    out
}

/// One generation's worth of orphan records.
pub(crate) struct OrphanUnit<'a> {
    /// Index into the consulted profiles.
    pub(crate) profile: usize,
    /// The group, in `(timeUnixNano, input order)`; never empty.
    pub(crate) logs: Vec<LogRef<'a>>,
}

/// Orphan units, in the input order of each group's earliest record, and
/// the count of records no profile claimed.
pub(crate) fn orphan_units<'a>(
    orphans: Vec<Indexed<'a>>,
    profiles: &[&dyn Profile],
) -> (Vec<OrphanUnit<'a>>, usize) {
    // `group_logs` hands back only `LogRef`s, so a record's address (stable
    // for the whole read) recovers its input position; an invented one sorts last.
    let order: HashMap<*const LogRecord, usize> = orphans
        .iter()
        .map(|ix| (ix.log.record as *const LogRecord, ix.order))
        .collect();
    let position = |l: &LogRef<'a>| {
        order
            .get(&(l.record as *const LogRecord))
            .copied()
            .unwrap_or(usize::MAX)
    };
    let mut accepted: Vec<Vec<LogRef<'a>>> = profiles.iter().map(|_| Vec::new()).collect();
    let mut unclaimed = 0;
    for ix in &orphans {
        match profiles
            .iter()
            .position(|p| p.claims_log(ix.log.resource, ix.log.record))
        {
            Some(i) => accepted[i].push(ix.log),
            None => unclaimed += 1,
        }
    }
    let mut units: Vec<(usize, OrphanUnit<'a>)> = Vec::new();
    for (i, logs) in accepted.into_iter().enumerate() {
        if logs.is_empty() {
            continue;
        }
        for mut group in profiles[i].group_logs(logs) {
            if group.is_empty() {
                continue;
            }
            group.sort_by_key(|l| (nanos(l.record.time_unix_nano).unwrap_or(0), position(l)));
            let first = group.iter().map(&position).min().unwrap_or(usize::MAX);
            units.push((
                first,
                OrphanUnit {
                    profile: i,
                    logs: group,
                },
            ));
        }
    }
    units.sort_by_key(|(first, _)| *first);
    (units.into_iter().map(|(_, u)| u).collect(), unclaimed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otlp::Resource;

    fn deliveries(values: &[Value]) -> Vec<Delivery<'static>> {
        values
            .iter()
            .map(|v| Delivery::read(Box::leak(Box::new(v.clone()))))
            .collect()
    }

    fn logs(records: Value) -> Value {
        json!({"resourceLogs": [{"scopeLogs": [{"logRecords": records}]}]})
    }

    const T: &str = "0102030405060708090a0b0c0d0e0f10";

    fn bodies(v: &[LogRef<'_>]) -> Vec<String> {
        v.iter()
            .map(|l| {
                l.record.body["stringValue"]
                    .as_str()
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_same_record_in_two_encodings_is_read_once() {
        let as_json = logs(json!([{
            "timeUnixNano": 5, "traceId": T, "spanId": "a1a2a3a4a5a6a7a8",
            "eventName": "e",
            "body": {"kvlistValue": {"values": [
                {"key": "n", "value": {"intValue": 5}},
                {"key": "d", "value": {"doubleValue": 1}}]}},
            "attributes": [{"key": "k", "value": {"stringValue": "v"}}]
        }]));
        let as_proto = logs(json!([{
            "timeUnixNano": "5", "traceId": T.to_uppercase(), "spanId": "A1A2A3A4A5A6A7A8",
            "eventName": "e",
            "body": {"kvlistValue": {"values": [
                {"key": "d", "value": {"doubleValue": 1.0}},
                {"key": "n", "value": {"intValue": "5"}}]}},
            "attributes": [{"key": "k", "value": {"stringValue": "v"}}]
        }]));
        let d = deliveries(&[as_json, as_proto]);
        let ix = index_logs(&d);
        assert_eq!(ix.len(), 1);
        assert_eq!(ix[0].order, 0);
    }

    #[test]
    fn records_differing_in_any_key_part_are_kept() {
        let base = json!({"timeUnixNano": "5", "traceId": T, "spanId": "a1a2a3a4a5a6a7a8",
                          "eventName": "e", "body": {"stringValue": "x"}});
        let mut variants = vec![base.clone()];
        for (k, v) in [
            ("timeUnixNano", json!("6")),
            ("traceId", json!("ff02030405060708090a0b0c0d0e0f10")),
            ("spanId", json!("b1a2a3a4a5a6a7a8")),
            ("eventName", json!("f")),
            ("body", json!({"stringValue": "y"})),
            (
                "attributes",
                json!([{"key": "k", "value": {"stringValue": "v"}}]),
            ),
        ] {
            let mut r = base.clone();
            r[k] = v;
            variants.push(r);
        }
        let d = deliveries(&[logs(Value::Array(variants))]);
        assert_eq!(index_logs(&d).len(), 7);
    }

    #[test]
    fn the_event_name_attribute_and_field_key_alike() {
        use crate::otlp::test_read::from_value;
        let a: LogRecord = from_value(json!({"eventName": "e"})).unwrap();
        let b: LogRecord = from_value(json!({
            "attributes": [{"key": "event.name", "value": {"stringValue": "e"}}]
        }))
        .unwrap();
        assert_eq!(dedupe_key(&a).2, dedupe_key(&b).2);
    }

    #[test]
    fn partition_routes_by_role_and_sorts_by_time_then_input_order() {
        let d = deliveries(&[logs(json!([
            {"timeUnixNano": "9", "traceId": T, "spanId": "0000000000000001", "body": {"stringValue": "late"}},
            {"timeUnixNano": "3", "traceId": T, "spanId": "0000000000000001", "body": {"stringValue": "early"}},
            {"timeUnixNano": "3", "traceId": T, "spanId": "0000000000000001", "body": {"stringValue": "early-2"}},
            {"traceId": T, "spanId": "0000000000000002", "body": {"stringValue": "absorbed"}},
            {"traceId": T, "spanId": "0000000000000003", "body": {"stringValue": "orphan"}},
            {"body": {"stringValue": "no ids"}}
        ]))]);
        let mut roles = HashMap::new();
        roles.insert(span_key(T, "0000000000000001"), Role::Units(vec![0, 4]));
        roles.insert(span_key(T, "0000000000000002"), Role::Absorbed);
        let p = partition(index_logs(&d), &roles);
        assert_eq!(bodies(&p.unit_logs[&0]), ["early", "early-2", "late"]);
        assert_eq!(bodies(&p.unit_logs[&4]), ["early", "early-2", "late"]);
        let orphans: Vec<LogRef<'_>> = p.orphans.iter().map(|ix| ix.log).collect();
        assert_eq!(bodies(&orphans), ["orphan", "no ids"]);
    }

    struct TakesEvent(&'static str);
    impl Profile for TakesEvent {
        fn name(&self) -> &'static str {
            self.0
        }
        fn claims(
            &self,
            _: &crate::otlp::Resource,
            _: &crate::otlp::Scope,
            _: &crate::otlp::Span,
        ) -> bool {
            false
        }
        fn claims_log(&self, _: &Resource, log: &LogRecord) -> bool {
            log.event() == Some(self.0)
        }
        fn identify(&self, _: &crate::profile::Unit<'_>) -> crate::profile::Ident {
            crate::profile::Ident {
                generation_id: None,
                session_id: None,
            }
        }
        fn extract<'a>(
            &self,
            _: &crate::profile::Unit<'a>,
            _: &crate::profile::TraceView<'a>,
            _cx: &mut crate::walk::ReadCx<'a>,
        ) -> Result<crate::generation::Generation, crate::walk::SkipReason> {
            Err(crate::walk::SkipReason::MissingPayload)
        }
    }

    #[test]
    fn orphans_go_to_the_first_accepting_profile_and_group_by_span() {
        let d = deliveries(&[logs(json!([
            {"eventName": "b", "traceId": T, "spanId": "0000000000000009", "timeUnixNano": "2"},
            {"eventName": "a", "traceId": T, "spanId": "0000000000000001", "timeUnixNano": "7"},
            {"eventName": "a", "traceId": T, "spanId": "0000000000000001", "timeUnixNano": "1"},
            {"eventName": "a", "body": {"stringValue": "one"}},
            {"eventName": "a", "body": {"stringValue": "two"}},
            {"eventName": "zzz"}
        ]))]);
        let (a, b) = (TakesEvent("a"), TakesEvent("b"));
        let profiles: [&dyn Profile; 2] = [&a, &b];
        let (units, unclaimed) = orphan_units(index_logs(&d), &profiles);
        assert_eq!(unclaimed, 1);
        let shape: Vec<(usize, Vec<u64>)> = units
            .iter()
            .map(|u| {
                (
                    u.profile,
                    u.logs
                        .iter()
                        .map(|l| nanos(l.record.time_unix_nano).unwrap_or(0))
                        .collect(),
                )
            })
            .collect();
        // b's group (earliest record at input position 0); a's span group
        // (earliest input position 1, sorted by time); then the two id-less
        // singletons in input order.
        assert_eq!(
            shape,
            [(1, vec![2]), (0, vec![1, 7]), (0, vec![0]), (0, vec![0])]
        );
        assert_eq!(bodies(&units[2].logs), ["one"]);
        assert_eq!(bodies(&units[3].logs), ["two"]);
    }

    #[test]
    fn identical_id_less_records_are_one_record() {
        let d = deliveries(&[logs(json!([{"eventName": "a"}, {"eventName": "a"}]))]);
        assert_eq!(index_logs(&d).len(), 1);
    }

    #[test]
    fn a_group_logs_override_that_drops_records_never_panics() {
        struct Drops;
        impl Profile for Drops {
            fn name(&self) -> &'static str {
                "drops"
            }
            fn claims(
                &self,
                _: &crate::otlp::Resource,
                _: &crate::otlp::Scope,
                _: &crate::otlp::Span,
            ) -> bool {
                false
            }
            fn claims_log(&self, _: &Resource, _: &LogRecord) -> bool {
                true
            }
            fn group_logs<'a>(&self, _logs: Vec<LogRef<'a>>) -> Vec<Vec<LogRef<'a>>> {
                vec![Vec::new()]
            }
            fn identify(&self, _: &crate::profile::Unit<'_>) -> crate::profile::Ident {
                crate::profile::Ident {
                    generation_id: None,
                    session_id: None,
                }
            }
            fn extract<'a>(
                &self,
                _: &crate::profile::Unit<'a>,
                _: &crate::profile::TraceView<'a>,
                _cx: &mut crate::walk::ReadCx<'a>,
            ) -> Result<crate::generation::Generation, crate::walk::SkipReason> {
                Err(crate::walk::SkipReason::MissingPayload)
            }
        }
        let d = deliveries(&[logs(json!([{"eventName": "a"}]))]);
        let (units, unclaimed) = orphan_units(index_logs(&d), &[&Drops]);
        assert!(units.is_empty());
        assert_eq!(unclaimed, 0);
    }

    /// A test-only profile: claims spans named `llm`, absorbs spans named
    /// `tool`, claims orphan records whose event is `evt`. Its generation
    /// id is the `gid` string attribute of the span, else of the first log
    /// record. `extract` records what the walker handed it.
    struct LogTest;
    fn gid(attrs: &[crate::otlp::KeyValue]) -> Option<String> {
        crate::otlp::Attrs(attrs).str("gid").map(str::to_string)
    }
    impl Profile for LogTest {
        fn name(&self) -> &'static str {
            "logtest"
        }
        fn claims(
            &self,
            _: &crate::otlp::Resource,
            _: &crate::otlp::Scope,
            s: &crate::otlp::Span,
        ) -> bool {
            s.name == "llm"
        }
        fn absorbs(
            &self,
            _: &crate::otlp::Resource,
            _: &crate::otlp::Scope,
            s: &crate::otlp::Span,
        ) -> bool {
            s.name == "tool"
        }
        fn claims_log(&self, _: &Resource, log: &LogRecord) -> bool {
            log.event() == Some("evt")
        }
        fn identify(&self, unit: &crate::profile::Unit<'_>) -> crate::profile::Ident {
            let generation_id = unit
                .span
                .and_then(|s| gid(&s.attributes))
                .or_else(|| unit.logs.first().and_then(|l| gid(&l.record.attributes)));
            crate::profile::Ident {
                generation_id,
                session_id: None,
            }
        }
        fn extract<'a>(
            &self,
            unit: &crate::profile::Unit<'a>,
            _trace: &crate::profile::TraceView<'a>,
            _cx: &mut crate::walk::ReadCx<'a>,
        ) -> Result<crate::generation::Generation, crate::walk::SkipReason> {
            let id = self
                .identify(unit)
                .generation_id
                .ok_or(crate::walk::SkipReason::MissingPayload)?;
            let mut source_meta = serde_json::Map::new();
            source_meta.insert("span".into(), json!(unit.span.is_some()));
            source_meta.insert("logs".into(), json!(bodies(&unit.logs)));
            Ok(crate::generation::Generation {
                id,
                source_meta,
                ..Default::default()
            })
        }
    }

    fn span(name: &str, span_id: &str, parent: &str, gid: Option<&str>) -> Value {
        let mut s = json!({"traceId": T, "spanId": span_id, "parentSpanId": parent, "name": name,
                           "startTimeUnixNano": "1", "endTimeUnixNano": "2"});
        if let Some(g) = gid {
            s["attributes"] = json!([{"key": "gid", "value": {"stringValue": g}}]);
        }
        s
    }

    fn spans(list: Vec<Value>) -> Value {
        json!({"resourceSpans": [{"scopeSpans": [{"spans": list}]}]})
    }

    fn rec(span_id: &str, time: &str, body: &str, event: Option<&str>, gid: Option<&str>) -> Value {
        let mut r = json!({"traceId": T, "spanId": span_id, "timeUnixNano": time,
                           "body": {"stringValue": body}});
        if let Some(e) = event {
            r["eventName"] = json!(e);
        }
        if let Some(g) = gid {
            r["attributes"] = json!([{"key": "gid", "value": {"stringValue": g}}]);
        }
        r
    }

    fn read(values: &[Value]) -> crate::walk::ReadOutcome {
        crate::walk::read_with(values.iter(), &[&LogTest]).unwrap()
    }

    fn batch() -> (Value, Value) {
        let s = spans(vec![
            span("llm", "0000000000000001", "", Some("g1")),
            span("tool", "0000000000000002", "0000000000000001", None),
            span("other", "0000000000000003", "", None),
        ]);
        let l = logs(json!([
            rec("0000000000000001", "2", "a", None, None),
            rec("0000000000000001", "1", "b", None, None),
            rec("0000000000000002", "5", "t", None, None),
            rec("0000000000000003", "6", "u3", Some("evt"), Some("g3")),
            rec("", "7", "free", Some("evt"), Some("g4")),
            rec("", "8", "x", Some("not-claimed"), None),
        ]));
        (s, l)
    }

    fn summary(out: &crate::walk::ReadOutcome) -> Vec<(String, Value)> {
        out.generations
            .iter()
            .map(|g| (g.id.clone(), Value::Object(g.source_meta.clone())))
            .collect()
    }

    #[test]
    fn correlated_absorbed_and_orphan_records_reach_the_right_units() {
        let (s, l) = batch();
        let out = read(&[s, l]);
        assert_eq!(
            summary(&out),
            [
                ("g1".to_string(), json!({"span": true, "logs": ["b", "a"]})),
                ("g3".to_string(), json!({"span": false, "logs": ["u3"]})),
                // `free` names trace T but no span: an orphan.
                ("g4".to_string(), json!({"span": false, "logs": ["free"]})),
            ]
        );
        // The `other` span and the `not-claimed` record.
        assert_eq!(out.unclaimed, 2);
    }

    #[test]
    fn logs_before_spans_and_uppercase_ids_still_correlate() {
        let (s, l) = batch();
        let mut upper = l.clone();
        for r in upper["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array_mut()
            .unwrap()
        {
            for k in ["traceId", "spanId"] {
                let id = r[k].as_str().unwrap().to_uppercase();
                r[k] = json!(id);
            }
        }
        // As `logs.json` sorts before `traces.json` in a directory.
        let out = read(&[upper, s.clone()]);
        let expected = read(&[s, l]);
        assert_eq!(summary(&out), summary(&expected));
        assert_eq!(out.unclaimed, expected.unclaimed);
        assert_eq!(out.generations[0].source_meta["logs"], json!(["b", "a"]));
    }

    #[test]
    fn records_of_an_errored_span_unit_never_become_orphans() {
        let mut errored = span("llm", "0000000000000001", "", Some("g1"));
        errored["status"] = json!({"code": 2});
        let l = logs(json!([rec(
            "0000000000000001",
            "1",
            "a",
            Some("evt"),
            Some("gX")
        )]));
        let out = read(&[spans(vec![errored]), l]);
        assert!(out.generations.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.unclaimed, 0);
    }

    #[test]
    fn records_of_an_ancestor_absorbed_span_reach_only_the_trace_view() {
        let s = spans(vec![
            span("llm", "0000000000000001", "", Some("outer")),
            span("llm", "0000000000000002", "0000000000000001", Some("inner")),
        ]);
        let l = logs(json!([rec(
            "0000000000000002",
            "1",
            "inner-log",
            Some("evt"),
            None
        )]));
        let out = read(&[s, l]);
        assert_eq!(
            summary(&out),
            [("outer".to_string(), json!({"span": true, "logs": []}))]
        );
        assert_eq!(out.unclaimed, 0);
    }

    #[test]
    fn an_orphan_repeating_a_span_units_generation_id_is_a_duplicate() {
        let s = spans(vec![span("llm", "0000000000000001", "", Some("g1"))]);
        let l = logs(json!([rec("", "1", "late", Some("evt"), Some("g1"))]));
        let out = read(&[s, l]);
        assert_eq!(out.generations.len(), 1);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].reason, crate::walk::SkipReason::Duplicate);
        assert_eq!(out.skipped[0].generation_id.as_deref(), Some("g1"));
    }

    /// [`LogTest`]'s claims, but `extract` records every log record its
    /// `TraceView` shows through `all_logs`.
    struct AllLogs;
    impl Profile for AllLogs {
        fn name(&self) -> &'static str {
            "all-logs"
        }
        fn claims(
            &self,
            r: &crate::otlp::Resource,
            sc: &crate::otlp::Scope,
            s: &crate::otlp::Span,
        ) -> bool {
            LogTest.claims(r, sc, s)
        }
        fn absorbs(
            &self,
            r: &crate::otlp::Resource,
            sc: &crate::otlp::Scope,
            s: &crate::otlp::Span,
        ) -> bool {
            LogTest.absorbs(r, sc, s)
        }
        fn claims_log(&self, r: &Resource, log: &LogRecord) -> bool {
            LogTest.claims_log(r, log)
        }
        fn identify(&self, unit: &crate::profile::Unit<'_>) -> crate::profile::Ident {
            LogTest.identify(unit)
        }
        fn extract<'a>(
            &self,
            unit: &crate::profile::Unit<'a>,
            trace: &crate::profile::TraceView<'a>,
            _cx: &mut crate::walk::ReadCx<'a>,
        ) -> Result<crate::generation::Generation, crate::walk::SkipReason> {
            let id = self
                .identify(unit)
                .generation_id
                .ok_or(crate::walk::SkipReason::MissingPayload)?;
            let all: Vec<LogRef<'_>> = trace.all_logs().collect();
            let mut source_meta = serde_json::Map::new();
            source_meta.insert("all".into(), json!(bodies(&all)));
            Ok(crate::generation::Generation {
                id,
                source_meta,
                ..Default::default()
            })
        }
    }

    #[test]
    fn trace_view_all_logs_sees_every_record_of_its_trace() {
        let (s, l) = batch();
        let mut upper = l.clone();
        let records = upper["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array_mut()
            .unwrap();
        for r in records.iter_mut() {
            let id = r["traceId"].as_str().unwrap().to_uppercase();
            r["traceId"] = json!(id);
        }
        // Another trace's record and an id-less record are in no view.
        records.push(
            json!({"traceId": "ff02030405060708090a0b0c0d0e0f10", "spanId": "0000000000000001",
                            "timeUnixNano": "3", "body": {"stringValue": "elsewhere"}}),
        );
        records.push(json!({"timeUnixNano": "4", "body": {"stringValue": "no trace"}}));
        // A redelivered record is one record.
        let again = logs(json!([rec("0000000000000001", "2", "a", None, None)]));
        let out = crate::walk::read_with([&upper, &s, &again], &[&AllLogs]).unwrap();
        // Correlated (b, a), absorbed (t), orphan claimed (u3, free) and
        // orphan unclaimed (x), in (timeUnixNano, input order), for span
        // and orphan units alike.
        let every = json!(["b", "a", "t", "u3", "free", "x"]);
        let got: Vec<(String, Value)> = out
            .generations
            .iter()
            .map(|g| (g.id.clone(), g.source_meta["all"].clone()))
            .collect();
        assert_eq!(
            got,
            [
                ("g1".to_string(), every.clone()),
                ("g3".to_string(), every.clone()),
                ("g4".to_string(), every),
            ]
        );
    }
}
