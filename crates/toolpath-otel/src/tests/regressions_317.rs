//! The PR #315 review findings whose failure scenarios start from OTLP
//! request bodies, re-checked through the record and JSONL entry points:
//! `read_generations` (one call per delivery, records through JSON),
//! `derive_path_from_records` and `derive_jsonl`. The scenarios that start
//! from generations are in `jsonl/tests/regressions.rs`.

use crate::hash::canonical_json;
use crate::{
    BatchLimits, DeriveConfig, GenerationBatch, GenerationRecord, ProfileSelection, Remote, Settle,
    SkipCounts,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use toolpath::v1::Path;

fn through_json(b: &GenerationBatch) -> GenerationBatch {
    serde_json::from_str(&serde_json::to_string(b).unwrap()).unwrap()
}

fn cfg(profile: ProfileSelection) -> DeriveConfig {
    DeriveConfig {
        profile,
        ..crate::tests::otel::classified()
    }
}

/// What a store holds after reading `requests` one delivery at a time.
fn stored_with(requests: &[Value], profile: ProfileSelection) -> (GenerationBatch, SkipCounts) {
    let mut all = GenerationBatch::default();
    let mut skipped = SkipCounts::default();
    for r in requests {
        let d = crate::read_generations(std::slice::from_ref(r), profile).unwrap();
        skipped.add(&d.skipped);
        let b = through_json(&d.output);
        all.records.extend(b.records);
        for (h, m) in b.messages {
            all.messages.entry(h).or_insert(m);
        }
    }
    (all, skipped)
}

fn stored(requests: &[Value]) -> GenerationBatch {
    stored_with(requests, ProfileSelection::Auto).0
}

fn from_records(b: &GenerationBatch, config: &DeriveConfig) -> Path {
    crate::derive_path_from_records(&b.records, |h| b.messages.get(h), config)
        .unwrap()
        .output
}

/// One `Settle::Final` send to an empty store, read back.
fn final_jsonl(b: &GenerationBatch, config: &DeriveConfig) -> Path {
    let mut stream = Stream {
        config: config.clone(),
        ..Default::default()
    };
    stream.send(b, true);
    stream.path()
}

/// A stateless caller in front of a store that keeps every accepted body.
#[derive(Default)]
struct Stream {
    config: DeriveConfig,
    text: String,
}

impl Stream {
    fn path(&self) -> Path {
        Path::from_jsonl_str(&self.text).unwrap()
    }

    /// What the store says about the path: whether it exists, its feed
    /// order and harness from its meta, and every step id it holds.
    fn remote(&self) -> Remote {
        if self.text.is_empty() {
            return Remote::default();
        }
        let p = self.path();
        let otel = meta_otel(&p);
        Remote {
            opened: true,
            fed: serde_json::from_value(otel["generation_ids"].clone()).unwrap(),
            stored: p
                .steps
                .iter()
                .map(|s| s.step.id.clone())
                .collect::<HashSet<_>>(),
            harness: otel["harness"].as_str().map(str::to_string),
            base: HashSet::new(),
        }
    }

    fn send(&mut self, b: &GenerationBatch, final_: bool) {
        let settle = if final_ {
            Settle::Final
        } else {
            Settle::Settled
        };
        let bodies = match crate::derive_jsonl(
            &b.records,
            |h| b.messages.get(h),
            &self.config,
            &self.remote(),
            settle,
            BatchLimits::new(None, None),
        ) {
            Ok(d) => d.output,
            Err(crate::OtelError::NoGenerations { .. }) => Vec::new(),
            Err(e) => panic!("{e}"),
        };
        for body in bodies {
            self.text += &body.text;
        }
    }
}

/// Step changes and extras are hash maps: compare the key-sorted form.
fn bytes(p: &Path) -> String {
    canonical_json(&serde_json::to_value(p).unwrap())
}

fn meta_otel(p: &Path) -> Value {
    p.meta.as_ref().unwrap().extra["otel"].clone()
}

/// The id of the step generation `gid` produced.
fn produced(p: &Path, gid: &str) -> Option<String> {
    p.steps
        .iter()
        .find(|s| {
            s.change.values().any(|c| {
                c.structural
                    .as_ref()
                    .and_then(|st| st.extra.get("otel"))
                    .is_some_and(|o| o["generation_id"] == gid)
            })
        })
        .map(|s| s.step.id.clone())
}

/// Every order of `items`.
fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first);
            out.push(p);
        }
    }
    out
}

// OpenRouter Broadcast request bodies.
// ---------------------------------------------------------------------------

const GOOD: &str = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
const CUT: &str = r#"{"messages":[{"role""#;

/// One OpenRouter generation span; no `session.id` when `session` is empty.
fn request(id: &str, start: u64, session: &str, prompt: &str, completion: &str) -> Value {
    let attr = |k: &str, v: &str| json!({"key": k, "value": {"stringValue": v}});
    let mut attrs = vec![
        attr("gen_ai.response.id", id),
        attr("gen_ai.prompt", prompt),
        attr("gen_ai.completion", completion),
        json!({"key": "gen_ai.usage.total_cost", "value": {"doubleValue": 0.5}}),
    ];
    if !session.is_empty() {
        attrs.push(attr("session.id", session));
    }
    json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": format!("t-{id}"), "spanId": "r", "name": "LLM Generation",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 1).to_string(),
        "attributes": attrs
    }]}]}]})
}

/// `[user hi, assistant hello, user k]`: continues `GOOD`'s answer.
fn next_prompt(k: &str) -> String {
    json!({"messages": [
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": "hello"},
        {"role": "user", "content": k}
    ]})
    .to_string()
}

const HELLO: &str = r#"{"completion":"hello"}"#;

// Finding 4187996108 (B1, graph title for a single session): no #317 entry
// point builds a Graph, so `title` has no record or JSONL door.

// Findings 4187996116 / 4187996198 (B2, an id-less session's truncation).
// ---------------------------------------------------------------------------

#[test]
fn records_mark_an_idless_session_truncated() {
    let requests = [
        request("g1", 10, "", GOOD, HELLO),
        request("g2", 20, "", CUT, HELLO),
    ];
    let one = crate::derive_path(&requests, &crate::tests::otel::classified()).unwrap();
    assert_eq!(meta_otel(&one.output)["truncated"], true);
    let b = stored(&requests);
    assert!(b.records.iter().any(GenerationRecord::is_truncated));
    let rec = from_records(&b, &crate::tests::otel::classified());
    assert_eq!(
        meta_otel(&rec)["truncated"],
        true,
        "derive_path_from_records"
    );
    let sent = final_jsonl(&b, &crate::tests::otel::classified());
    assert_eq!(meta_otel(&sent)["truncated"], true, "derive_jsonl");
    assert_eq!(bytes(&rec), bytes(&one.output));
}

#[test]
fn a_truncation_marker_without_an_id_marks_a_session_that_has_one() {
    // The cut call carries neither a session id nor a generation id.
    let requests = [
        request("g1", 10, "s", GOOD, HELLO),
        request("", 20, "", CUT, HELLO),
        request(
            "g3",
            30,
            "s",
            &next_prompt("more"),
            r#"{"completion":"ok"}"#,
        ),
    ];
    let one = crate::derive_path(&requests, &crate::tests::otel::classified()).unwrap();
    let b = stored(&requests);
    let rec = from_records(&b, &crate::tests::otel::classified());
    assert_eq!(
        meta_otel(&rec)["truncated"],
        meta_otel(&one.output)["truncated"]
    );
    assert_eq!(bytes(&rec), bytes(&one.output));
}

/// The marker arrives in any delivery of a growing stream: the store's
/// path ends up marked whichever send carried it.
#[test]
fn a_streamed_truncation_marker_reaches_the_stored_meta() {
    let requests = [
        request("g1", 10, "", GOOD, HELLO),
        request("g2", 20, "", &next_prompt("more"), r#"{"completion":"ok"}"#),
        request("g3", 30, "", CUT, HELLO),
    ];
    for order in permutations(&[0, 1, 2]) {
        let all: Vec<Value> = order.iter().map(|&i| requests[i].clone()).collect();
        let mut stream = Stream::default();
        for k in 1..=all.len() {
            stream.send(&stored(&all[..k]), k == all.len());
        }
        assert_eq!(
            meta_otel(&stream.path())["truncated"],
            true,
            "{order:?}: the stored path is not marked truncated"
        );
    }
}

// Finding 4187996210 (B12, OpenInference `message.contents`).
// ---------------------------------------------------------------------------

fn kv(k: &str, v: &str) -> Value {
    json!({"key": k, "value": {"stringValue": v}})
}

/// An OpenInference LLM span with these attributes.
fn llm_span(trace: &str, span: &str, attrs: Vec<Value>) -> Value {
    let mut a = vec![
        kv("openinference.span.kind", "LLM"),
        kv("session.id", "oi-s"),
    ];
    a.extend(attrs);
    json!({"resourceSpans": [{"resource": {"attributes": [kv("service.name", "app")]},
        "scopeSpans": [{"scope": {"name": "openinference.instrumentation.openai"}, "spans": [
            {"traceId": trace, "spanId": span, "name": "ChatCompletion", "startTimeUnixNano": "1",
             "endTimeUnixNano": "2", "attributes": a, "status": {}}]}]}]})
}

#[test]
fn message_contents_parts_survive_records() {
    let part = |side: &str, p: usize, rest: &str, v: &str| {
        kv(
            &format!("llm.{side}_messages.0.message.contents.{p}.message_content.{rest}"),
            v,
        )
    };
    let span = llm_span(
        "t1",
        "abcd",
        vec![
            kv("llm.input_messages.0.message.role", "user"),
            part("input", 0, "type", "text"),
            part("input", 0, "text", "first"),
            part("input", 1, "type", "text"),
            part("input", 1, "text", "second"),
            kv("llm.output_messages.0.message.role", "assistant"),
            part("output", 0, "type", "text"),
            part("output", 0, "text", "A cat."),
        ],
    );
    let config = cfg(ProfileSelection::OpenInference);
    let one = crate::derive_path(std::slice::from_ref(&span), &config)
        .unwrap()
        .output;
    let (b, _) = stored_with(&[span], ProfileSelection::OpenInference);
    let rec = from_records(&b, &config);
    let sent = final_jsonl(&b, &config);
    for (p, what) in [(&rec, "records"), (&sent, "derive_jsonl")] {
        let text = serde_json::to_string(p).unwrap();
        assert!(
            text.contains("first\\nsecond") && text.contains("A cat."),
            "{what}"
        );
        assert_eq!(bytes(p), bytes(&one), "{what}");
    }
}

// Finding 4187996223 (B13, a non-object completion).
// ---------------------------------------------------------------------------

#[test]
fn a_non_object_completion_is_a_truncation_marker_in_records() {
    for raw in ["[1]", "\"text\"", "5", "null"] {
        let requests = [
            request("g1", 10, "s", GOOD, HELLO),
            request("g2", 20, "s", &next_prompt("more"), raw),
        ];
        let (b, skipped) = stored_with(&requests, ProfileSelection::Auto);
        assert_eq!(skipped.truncated, 1, "{raw}");
        assert_eq!(
            b.records.iter().filter(|r| r.is_truncated()).count(),
            1,
            "{raw}"
        );
        let rec = from_records(&b, &crate::tests::otel::classified());
        assert_eq!(meta_otel(&rec)["truncated"], true, "{raw}");
        assert!(produced(&rec, "g2").is_none(), "{raw}: an empty answer");
        let sent = final_jsonl(&b, &crate::tests::otel::classified());
        assert_eq!(bytes(&sent), bytes(&rec), "{raw}");
    }
}

// Findings 4187996330 / 4187996424 / 4187996486 (hex id case).
// ---------------------------------------------------------------------------

fn oi_span(trace: &str, span: &str) -> Value {
    llm_span(
        trace,
        span,
        vec![
            kv("llm.input_messages.0.message.role", "user"),
            kv("llm.input_messages.0.message.content", "hi"),
            kv("llm.output_messages.0.message.role", "assistant"),
            kv("llm.output_messages.0.message.content", "hello"),
        ],
    )
}

#[test]
fn a_span_redelivered_in_uppercase_is_one_generation_in_records() {
    let upper = oi_span("AB12CD", "ABCD01");
    let lower = oi_span("ab12cd", "abcd01");
    let config = cfg(ProfileSelection::OpenInference);
    for requests in [[upper.clone(), lower.clone()], [lower, upper]] {
        let one = crate::derive_path(&requests, &config).unwrap();
        let (b, _) = stored_with(&requests, ProfileSelection::OpenInference);
        let ids: Vec<&str> = b.records.iter().filter_map(|r| r.generation_id()).collect();
        assert!(
            ids.iter().all(|id| *id == id.to_ascii_lowercase()),
            "{ids:?}"
        );
        let rec =
            crate::derive_path_from_records(&b.records, |h| b.messages.get(h), &config).unwrap();
        assert_eq!(rec.skipped.duplicate, 1);
        assert_eq!(bytes(&rec.output), bytes(&one.output));
        assert_eq!(meta_otel(&rec.output)["trace_ids"], json!(["ab12cd"]));
        let d = crate::derive_jsonl(
            &b.records,
            |h| b.messages.get(h),
            &config,
            &Remote::default(),
            Settle::Final,
            BatchLimits::new(None, None),
        )
        .unwrap();
        assert_eq!(d.skipped.duplicate, 1);
    }
}

#[test]
fn an_unclaimed_span_redelivered_counts_once_in_one_read() {
    let other = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        {"traceId": "t9", "spanId": "02", "name": "http.request"}]}]}]});
    let upper = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        {"traceId": "T9", "spanId": "02", "name": "http.request"}]}]}]});
    let requests = [request("g1", 10, "s", GOOD, HELLO), other, upper];
    let one = crate::derive_path(&requests, &crate::tests::otel::classified()).unwrap();
    let read = crate::read_generations(&requests, ProfileSelection::Auto).unwrap();
    assert_eq!(read.skipped.unclaimed, 1);
    assert_eq!(read.skipped, one.skipped);
}

// Finding 4187996319 (which copy of a duplicate is kept).
// ---------------------------------------------------------------------------

/// An app-side semconv span reports a call OpenRouter's Broadcast also
/// reports (one `gen-…` id), with its input cut off.
fn semconv_cut(id: &str, start: u64) -> Value {
    let attr = |k: &str, v: &str| json!({"key": k, "value": {"stringValue": v}});
    json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": format!("app-{id}"), "spanId": "s1", "name": "chat",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 1).to_string(),
        "attributes": [
            attr("gen_ai.operation.name", "chat"), attr("gen_ai.response.id", id),
            attr("session.id", "s"), attr("gen_ai.input.messages", r#"[{"role""#)
        ]
    }]}]}]})
}

/// The better-ranked profile's copy of a call is kept whatever the record
/// order, and a worse-ranked profile's truncated copy of a kept call is a
/// duplicate, not a truncation, as one read of every delivery has it.
#[test]
fn a_duplicates_truncation_marker_does_not_depend_on_record_order() {
    let requests = [
        request("g1", 10, "s", GOOD, HELLO),
        request(
            "g2",
            20,
            "s",
            &next_prompt("more"),
            r#"{"completion":"ok"}"#,
        ),
        semconv_cut("g2", 20),
    ];
    let config = crate::tests::otel::classified();
    let one = crate::derive_path(&requests, &config).unwrap();
    for order in permutations(&[0, 1, 2]) {
        let arrived: Vec<Value> = order.iter().map(|&i| requests[i].clone()).collect();
        let read_once = crate::derive_path(&arrived, &config).unwrap();
        assert_eq!(
            bytes(&read_once.output),
            bytes(&one.output),
            "{order:?}: one read"
        );
        assert_eq!(
            read_once.skipped, one.skipped,
            "{order:?}: one read's skips"
        );
        let whole = crate::read_generations(&arrived, ProfileSelection::Auto).unwrap();
        assert_eq!(
            whole.skipped, one.skipped,
            "{order:?}: read_generations' skips"
        );
        let b = stored(&arrived);
        let rec = from_records(&b, &config);
        assert_eq!(
            meta_otel(&rec)["truncated"],
            meta_otel(&one.output)["truncated"],
            "{order:?}: truncated"
        );
        assert_eq!(bytes(&rec), bytes(&one.output), "{order:?}");
    }
}
