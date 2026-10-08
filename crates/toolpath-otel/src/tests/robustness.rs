//! Mutation robustness: no single structural mutation of a fixture delivery
//! may panic the reader, fail the batch (unless the delivery stops being
//! OTLP), or turn the committed error span into a generation.

use super::common::*;
use crate::tests::otel::{OtelError, ProfileSelection, ReadOutcome, SkipReason};
use serde_json::{Map, Value, json};
use std::panic::{self, AssertUnwindSafe};

const MAX_DEPTH: usize = 6;
const MAX_TARGETS: usize = 500; // × 4 replacements ≈ 2000 mutations

#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Idx(usize),
}

fn collect(v: &Value, max_depth: usize, path: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    if !matches!(v, Value::Object(_) | Value::Array(_)) {
        return;
    }
    out.push(path.clone());
    if path.len() == max_depth {
        return;
    }
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                path.push(Seg::Key(k.clone()));
                collect(child, max_depth, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(Seg::Idx(i));
                collect(child, max_depth, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn mutated(root: &Value, path: &[Seg], replacement: Value) -> Value {
    let mut out = root.clone();
    let mut cur = &mut out;
    for seg in path {
        cur = match (cur, seg) {
            (Value::Object(m), Seg::Key(k)) => m.get_mut(k).unwrap(),
            (Value::Array(a), Seg::Idx(i)) => a.get_mut(*i).unwrap(),
            _ => unreachable!(),
        };
    }
    *cur = replacement;
    out
}

fn pointer(path: &[Seg]) -> String {
    path.iter()
        .map(|s| match s {
            Seg::Key(k) => format!("/{k}"),
            Seg::Idx(i) => format!("/{i}"),
        })
        .collect()
}

/// Every delivery in every fixture file carrying OTLP, in file-name order.
fn all_deliveries() -> Vec<(String, Value)> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ndjson") || n.ends_with(".json"))
        .collect();
    names.sort();
    names
        .into_iter()
        .flat_map(|n| {
            deliveries(&n)
                .into_iter()
                .enumerate()
                .map(move |(i, v)| (format!("{n}#{i}"), v))
        })
        .filter(|(_, v)| v.get("resourceSpans").is_some())
        .collect()
}

fn read_caught(v: &Value) -> std::thread::Result<Result<ReadOutcome, OtelError>> {
    panic::catch_unwind(AssertUnwindSafe(|| read_deliveries(std::iter::once(v))))
}

/// The generation id of the error span in `codex-error-span.json`.
fn error_span_id(error_span: &Value) -> String {
    let out = read_deliveries(std::slice::from_ref(error_span)).unwrap();
    assert!(out.generations.is_empty());
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].reason, SkipReason::ErrorStatus);
    out.skipped[0].generation_id.clone().unwrap()
}

fn error_span_status_path(error_span: &Value) -> Vec<Seg> {
    let spans = error_span["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap();
    let i = spans
        .iter()
        .position(|s| s["name"] == "LLM Generation")
        .unwrap();
    ["resourceSpans", "0", "scopeSpans", "0", "spans"]
        .iter()
        .map(|s| match s.parse() {
            Ok(n) => Seg::Idx(n),
            Err(_) => Seg::Key(s.to_string()),
        })
        .chain([Seg::Idx(i), Seg::Key("status".into())])
        .collect()
}

#[test]
fn malformed_status_on_the_error_span_is_never_read() {
    let error_span = deliveries("codex-error-span.json").remove(0);
    let id = error_span_id(&error_span);
    let status = error_span_status_path(&error_span);
    for bad in [
        json!(2),
        json!("STATUS_CODE_ERROR"),
        json!("weird"),
        json!("x"),
        json!([]),
        json!(true),
        json!({"code": "ERROR"}),
        json!({"code": "STATUS_CODE_ERROR "}),
        json!({"code": "status_code_error"}),
        json!({"code": "2"}),
        json!({"code": 7}),
        json!({"code": []}),
    ] {
        let out = read_deliveries(&[mutated(&error_span, &status, bad.clone())]).unwrap();
        assert!(
            out.generations.iter().all(|g| g.id != id),
            "status {bad} read the error span as a generation"
        );
        assert_eq!(out.skipped[0].reason, SkipReason::ErrorStatus, "{bad}");
    }
}

#[test]
fn single_mutations_never_panic_or_abort_and_never_read_the_error_span() {
    let all = all_deliveries();
    let error_span = deliveries("codex-error-span.json").remove(0);
    let error_id = error_span_id(&error_span);
    let status = pointer(&error_span_status_path(&error_span));

    let mut targets: Vec<(usize, Vec<Seg>)> = Vec::new();
    for (d, (_, v)) in all.iter().enumerate() {
        let mut found = Vec::new();
        collect(v, MAX_DEPTH, &mut Vec::new(), &mut found);
        targets.extend(found.into_iter().map(|p| (d, p)));
    }
    // Deterministic fixed stride over every candidate.
    let stride = targets.len().div_ceil(MAX_TARGETS).max(1);
    let replacements = [
        Value::Null,
        json!("x"),
        Value::Object(Map::new()),
        Value::Array(Vec::new()),
    ];

    let mut error_targets = Vec::new();
    collect(&error_span, usize::MAX, &mut Vec::new(), &mut error_targets);
    assert!(error_targets.iter().any(|p| pointer(p) == status));

    // Silence the expected-panic noise only while probing; every assert
    // runs after the hook is restored so its message is not swallowed.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let mut tried = 0;
    let check = |name: &str, path: &[Seg], v: &Value, failures: &mut Vec<String>| {
        let otlp = crate::tests::otel::otlp::is_otlp(v);
        match read_caught(v) {
            Err(_) => failures.push(format!("panic at {name} {}", pointer(path))),
            Ok(Err(e)) if otlp => failures.push(format!("Err {e} at {name} {}", pointer(path))),
            Ok(Err(_)) => {}
            Ok(Ok(_)) if !otlp => {
                failures.push(format!("expected NotOtlp at {name} {}", pointer(path)))
            }
            Ok(Ok(_)) => {}
        }
    };
    for (d, path) in targets.iter().step_by(stride) {
        let (name, root) = &all[*d];
        for r in &replacements {
            tried += 1;
            check(name, path, &mutated(root, path, r.clone()), &mut failures);
        }
    }
    // Every mutation of the error span at any depth (its status sits at
    // depth 7), unsampled: none may read it.
    for path in &error_targets {
        for r in &replacements {
            tried += 1;
            let v = mutated(&error_span, path, r.clone());
            check("codex-error-span.json", path, &v, &mut failures);
            // A `null` or `{}` status is proto3's UNSET, a legitimately
            // readable span; every other mutation must keep it unread.
            let unset = pointer(path) == status && matches!(r, Value::Null | Value::Object(_));
            if !unset
                && let Ok(Ok(out)) = read_caught(&v)
                && out.generations.iter().any(|g| g.id == error_id)
            {
                failures.push(format!("error span read at {} = {r}", pointer(path)));
            }
        }
    }
    panic::set_hook(hook);
    assert!(tried > 1000, "only {tried} mutations");
    assert!(
        failures.is_empty(),
        "{tried} mutations:\n{}",
        failures.join("\n")
    );
}

/// Random `parentSpanId` rewiring (cycles, self-parents, dangling ids)
/// over one shared trace: never panics, never errs, never hangs.
#[test]
fn random_parent_links_never_panic_err_or_hang() {
    let base: Vec<Value> = ["claude-code.ndjson", "synthetic-fork.ndjson"]
        .iter()
        .flat_map(|f| deliveries(f))
        .collect();
    let mut ids: Vec<String> = vec![String::new(), "ffffffffffffffff".into()];
    for d in &base {
        for rs in d["resourceSpans"].as_array().unwrap() {
            for ss in rs["scopeSpans"].as_array().unwrap() {
                for sp in ss["spans"].as_array().unwrap() {
                    ids.push(sp["spanId"].as_str().unwrap().to_string());
                }
            }
        }
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // Silence probed panics; report only after the hook is restored.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    for round in 0..200 {
        let mut batch = base.clone();
        for d in &mut batch {
            for rs in d["resourceSpans"].as_array_mut().unwrap() {
                for ss in rs["scopeSpans"].as_array_mut().unwrap() {
                    for sp in ss["spans"].as_array_mut().unwrap() {
                        sp["traceId"] = json!("shared");
                        sp["parentSpanId"] = json!(ids[(next() as usize) % ids.len()]);
                    }
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = panic::catch_unwind(AssertUnwindSafe(|| read_deliveries(&batch)));
            let _ = tx.send(match r {
                Ok(Ok(_)) => "ok",
                Ok(Err(_)) => "err",
                Err(_) => "panic",
            });
        });
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok("ok") => {}
            Ok(other) => failure = Some(format!("round {round}: {other}")),
            Err(_) => failure = Some(format!("round {round}: hung")),
        }
        if failure.is_some() {
            break;
        }
    }
    panic::set_hook(hook);
    if let Some(f) = failure {
        panic!("{f}");
    }
}

/// A spans delivery and a logs delivery that correlate: one semconv-shaped
/// details record (structured content), one legacy per-role record, one
/// orphan, one id-less record. Content keys are plain data here; no profile
/// has to claim anything for the sweep to be meaningful.
fn logs_pair() -> (Value, Value) {
    let t = "0102030405060708090a0b0c0d0e0f10";
    let spans = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        {"traceId": t, "spanId": "a1a2a3a4a5a6a7a8", "name": "chat m",
         "startTimeUnixNano": "1", "endTimeUnixNano": "2",
         "attributes": [{"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}}]}
    ]}]}]});
    // Built in two parts: one literal nests past `json!`'s recursion limit.
    let part = json!({"kvlistValue": {"values": [
        {"key": "type", "value": {"stringValue": "text"}},
        {"key": "content", "value": {"stringValue": "hi"}}]}});
    let message = json!({"kvlistValue": {"values": [
        {"key": "role", "value": {"stringValue": "user"}},
        {"key": "parts", "value": {"arrayValue": {"values": [part]}}}]}});
    let logs = json!({"resourceLogs": [{"resource": {"attributes": []}, "scopeLogs": [{"logRecords": [
        {"timeUnixNano": "3", "traceId": t, "spanId": "a1a2a3a4a5a6a7a8",
         "eventName": "gen_ai.client.inference.operation.details",
         "attributes": [{"key": "gen_ai.input.messages", "value": {"arrayValue": {"values": [message]}}}]},
        {"timeUnixNano": "4", "traceId": t, "spanId": "a1a2a3a4a5a6a7a8", "eventName": "gen_ai.user.message",
         "body": {"kvlistValue": {"values": [{"key": "content", "value": {"stringValue": "hi"}}]}}},
        {"timeUnixNano": "5", "traceId": t, "spanId": "ffffffffffffffff", "eventName": "gen_ai.choice",
         "body": {"kvlistValue": {"values": [{"key": "index", "value": {"intValue": "0"}}]}}},
        {"body": {"stringValue": "no ids"}}
    ]}]}]});
    (spans, logs)
}

#[test]
fn log_mutations_never_panic_or_fail_the_batch() {
    let (spans, logs) = logs_pair();
    let mut targets = Vec::new();
    collect(&logs, MAX_DEPTH, &mut Vec::new(), &mut targets);
    targets.truncate(MAX_TARGETS);
    let replacements = [Value::Null, json!({}), json!([]), json!("x"), json!(-1)];
    for path in &targets {
        for r in &replacements {
            let m = mutated(&logs, path, r.clone());
            let opts = ProfileSelection::Auto;
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                crate::tests::otel::read_deliveries([&spans, &m], opts)
            }));
            let result = result.unwrap_or_else(|_| panic!("panic at {} = {r}", pointer(path)));
            // A mutation that breaks the top-level shape (the root, or
            // `resourceLogs` itself) is NotOtlp, as in the sweep above;
            // anything still OTLP-shaped must read.
            let otlp = crate::tests::otel::otlp::is_otlp(&m);
            match result {
                Ok(_) if otlp => {}
                Err(OtelError::NotOtlp) if !otlp => {}
                Ok(_) => panic!("{} = {r}: expected NotOtlp", pointer(path)),
                Err(e) => panic!("{} = {r}: {e}", pointer(path)),
            }
        }
    }
}
