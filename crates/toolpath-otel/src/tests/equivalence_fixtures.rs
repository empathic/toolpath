//! Checks the re-encoded fixtures against the spec's Re-encoder table.

use super::common::*;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

const PAIRS: [(&str, &str); 6] = [
    ("claude-code.ndjson", "claude-code.ndjson"),
    ("codex.ndjson", "codex.ndjson"),
    ("opencode.ndjson", "opencode.ndjson"),
    ("pi.ndjson", "pi.ndjson"),
    ("synthetic-fork.ndjson", "synthetic-fork.ndjson"),
    ("codex-error-span.json", "codex-error-span.ndjson"),
];

fn reencoded(name: &str) -> Vec<Value> {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel/equivalence");
    std::fs::read_to_string(dir.join(name))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn spans(d: &Value) -> Vec<&Value> {
    d["resourceSpans"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|rs| rs["scopeSpans"].as_array().unwrap())
        .flat_map(|ss| ss["spans"].as_array().unwrap())
        .collect()
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kv| kv["key"] == key)
        .map(|kv| &kv["value"])
}

#[test]
fn one_span_per_openrouter_root_with_the_same_ids() {
    for (src, dst) in PAIRS {
        let roots: BTreeSet<(String, String)> = deliveries(src)
            .iter()
            .flat_map(|d| spans(d).into_iter().cloned().collect::<Vec<_>>())
            .filter(|s| s["name"] == "LLM Generation")
            .map(|s| {
                (
                    s["traceId"].as_str().unwrap().into(),
                    s["spanId"].as_str().unwrap().into(),
                )
            })
            .collect();
        let got: BTreeSet<(String, String)> = reencoded(dst)
            .iter()
            .flat_map(|d| spans(d).into_iter().cloned().collect::<Vec<_>>())
            .map(|s| {
                (
                    s["traceId"].as_str().unwrap().into(),
                    s["spanId"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(got, roots, "{dst}");
    }
}

#[test]
fn reencoded_spans_carry_only_semconv_keys() {
    for (_, dst) in PAIRS {
        for d in reencoded(dst) {
            for rs in d["resourceSpans"].as_array().unwrap() {
                assert_eq!(rs["scopeSpans"][0]["scope"]["name"], "toolpath-reencode");
            }
            for s in spans(&d) {
                assert!(s["name"].as_str().unwrap().starts_with("chat "), "{dst}");
                assert_eq!(
                    attr(s, "gen_ai.operation.name").unwrap()["stringValue"],
                    "chat"
                );
                for kv in s["attributes"].as_array().unwrap() {
                    let k = kv["key"].as_str().unwrap();
                    assert!(
                        !k.starts_with("trace.metadata")
                            && !k.starts_with("span.")
                            && k != "gen_ai.prompt"
                            && k != "gen_ai.completion"
                            && k != "session.id"
                            && k != "gen_ai.usage.total_tokens"
                            && !k.contains("cost"),
                        "{dst}: {k}"
                    );
                }
            }
        }
    }
}

#[test]
fn session_ids_move_to_conversation_id_and_messages_are_json_arrays() {
    let d = &reencoded("claude-code.ndjson")[0];
    let s = spans(d)[0];
    assert_eq!(
        attr(s, "gen_ai.conversation.id").unwrap()["stringValue"],
        "177b923f-8cf6-42fc-9f30-9a7b86236265"
    );
    let input: Value = serde_json::from_str(
        attr(s, "gen_ai.input.messages").unwrap()["stringValue"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        input
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["parts"].is_array())
    );
    let output: Value = serde_json::from_str(
        attr(s, "gen_ai.output.messages").unwrap()["stringValue"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output.as_array().unwrap().len(), 1);
    assert_eq!(output[0]["role"], "assistant");
    // pi carries no session.id, so no conversation id either.
    assert!(
        attr(
            spans(&reencoded("pi.ndjson")[0])[0],
            "gen_ai.conversation.id"
        )
        .is_none()
    );
}

#[test]
fn the_error_span_keeps_its_status() {
    let d = &reencoded("codex-error-span.ndjson")[0];
    assert_eq!(spans(d)[0]["status"]["code"], 2);
}
