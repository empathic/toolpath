//! The OTLP walker: index every span, resolve each against the consulted
//! profiles, gate on status, dedupe, extract. Knows no attribute key.

mod logs;
pub(crate) mod memo;
pub(crate) mod scan;

pub use memo::ReadCx;

use crate::error::{OtelError, Result};
use crate::generation::Generation;
use crate::otlp::Delivery;
use crate::profile::{self, Ident, Profile, ProfileSelection, SpanRef, TraceView, Unit};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// What reading a batch produced.
#[derive(Debug, Clone, Default)]
pub struct ReadOutcome {
    pub generations: Vec<Generation>,
    pub skipped: Vec<Skipped>,
    /// Distinct spans no consulted profile claimed or absorbed (once per
    /// `(traceId, spanId)`; each id-less span counts), plus distinct orphan
    /// log records no profile claims. Counted, never listed.
    pub unclaimed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Skipped {
    #[cfg_attr(not(test), allow(dead_code))]
    pub generation_id: Option<String>,
    pub session_id: Option<String>,
    /// Name of the profile that produced the skip; `None` for walker
    /// skips (`ErrorStatus`, `Duplicate`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub profile: Option<String>,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Span status is an error (e.g. client disconnected), or unreadable.
    ErrorStatus,
    /// OpenRouter's destination test span.
    ConnectionTest,
    /// A generation id already read from an earlier delivery.
    Duplicate,
    /// Prompt or completion present but not valid JSON (a cut-off
    /// attribute), or valid JSON of the wrong shape.
    Truncated,
    /// No generation id, or nothing even a skeleton can be built from.
    MissingPayload,
}

/// `Candidate(rank)`: claimed by the profile at that consulted index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Candidate(usize),
    Absorbed,
    Unclaimed,
}

/// Read a batch of OTLP/JSON deliveries; content problems are reported in
/// the outcome, not as errors.
pub fn read_deliveries<'a>(
    deliveries: impl IntoIterator<Item = &'a Value>,
    sel: ProfileSelection,
) -> Result<ReadOutcome> {
    read_with(deliveries, profile::consulted(sel))
}

/// [`read_deliveries`] over an explicit profile list (rank = index).
pub(crate) fn read_with<'a>(
    deliveries: impl IntoIterator<Item = &'a Value>,
    profiles: &[&dyn Profile],
) -> Result<ReadOutcome> {
    let mut parsed: Vec<Delivery> = Vec::new();
    for value in deliveries {
        if !crate::otlp::is_otlp(value) {
            return Err(OtelError::NotOtlp);
        }
        // Lenient types: an object never fails to read.
        parsed.push(Delivery::read(value));
    }

    let spans: Vec<SpanRef<'_>> = parsed
        .iter()
        .flat_map(|d| &d.resource_spans)
        .flat_map(|rs| {
            rs.scope_spans.iter().flat_map(move |ss| {
                ss.spans.iter().map(move |span| SpanRef {
                    resource: &rs.resource,
                    scope: &ss.scope,
                    span,
                })
            })
        })
        .collect();
    let roles: Vec<Role> = spans.iter().map(|s| resolve(profiles, s)).collect();
    // Ancestor rule: a candidate with a qualifying ancestor is absorbed.
    let index = span_index(&spans);
    let roles: Vec<Role> = (0..spans.len())
        .map(|i| match absorbing_ancestor(i, &spans, &roles, &index) {
            Some(_) => Role::Absorbed,
            None => roles[i],
        })
        .collect();
    // TraceView sees each (traceId, spanId) once, first in input order.
    let mut by_trace: HashMap<String, Vec<SpanRef<'_>>> = HashMap::new();
    for (i, s) in spans.iter().enumerate() {
        if !first_copy(s, i, &index) {
            continue;
        }
        by_trace
            .entry(s.span.trace_id.to_string())
            .or_default()
            .push(*s);
    }

    // A candidate's unit index is its index in `spans`.
    let mut log_roles: HashMap<logs::SpanKey, logs::Role> = HashMap::new();
    for (i, (s, role)) in spans.iter().zip(&roles).enumerate() {
        if let Role::Candidate(_) = role {
            let entry = log_roles
                .entry(logs::span_key(&s.span.trace_id, &s.span.span_id))
                .or_insert_with(|| logs::Role::Units(Vec::new()));
            if let logs::Role::Units(list) = entry {
                list.push(i);
            }
        }
    }
    for (s, role) in spans.iter().zip(&roles) {
        if *role == Role::Absorbed {
            log_roles
                .entry(logs::span_key(&s.span.trace_id, &s.span.span_id))
                .or_insert(logs::Role::Absorbed);
        }
    }
    let indexed = logs::index_logs(&parsed);
    let all_logs = logs::by_trace(&indexed);
    let logs::Partition {
        mut unit_logs,
        orphans,
    } = logs::partition(indexed, &log_roles);
    let (orphan_units, orphan_unclaimed) = logs::orphan_units(orphans, profiles);
    let trace_view = |trace_id: &str| {
        TraceView::new(slice_of(&by_trace, trace_id), slice_of(&all_logs, trace_id))
    };

    let unclaimed_spans = (0..spans.len())
        .filter(|&i| roles[i] == Role::Unclaimed && first_copy(&spans[i], i, &index))
        .count();
    let mut out = ReadOutcome {
        unclaimed: unclaimed_spans + orphan_unclaimed,
        ..Default::default()
    };
    // Generation-id dedupe: the better-ranked profile wins, then the first
    // in input order (an app-side semconv span shares OpenRouter's `gen-…` id).
    let mut dedupe = Dedupe::default();
    let mut cx = ReadCx::default();
    for (i, (s, role)) in spans.iter().zip(&roles).enumerate() {
        let Role::Candidate(rank) = *role else {
            continue;
        };
        let unit = Unit {
            resource: s.resource,
            scope: s.scope,
            span: Some(s.span),
            logs: unit_logs.remove(&i).unwrap_or_default(),
        };
        let trace = trace_view(&s.span.trace_id);
        run_unit(
            profiles[rank],
            rank,
            &unit,
            &trace,
            &mut cx,
            &mut dedupe,
            &mut out,
        );
    }

    for orphan in orphan_units {
        let first = orphan.logs[0];
        let trace_id = &*first.record.trace_id;
        let trace = if trace_id.is_empty() {
            TraceView::default()
        } else {
            trace_view(trace_id)
        };
        let unit = Unit {
            resource: first.resource,
            scope: first.scope,
            span: None,
            logs: orphan.logs,
        };
        run_unit(
            profiles[orphan.profile],
            orphan.profile,
            &unit,
            &trace,
            &mut cx,
            &mut dedupe,
            &mut out,
        );
    }
    out.generations = dedupe.kept.into_iter().flatten().collect();
    Ok(out)
}

#[derive(Default)]
struct Dedupe {
    /// id -> (rank, slot in `kept`, the kept unit's ident).
    seen: HashMap<String, (usize, usize, Ident)>,
    kept: Vec<Option<Generation>>,
}

fn slice_of<'m, T>(m: &'m HashMap<String, Vec<T>>, key: &str) -> &'m [T] {
    m.get(key).map_or(&[], Vec::as_slice)
}

fn run_unit<'a>(
    p: &dyn Profile,
    rank: usize,
    unit: &Unit<'a>,
    trace: &TraceView<'a>,
    cx: &mut ReadCx<'a>,
    dedupe: &mut Dedupe,
    out: &mut ReadOutcome,
) {
    let skip = |reason: SkipReason, ident: &Ident, by_profile: bool| Skipped {
        generation_id: ident.generation_id.clone(),
        session_id: ident.session_id.clone(),
        profile: by_profile.then(|| p.name().to_string()),
        reason,
    };
    if let Some(reason) = p.pre_skip(unit) {
        out.skipped.push(skip(reason, &Ident::default(), true));
        return;
    }
    let ident = p.identify(unit);
    if unit
        .span
        .and_then(|s| s.status.as_ref())
        .is_some_and(|st| st.is_error())
    {
        out.skipped
            .push(skip(SkipReason::ErrorStatus, &ident, false));
        return;
    }
    if ident
        .generation_id
        .as_ref()
        .and_then(|id| dedupe.seen.get(id))
        .is_some_and(|(kept_rank, ..)| *kept_rank <= rank)
    {
        out.skipped.push(skip(SkipReason::Duplicate, &ident, false));
        return;
    }
    match p.extract_with(unit, trace, cx) {
        Ok(mut g) => {
            g.profile = p.name().to_string();
            // Only after a successful extract, so a truncated first
            // copy never suppresses a good redelivery.
            if let Some((kept_rank, slot, old)) = dedupe.seen.remove(&g.id) {
                if kept_rank <= rank {
                    // Same id reached under a different ident key: the
                    // earlier, at-least-as-good unit stays.
                    dedupe.seen.insert(g.id.clone(), (kept_rank, slot, old));
                    out.skipped.push(skip(SkipReason::Duplicate, &ident, false));
                    return;
                }
                dedupe.kept[slot] = None;
                out.skipped.push(Skipped {
                    generation_id: old.generation_id.or_else(|| Some(g.id.clone())),
                    session_id: old.session_id,
                    profile: None,
                    reason: SkipReason::Duplicate,
                });
            }
            dedupe
                .seen
                .insert(g.id.clone(), (rank, dedupe.kept.len(), ident));
            dedupe.kept.push(Some(g));
        }
        Err(reason) => out.skipped.push(skip(reason, &ident, true)),
    }
}

/// First consulted profile that claims the span, else the first that
/// absorbs it, in rank order.
fn resolve(profiles: &[&dyn Profile], s: &SpanRef<'_>) -> Role {
    for (rank, p) in profiles.iter().enumerate() {
        if p.claims(s.resource, s.scope, s.span) {
            return Role::Candidate(rank);
        }
        if p.absorbs(s.resource, s.scope, s.span) {
            return Role::Absorbed;
        }
    }
    Role::Unclaimed
}

/// Longest `parentSpanId` walk the ancestor rule follows.
pub const MAX_ANCESTOR_DEPTH: usize = 4096;

/// `(traceId, spanId)` → first span in input order with that id.
fn span_index<'s>(spans: &'s [SpanRef<'_>]) -> HashMap<(&'s str, &'s str), usize> {
    let mut index = HashMap::new();
    for (i, s) in spans.iter().enumerate() {
        if !s.span.span_id.is_empty() {
            index
                .entry((&*s.span.trace_id, &*s.span.span_id))
                .or_insert(i);
        }
    }
    index
}

/// Whether `spans[i]` is the first copy of its `(traceId, spanId)`; a span
/// with no span id is always its own.
fn first_copy(s: &SpanRef<'_>, i: usize, index: &HashMap<(&str, &str), usize>) -> bool {
    s.span.span_id.is_empty() || index.get(&(&*s.span.trace_id, &*s.span.span_id)) == Some(&i)
}

/// The outermost ancestor of `start` that is a candidate ranked at or
/// before it. A cycle or a walk past [`MAX_ANCESTOR_DEPTH`] yields `None`,
/// so malformed parent links never cost a unit.
fn absorbing_ancestor(
    start: usize,
    spans: &[SpanRef<'_>],
    roles: &[Role],
    index: &HashMap<(&str, &str), usize>,
) -> Option<usize> {
    let Role::Candidate(rank) = roles[start] else {
        return None;
    };
    let trace = &*spans[start].span.trace_id;
    let mut visited = HashSet::from([start]);
    let mut outermost = None;
    let mut cur = start;
    for _ in 0..MAX_ANCESTOR_DEPTH {
        let parent = &*spans[cur].span.parent_span_id;
        if parent.is_empty() {
            return outermost;
        }
        let Some(&p) = index.get(&(trace, parent)) else {
            return outermost;
        };
        if !visited.insert(p) {
            return None;
        }
        if let Role::Candidate(r) = roles[p]
            && r <= rank
        {
            outermost = Some(p);
        }
        cur = p;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attr(k: &str, v: &str) -> Value {
        json!({"key": k, "value": {"stringValue": v}})
    }

    fn root(id: &str, prompt: &str) -> Value {
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
            "traceId": id, "name": "LLM Generation", "status": {"code": 1},
            "attributes": [
                attr("gen_ai.response.id", id),
                attr("session.id", "s1"),
                attr("gen_ai.prompt", prompt),
                attr("gen_ai.completion", r#"{"completion":"ok"}"#)
            ]
        }]}]}]})
    }

    const GOOD: &str = r#"{"messages":[{"role":"user","content":"hi"}]}"#;

    fn read(values: &[Value]) -> ReadOutcome {
        read_deliveries(values, ProfileSelection::Auto).unwrap()
    }

    #[test]
    fn a_truncated_first_copy_does_not_suppress_a_good_redelivery() {
        let out = read(&[root("g1", r#"{"messages":[{"role""#), root("g1", GOOD)]);
        assert_eq!(out.generations.len(), 1);
        assert_eq!(out.generations[0].id, "g1");
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].reason, SkipReason::Truncated);
    }

    #[test]
    fn skips_name_the_profile_that_made_them() {
        let mut err = root("g2", GOOD);
        err["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["status"] = json!({"code": 2});
        let conn = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"name": "openrouter-connection-test"}
        ]}]}]});
        let out = read(&[err, conn, root("g3", GOOD), root("g3", GOOD)]);
        let got: Vec<(SkipReason, Option<&str>)> = out
            .skipped
            .iter()
            .map(|s| (s.reason, s.profile.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (SkipReason::ErrorStatus, None),
                (SkipReason::ConnectionTest, Some("openrouter")),
                (SkipReason::Duplicate, None),
            ]
        );
        assert_eq!(out.generations[0].profile, "openrouter");
    }

    #[test]
    fn named_openrouter_equals_auto_on_openrouter_spans() {
        let a = read_deliveries(&[root("g6", GOOD)], ProfileSelection::OpenRouter).unwrap();
        assert_eq!(a.generations, read(&[root("g6", GOOD)]).generations);
    }

    #[test]
    fn skip_reasons_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(SkipReason::MissingPayload).unwrap(),
            json!("missing_payload")
        );
    }

    use crate::otlp::{Resource, Scope, Span};

    /// Test-only profile: claims spans whose name starts with `.1`,
    /// absorbs nothing; the generation id is the span id.
    struct Prefix(&'static str, &'static str);

    impl Profile for Prefix {
        fn name(&self) -> &'static str {
            self.0
        }
        fn claims(&self, _: &Resource, _: &Scope, span: &Span) -> bool {
            span.name.starts_with(self.1)
        }
        fn identify(&self, unit: &Unit<'_>) -> Ident {
            Ident {
                generation_id: unit.span.map(|s| s.span_id.to_string()),
                session_id: None,
            }
        }
        fn extract(
            &self,
            unit: &Unit<'_>,
            _: &TraceView<'_>,
        ) -> std::result::Result<Generation, SkipReason> {
            Ok(Generation {
                id: unit.span.map(|s| s.span_id.to_string()).unwrap_or_default(),
                ..Default::default()
            })
        }
    }

    fn span(id: &str, parent: &str, name: &str) -> Value {
        json!({"traceId": "t", "spanId": id, "parentSpanId": parent, "name": name})
    }

    fn batch(spans: Vec<Value>) -> Value {
        json!({"resourceSpans": [{"scopeSpans": [{"spans": spans}]}]})
    }

    fn units(profiles: &[&dyn Profile], d: &Value) -> (Vec<String>, usize) {
        let out = read_with(std::iter::once(d), profiles).unwrap();
        (
            out.generations.iter().map(|g| g.id.clone()).collect(),
            out.unclaimed,
        )
    }

    #[test]
    fn outermost_claimed_span_wins_within_a_profile() {
        let all = Prefix("all", "");
        let d = batch(vec![
            span("leaf", "mid", "x"),
            span("mid", "root", "x"),
            span("root", "", "x"),
        ]);
        assert_eq!(units(&[&all], &d), (vec!["root".to_string()], 0));
    }

    #[test]
    fn a_later_ranked_candidate_under_an_earlier_ranked_ancestor_is_absorbed() {
        let (a, b) = (Prefix("a", "a"), Prefix("b", "b"));
        let d = batch(vec![span("a1", "", "a-root"), span("b1", "a1", "b-child")]);
        assert_eq!(units(&[&a, &b], &d), (vec!["a1".to_string()], 0));
    }

    #[test]
    fn an_earlier_ranked_candidate_under_a_later_ranked_ancestor_stays_a_unit() {
        let (a, b) = (Prefix("a", "a"), Prefix("b", "b"));
        let d = batch(vec![span("b2", "", "b-root"), span("a2", "b2", "a-child")]);
        assert_eq!(units(&[&a, &b], &d).0, vec!["b2", "a2"]);
    }

    #[test]
    fn the_walk_crosses_unclaimed_spans_and_stops_at_a_missing_parent() {
        let a = Prefix("a", "a");
        let d = batch(vec![
            span("a1", "", "a-top"),
            span("x1", "a1", "other"),
            span("a2", "x1", "a-deep"),
            span("a3", "gone", "a-orphan"),
        ]);
        assert_eq!(
            units(&[&a], &d),
            (vec!["a1".to_string(), "a3".to_string()], 1)
        );
    }

    #[test]
    fn parent_cycles_and_self_parents_terminate_and_keep_their_units() {
        let all = Prefix("all", "");
        let d = batch(vec![
            span("s", "s", "x"),
            span("p", "q", "x"),
            span("q", "p", "x"),
        ]);
        assert_eq!(units(&[&all], &d).0, vec!["s", "p", "q"]);
    }

    #[test]
    fn a_chain_deeper_than_the_limit_voids_the_rule() {
        let c = Prefix("c", "c");
        let mut spans = vec![span("n0", "", "c-top")];
        for i in 1..=5000 {
            spans.push(span(&format!("n{i}"), &format!("n{}", i - 1), "link"));
        }
        spans.push(span("leaf", "n5000", "c-leaf"));
        let (ids, unclaimed) = units(&[&c], &batch(spans));
        assert_eq!(ids, vec!["n0", "leaf"]);
        assert_eq!(unclaimed, 5000);
    }

    #[test]
    fn log_records_count_as_unclaimed_and_metrics_are_ignored() {
        let logs = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {}, {"timeUnixNano": "1"}, {"traceId": "t"}
        ]}]}]});
        let metrics = json!({"resourceMetrics": [{"scopeMetrics": [{"metrics": [{}]}]}]});
        let out = read(&[logs, metrics, root("g7", GOOD)]);
        assert_eq!(out.unclaimed, 3);
        assert_eq!(out.generations.len(), 1);
        assert!(out.skipped.is_empty());
        // Identical records are deduplicated before counting.
        let pair = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{}, {}]}]}]});
        assert_eq!(read(&[pair]).unclaimed, 1);
    }

    #[test]
    fn a_span_redelivered_in_uppercase_dedupes_and_an_uppercase_parent_absorbs() {
        let all = Prefix("all", "");
        let lower = batch(vec![json!({"traceId": "ab", "spanId": "a1", "name": "x"})]);
        let upper = batch(vec![
            json!({"traceId": "AB", "spanId": "A1", "name": "x"}),
            json!({"traceId": "AB", "spanId": "c1", "parentSpanId": "A1", "name": "x"}),
        ]);
        let out = read_with(&[lower, upper], &[&all]).unwrap();
        let ids: Vec<&str> = out.generations.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["a1"]);
        assert_eq!(out.unclaimed, 0);
        let reasons: Vec<SkipReason> = out.skipped.iter().map(|s| s.reason).collect();
        assert_eq!(reasons, [SkipReason::Duplicate]);
    }

    #[test]
    fn span_ids_and_trace_ids_reach_generations_lowercased() {
        let chat = |trace: &str, span: &str| {
            batch(vec![
                json!({"traceId": trace, "spanId": span, "name": "chat m",
                "attributes": [attr("gen_ai.operation.name", "chat")]}),
            ])
        };
        let values = [chat("AB01", "C0FFEE"), chat("ab01", "c0ffee")];
        let out = read_deliveries(&values, ProfileSelection::Semconv).unwrap();
        assert_eq!(out.generations.len(), 1, "{:?}", out.skipped);
        assert_eq!(out.generations[0].id, "span-c0ffee");
        assert_eq!(out.generations[0].trace_id, "ab01");
    }

    #[test]
    fn an_unclaimed_span_counts_once_however_often_delivered() {
        let none = Prefix("none", "claimed");
        let d = batch(vec![
            span("u1", "", "x"),
            json!({"traceId": "T", "spanId": "U1", "name": "x"}),
            span("", "", "x"),
            span("", "", "x"),
        ]);
        let out = read_with(&[d.clone(), d], &[&none]).unwrap();
        assert_eq!(out.unclaimed, 1 + 4, "u1 once; each id-less span counts");
    }

    /// Test-only profile: claims `unit` spans, absorbs `aux` spans, and
    /// reports how many `aux` spans its `TraceView` shows.
    struct CountAux;

    impl Profile for CountAux {
        fn name(&self) -> &'static str {
            "count-aux"
        }
        fn claims(&self, _: &Resource, _: &Scope, span: &Span) -> bool {
            span.name == "unit"
        }
        fn absorbs(&self, _: &Resource, _: &Scope, span: &Span) -> bool {
            span.name == "aux"
        }
        fn identify(&self, unit: &Unit<'_>) -> Ident {
            Ident {
                generation_id: unit.span.map(|s| s.span_id.to_string()),
                session_id: None,
            }
        }
        fn extract(
            &self,
            _: &Unit<'_>,
            trace: &TraceView<'_>,
        ) -> std::result::Result<Generation, SkipReason> {
            let n = trace.spans().filter(|r| r.span.name == "aux").count();
            let mut source_meta = serde_json::Map::new();
            source_meta.insert("aux".into(), json!(n));
            Ok(Generation {
                id: "unit".into(),
                source_meta,
                ..Default::default()
            })
        }
    }

    #[test]
    fn trace_view_sees_each_trace_and_span_id_once() {
        let d = |spans: Value| json!({"resourceSpans": [{"scopeSpans": [{"spans": spans}]}]});
        let unit = json!({"traceId": "t", "spanId": "u1", "name": "unit"});
        let aux = json!({"traceId": "t", "spanId": "x1", "name": "aux"});
        let other = json!({"traceId": "t", "spanId": "x2", "name": "aux"});
        let values = [d(json!([unit, aux.clone()])), d(json!([aux, other]))];
        let out = read_with(&values, &[&CountAux]).unwrap();
        assert_eq!(
            out.generations[0].source_meta["aux"], 2,
            "x1 twice counts once; x2 once"
        );
    }
}
