//! Span-content captures from the official OTel GenAI instrumentations
//! (scripts/otel-fixtures). Oracles are each capture's expected.json; where
//! the pinned instrumentation diverges from what the mock served, the
//! divergence itself is pinned so a re-pin fails loudly.

use super::common::captures::{
    SEMCONV, continuation_dir, expected_in, for_each_span, manifest_in, semconv_dir, traces_in,
};
use crate::tests::otel::{
    CacheBasis, DeriveConfig, Generation, History, ProfileSelection, Session, derive_path,
    group_sessions, read_deliveries, stitch,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn gens(values: &[Value]) -> Vec<Generation> {
    let out = read_deliveries(values, ProfileSelection::Auto).unwrap();
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    out.generations
}
fn path(s: &Session) -> Value {
    serde_json::to_value(derive_path(s, &DeriveConfig::default())).unwrap()
}
fn conv(p: &Value) -> Vec<Value> {
    p["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| {
            s["change"]
                .as_object()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|c| c["structural"]["type"] == "conversation.append")
        .map(|c| c["structural"].clone())
        .collect()
}

fn attr<'a>(sp: &'a Value, key: &str) -> Option<&'a Value> {
    sp["attributes"]
        .as_array()?
        .iter()
        .find(|kv| kv["key"] == key)
        .map(|kv| &kv["value"])
}
/// Every attribute key any span of the capture carries.
fn attribute_keys(name: &str) -> BTreeSet<String> {
    let mut values = traces_in(&semconv_dir(name));
    let mut keys = BTreeSet::new();
    for_each_span(&mut values, |sp| {
        keys.extend(
            sp["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|kv| kv["key"].as_str().map(str::to_string)),
        );
    });
    keys
}

/// The SYNTHETIC continuation copy of the real openai-responses capture:
/// fixture normalization set the GenAI semconv attribute
/// `gen_ai.request.previous_response.id` on a copy, with the value the
/// client really sent; the real capture is untouched.
fn continuation_traces() -> Vec<Value> {
    let d = continuation_dir();
    assert_eq!(
        manifest_in(&d)["synthetic"]["label"],
        "SYNTHETIC",
        "the continuation copy must be labeled"
    );
    traces_in(&d)
}

fn one_session(values: &[Value]) -> Session {
    let mut s = group_sessions(gens(values));
    assert_eq!(s.len(), 1);
    s.remove(0)
}

#[test]
fn each_capture_imports_as_its_oracle_says() {
    for name in SEMCONV {
        let exp = expected_in(&semconv_dir(name));
        let values = if name == "openai-responses" {
            continuation_traces()
        } else {
            traces_in(&semconv_dir(name))
        };
        let g = gens(&values);
        assert_eq!(g.len(), 3, "{name}");
        assert!(g.iter().all(|g| g.profile == "semconv"), "{name}");
        match exp["generation_ids"].as_array() {
            Some(ids) => assert_eq!(
                json!(g.iter().map(|g| &g.id).collect::<Vec<_>>()),
                json!(ids),
                "{name}"
            ),
            None => assert!(
                g.iter().all(|g| g.id.starts_with("span-")
                    || exp["response_ids"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| r == &json!(g.id))),
                "{name}"
            ),
        }
        let texts: Vec<&str> = g.iter().map(|g| g.completion.text.as_str()).collect();
        assert_eq!(json!(texts), exp["completion_texts"], "{name}");
        let session = one_session(&values);
        let turns = conv(&path(&session));
        let roles: Vec<&Value> = turns.iter().map(|t| &t["role"]).collect();
        assert_eq!(json!(roles), exp["turn_roles"], "{name}");
        assert_eq!(turns[0]["text"], exp["system_text"], "{name}");
        let tools: Vec<Value> = turns
            .iter()
            .filter_map(|t| t["tool_uses"].as_array())
            .flatten()
            .map(|t| {
                json!({"id": t["id"], "name": t["name"], "input": t["input"],
                       "result": t["result"]["content"]})
            })
            .collect();
        assert_eq!(json!(tools), exp["tool_calls"], "{name}");
    }
}

#[test]
fn responses_capture_reflects_the_pinned_instrumentation() {
    let exp = expected_in(&semconv_dir("openai-responses"));
    let mut values = traces_in(&semconv_dir("openai-responses"));
    let (mut continuation, mut reasoning) = (false, false);
    for_each_span(&mut values, |sp| {
        continuation |= attr(sp, "gen_ai.request.previous_response.id").is_some();
        reasoning |= attr(sp, "gen_ai.usage.reasoning.output_tokens").is_some();
    });
    // genai-openai 1.2b0 does not emit gen_ai.request.previous_response.id.
    assert!(
        !continuation,
        "a re-pin now emits the attribute: replace the SYNTHETIC span-continuation copy with the real capture"
    );
    // Nor gen_ai.usage.reasoning.output_tokens, although the mock served
    // a reasoning count. The reasoning-count row is pinned on Gemini instead.
    assert_eq!(
        exp["usage_served"][0]["reasoning"], 6,
        "the mock served a count"
    );
    assert!(
        !reasoning,
        "a re-pin now emits the Responses reasoning count: pin reasoning_tokens and breakdowns.output.reasoning on this capture again"
    );
    let g = gens(&values);
    assert!(
        g.iter()
            .all(|g| g.usage.reasoning_tokens.is_none() && g.completion.reasoning.is_none())
    );
}

#[test]
fn server_side_state_chains_linearly_and_equals_a_full_restatement() {
    let s = one_session(&continuation_traces());
    assert!(
        s.generations[1..]
            .iter()
            .all(|g| g.history == History::Delta && g.continues.is_some())
    );
    let g = stitch(&s);
    assert!(g.missing_continuations.is_empty());
    assert_eq!(g.nodes.len(), 6);
    for w in g.nodes.windows(2) {
        assert_eq!(
            w[1].parent.as_deref(),
            Some(w[0].id.as_str()),
            "linear chain"
        );
    }
    // Restate each Delta as Full: previous history + previous completion + new non-system messages.
    let mut full = Vec::new();
    let mut hist = s.generations[0].messages.clone();
    for (i, g) in s.generations.iter().enumerate() {
        let mut f = g.clone();
        if i > 0 {
            hist.push(crate::tests::otel::normalize::completion_message(
                &s.generations[i - 1].completion,
            ));
            hist.extend(g.messages.iter().filter(|m| m.role != "system").cloned());
        }
        f.messages = hist.clone();
        f.history = History::Full;
        f.continues = None;
        full.push(f);
    }
    let restated = stitch(&Session::new(s.key.clone(), None, full));
    let ids = |t: &crate::tests::otel::TurnGraph| {
        t.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&g), ids(&restated));
}

#[test]
fn gemini_idless_calls_get_positional_ids_paired_by_position() {
    // The pinned instrumentation synthesizes "<name>_<index>"; strip those
    // ids to exercise what an id-less emitter sends.
    let exp = expected_in(&semconv_dir("gemini"));
    let mut values = traces_in(&semconv_dir("gemini"));
    for_each_span(&mut values, |sp| {
        for kv in sp["attributes"].as_array_mut().unwrap() {
            if kv["key"] != "gen_ai.input.messages" && kv["key"] != "gen_ai.output.messages" {
                continue;
            }
            let mut msgs: Value =
                serde_json::from_str(kv["value"]["stringValue"].as_str().unwrap()).unwrap();
            for m in msgs.as_array_mut().unwrap() {
                for p in m["parts"].as_array_mut().unwrap() {
                    if p["type"] == "tool_call" || p["type"] == "tool_call_response" {
                        p.as_object_mut().unwrap().remove("id");
                    }
                }
            }
            kv["value"] = json!({"stringValue": msgs.to_string()});
        }
    });
    let s = one_session(&values);
    let (a, b) = (stitch(&s), stitch(&s));
    let turn = a
        .nodes
        .iter()
        .find(|n| n.message.tool_calls.len() == 2)
        .unwrap();
    let got: Vec<&str> = turn
        .message
        .tool_calls
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(got, [format!("{}:0", turn.id), format!("{}:1", turn.id)]);
    assert_eq!(
        turn.results[got[0]].content,
        exp["tool_calls"][0]["result"].as_str().unwrap()
    );
    assert_eq!(
        turn.results[got[1]].content,
        exp["tool_calls"][1]["result"].as_str().unwrap()
    );
    let again = b.nodes.iter().find(|n| n.id == turn.id).unwrap();
    assert_eq!(
        again.message.tool_calls, turn.message.tool_calls,
        "stable across derives"
    );
}

#[test]
fn cache_tokens_are_additive_whatever_the_emitter_scope() {
    let exp = expected_in(&semconv_dir("anthropic"));
    let g = gens(&traces_in(&semconv_dir("anthropic")));
    for (g, served) in g.iter().zip(exp["usage_served"].as_array().unwrap()) {
        assert_eq!(
            g.usage.input_tokens,
            served["input"].as_u64(),
            "the pinned instrumentation reports inclusive input; cache is subtracted"
        );
        assert_eq!(g.usage.cache_basis, Some(CacheBasis::Inclusive));
        assert_eq!(g.usage.cached_input_tokens, served["cache_read"].as_u64());
    }
    let turns = conv(&path(&one_session(&traces_in(&semconv_dir("anthropic")))));
    let first = turns.iter().find(|t| t["token_usage"].is_object()).unwrap();
    assert_eq!(first["token_usage"]["input_tokens"], 11);
    assert_eq!(first["token_usage"]["cache_read_tokens"], 7);
    assert_eq!(first["token_usage"]["cache_write_tokens"], 5);

    let chat_exp = expected_in(&semconv_dir("openai-chat"));
    let chat = gens(&traces_in(&semconv_dir("openai-chat")));
    assert_eq!(
        chat[0].usage.input_tokens,
        chat_exp["usage_served"][0]["input"].as_u64()
    );
    assert!(
        chat.iter()
            .all(|g| g.usage.cache_basis == Some(CacheBasis::Inclusive))
    );
    // openai-chat is captured through util-genai 1.1b0 (openai-v2 2.4b0's
    // pin), whose span shape differs from the 1.2b0 captures. Pinned so a
    // re-pin fails here and the format note is revisited.
    assert_eq!(
        manifest_in(&semconv_dir("openai-chat"))["packages"]["opentelemetry-util-genai"],
        "1.1b0"
    );
    let scope = json!({"name": "opentelemetry.util.genai.handler", "version": "1.1b0"});
    assert!(chat.iter().all(|g| g.source_meta["scope"] == scope));
    assert_eq!(
        chat_exp["usage_served"][1]["cache_read"], 32,
        "the mock served a cache read"
    );
    assert!(
        chat.iter().all(|g| g.usage.cached_input_tokens.is_none()),
        "a re-pin now reports the served cache read"
    );
    let keys = attribute_keys("openai-chat");
    for k in [
        "gen_ai.usage.cache_read.input_tokens",
        "gen_ai.system_instructions",
        "gen_ai.tool.definitions",
        "server.address",
        "server.port",
    ] {
        assert!(!keys.contains(k), "a re-pin now emits {k}");
    }
    assert!(
        chat.iter()
            .all(|g| g.source_meta.get("tools_digest").is_none())
    );
}

#[test]
fn reasoning_is_retained_and_kept_out_of_the_key() {
    let exp = expected_in(&semconv_dir("anthropic"));
    let s = one_session(&traces_in(&semconv_dir("anthropic")));
    let first = &s.generations[0];
    assert_eq!(
        first.completion.reasoning.as_deref(),
        exp["thinking"][0].as_str()
    );
    assert_eq!(
        first.completion.reasoning_details,
        vec![json!({"type": "reasoning", "content": exp["thinking"][0]})]
    );
    let g = stitch(&s);
    assert_eq!(
        g.nodes.len(),
        6,
        "the reasoning-bearing history echo matched: no fork"
    );
    assert!(
        g.nodes
            .iter()
            .find(|n| n.producer == Some(0))
            .unwrap()
            .echoed
    );
    assert!(exp["signature_sent_in_history"].as_bool().unwrap());
    let text = std::fs::read_to_string(semconv_dir("anthropic").join("traces.json")).unwrap();
    assert!(
        !text.contains("sig-fixture-1"),
        "the pinned instrumentation drops signatures"
    );
    // Hidden reasoning, a count only. The Responses capture carries no
    // count at the pin, so the row runs on Gemini, whose instrumentation
    // reports thoughts and folds them into output.
    let exp = expected_in(&semconv_dir("gemini"));
    let served = &exp["usage_served"][0];
    let thoughts = served["thoughts"].as_u64().unwrap();
    assert_eq!(thoughts, 4);
    let r = gens(&traces_in(&semconv_dir("gemini")));
    assert_eq!(r[0].usage.reasoning_tokens, Some(thoughts));
    assert_eq!(
        r[0].usage.output_tokens,
        Some(served["candidates"].as_u64().unwrap() + thoughts),
        "google-genai 1.2b0 reports output inclusive of thoughts"
    );
    assert_eq!(r[0].completion.reasoning, None);
    let turns = conv(&path(&one_session(&traces_in(&semconv_dir("gemini")))));
    let with: Vec<&Value> = turns
        .iter()
        .filter(|t| t["token_usage"]["breakdowns"].is_object())
        .collect();
    assert_eq!(with.len(), 1, "only the first response served thoughts");
    assert_eq!(
        with[0]["token_usage"]["breakdowns"]["output"]["reasoning"],
        thoughts
    );
    assert!(with[0].get("thinking").is_none());
}

#[test]
fn a_thought_summary_becomes_thinking() {
    // google-genai 1.2b0 emits Gemini thought parts as text; a conforming
    // emitter sends a `reasoning` part, which is what this inline span does.
    let out = json!([{"role": "assistant", "parts": [{"type": "reasoning", "content": "Thought summary."},
                                                     {"type": "text", "content": "Answer."}]}]);
    let d = json!({"resourceSpans": [{"resource": {"attributes": []}, "scopeSpans": [{"scope": {"name": "x"}, "spans": [
        {"traceId": "t", "spanId": "s", "name": "generate_content gemini", "startTimeUnixNano": "1", "endTimeUnixNano": "2",
         "attributes": [
            {"key": "gen_ai.operation.name", "value": {"stringValue": "generate_content"}},
            {"key": "gen_ai.input.messages", "value": {"stringValue": "[{\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"content\":\"q\"}]}]"}},
            {"key": "gen_ai.output.messages", "value": {"stringValue": out.to_string()}}]}]}]}]});
    let turns = conv(&path(&one_session(&[d])));
    let agent = turns.iter().find(|t| t["role"] == "assistant").unwrap();
    assert_eq!(agent["thinking"], "Thought summary.");
    assert_eq!(agent["text"], "Answer.");
}

#[test]
fn system_prompt_placement_is_normalized() {
    for name in ["anthropic", "openai-chat", "gemini"] {
        let turns = conv(&path(&one_session(&traces_in(&semconv_dir(name)))));
        assert_eq!(
            (turns[0]["role"].as_str(), turns[0]["text"].as_str()),
            (
                Some("system"),
                expected_in(&semconv_dir(name))["system_text"].as_str()
            ),
            "{name}"
        );
    }
}

#[test]
fn extra_candidates_are_retained_not_turned_into_turns() {
    let exp = expected_in(&semconv_dir("openai-chat"));
    let g = gens(&traces_in(&semconv_dir("openai-chat")));
    let alt = &g[2].source_meta["choices"][0]["parts"][0]["content"];
    assert_eq!(alt, &exp["extra_choices"][2][0]);
    assert_eq!(
        g[2].completion.text,
        exp["completion_texts"][2].as_str().unwrap()
    );
}

#[test]
fn metadata_only_captures_derive_skeleton_paths() {
    const CONTENT: [&str; 4] = [
        "gen_ai.input.messages",
        "gen_ai.output.messages",
        "gen_ai.system_instructions",
        "gen_ai.tool.definitions",
    ];
    for name in SEMCONV {
        let mut values = traces_in(&semconv_dir(name));
        for_each_span(&mut values, |sp| {
            sp["attributes"]
                .as_array_mut()
                .unwrap()
                .retain(|kv| !CONTENT.contains(&kv["key"].as_str().unwrap_or("")));
        });
        let g = gens(&values);
        assert_eq!(g.len(), 3, "{name}");
        assert!(
            g.iter()
                .all(|g| g.absent.prompt && g.absent.completion && g.response_model.is_some()),
            "{name}"
        );
        let sessions = group_sessions(g);
        assert_eq!(
            sessions.iter().map(|s| s.generations.len()).sum::<usize>(),
            3,
            "{name}: grouping keeps every generation"
        );
        for s in sessions {
            let p = path(&s);
            let turns = conv(&p);
            assert!(!turns.is_empty(), "{name}");
            assert!(
                turns
                    .iter()
                    .all(|t| t["otel"]["absent"] == json!({"prompt": true, "completion": true})),
                "{name}"
            );
            assert!(
                turns
                    .iter()
                    .any(|t| t["token_usage"]["output_tokens"].as_u64().is_some()),
                "{name}"
            );
            assert!(
                p["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|st| st["step"]["actor"].as_str().unwrap().starts_with("agent:")),
                "{name}"
            );
        }
    }
}

#[test]
fn the_shared_readers_name_the_same_capture_directories() {
    assert_eq!(
        continuation_dir(),
        semconv_dir("openai-responses").with_file_name("span-continuation")
    );
    for name in SEMCONV {
        assert!(semconv_dir(name).join("traces.json").is_file(), "{name}");
    }
}
