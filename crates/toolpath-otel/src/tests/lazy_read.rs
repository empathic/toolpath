//! The memoized prompt read against a whole-prompt parse: same messages
//! and same failures, for fresh and memo-hit messages alike.

use crate::generation::Message;
use crate::profile::ProfileSelection;
use crate::profile::openrouter::{completion_members, completion_whole, prompt_messages};
use crate::tests::otlp_oracle::fixture_values;
use crate::walk::{ReadCx, read_deliveries};
use serde::Deserialize;
use serde_json::{Value, json};

/// What reading the prompt did before the memo: one parse of the whole text.
fn whole(raw: &str) -> Option<Vec<Message>> {
    #[derive(Deserialize)]
    struct Prompt {
        messages: Vec<Message>,
    }
    serde_json::from_str::<Prompt>(raw).ok().map(|p| p.messages)
}

fn check<'a>(raw: &'a str, cx: &mut ReadCx<'a>) {
    assert_eq!(prompt_messages(raw, cx), whole(raw), "{raw:.300}");
}

fn nested(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

/// Prompts that stress parsing: escapes, unicode, number spellings, the
/// recursion limit, and malformed text in and outside `messages`.
fn edge_prompts() -> Vec<String> {
    let mut out = vec![
        r#"{"messages":[{"role":"user","content":"caf\u00e9 \ud83d\ude00 \"q\" \\ \/ \n\t"}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"café 😀"}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"\ud800"}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"ok"}],"other":"\ud800"}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"\x"}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":[1,1.0,-0.0,0.0,1e2,1E2,-1,18446744073709551615,18446744073709551616,1.7976931348623157e308,5e-324]}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":1e400}]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"x"}],"n":1e400}"#.to_string(),
        r#"{"messages":[{"role":"user","role":"system","content":"dup"}]}"#.to_string(),
        r#"{"messages":[{"role":5,"content":"bad role"}]}"#.to_string(),
        r#"{"messages":[{"role":"assistant","content":null,"tool_calls":null,"reasoning_details":null}]}"#.to_string(),
        r#"{"messages":[{"role":"assistant","content":"","tool_calls":[{"id":"t","function":{"name":"Bash","arguments":"{\"a\":1}"}}]},{"role":"tool","tool_call_id":"t","content":"r","is_error":true}]}"#.to_string(),
        r#"{"messages":null}"#.to_string(),
        r#"{"messages":[]}"#.to_string(),
        r#"{}"#.to_string(),
        r#"{"messages":[],"messages":[]}"#.to_string(),
        r#"[[{"role":"user","content":"positional"}]]"#.to_string(),
        r#"{"messages":[{"role":"user","content":"x"}]} trailing"#.to_string(),
        r#"{"messages":[{"role":"user","content":"x"},]}"#.to_string(),
        r#"{"messages":[{"role":"user","content":"cut"#.to_string(),
        r#"  {"messages" : [ {"role" : "user" , "content" : "spaced" } ] }  "#.to_string(),
        r#"{"messages":["not an object"]}"#.to_string(),
    ];
    for depth in 120..=130 {
        out.push(format!(
            r#"{{"messages":[{{"role":"user","content":{}}}]}}"#,
            nested(depth)
        ));
    }
    out
}

/// Every `gen_ai.prompt` string in the fixtures.
fn fixture_prompts() -> Vec<String> {
    fixture_attrs("gen_ai.prompt")
}

fn fixture_attrs(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for v in fixture_values() {
        let d = crate::otlp::Delivery::read(&v);
        for rs in &d.resource_spans {
            for ss in &rs.scope_spans {
                for s in &ss.spans {
                    if let Some(p) = crate::otlp::Attrs(&s.attributes).str(key) {
                        out.push(p.to_string());
                    }
                }
            }
        }
    }
    out
}

#[test]
fn memoized_prompts_parse_as_the_whole_prompt_does() {
    let prompts: Vec<String> = fixture_prompts()
        .into_iter()
        .chain(edge_prompts())
        .collect();
    assert!(prompts.len() > 40, "{}", prompts.len());
    // Fresh per prompt, then one memo across all of them twice over, so
    // every message is also read as a memo hit.
    for p in &prompts {
        check(p, &mut ReadCx::default());
    }
    let mut cx = ReadCx::default();
    for _ in 0..2 {
        for p in &prompts {
            check(p, &mut cx);
        }
    }
}

#[test]
fn spellings_of_one_message_share_a_memo_entry_only_when_they_parse_alike() {
    let mut cx = ReadCx::default();
    let read = |raw: &'static str, cx: &mut ReadCx<'static>| prompt_messages(raw, cx).unwrap();
    let a = read(r#"{"messages":[{"role":"user","content":"A"}]}"#, &mut cx);
    let b = read(r#"{"messages":[{"content":"A","role":"user"}]}"#, &mut cx);
    assert_eq!(a, b);
    let one = read(r#"{"messages":[{"role":"user","content":[1]}]}"#, &mut cx);
    let float = read(r#"{"messages":[{"role":"user","content":[1.0]}]}"#, &mut cx);
    assert_ne!(one, float);
}

#[test]
fn a_history_repeated_across_deliveries_reads_as_fresh_copies() {
    let history = [
        json!({"role": "system", "content": [{"type": "text", "text": "sys \u{e9}", "cache_control": {"type": "ephemeral"}}]}),
        json!({"role": "user", "content": "do it"}),
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}]}),
        json!({"role": "tool", "tool_call_id": "t1", "content": "a\nb"}),
    ];
    let delivery = |i: usize| {
        let prompt = json!({"messages": history[..i].to_vec()}).to_string();
        json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
            "traceId": format!("t{i}"), "name": "LLM Generation",
            "attributes": [
                {"key": "gen_ai.response.id", "value": {"stringValue": format!("g{i}")}},
                {"key": "session.id", "value": {"stringValue": "s"}},
                {"key": "gen_ai.prompt", "value": {"stringValue": prompt}},
                {"key": "gen_ai.completion", "value": {"stringValue": "{\"completion\":\"ok\"}"}}
            ]
        }]}]}]})
    };
    let values: Vec<Value> = (1..=history.len()).map(delivery).collect();
    let together = read_deliveries(&values, ProfileSelection::Auto).unwrap();
    for (i, v) in values.iter().enumerate() {
        let alone = read_deliveries(std::iter::once(v), ProfileSelection::Auto).unwrap();
        assert_eq!(alone.generations[0], together.generations[i]);
    }
}

/// A conversation growing one message per prompt, OpenRouter-spelled, with
/// variants that share a prefix and then differ, so the prefix reuse meets
/// every kind of tail.
fn growing_prompts() -> Vec<String> {
    let msgs: Vec<String> = (0..8)
        .map(|i| {
            json!({"role": if i % 2 == 0 { "user" } else { "assistant" },
                   "content": format!("turn {i} \u{e9}\n\"q\" {}", "x".repeat(i * 40))})
            .to_string()
        })
        .collect();
    let head = r#"{"messages":["#;
    let mut out: Vec<String> = (1..=msgs.len())
        .map(|n| format!("{head}{}]}}", msgs[..n].join(",")))
        .collect();
    let three = msgs[..3].join(",");
    for tail in [
        format!("{three}]}}"),
        format!("{three} , {} ]}} ", msgs[3]),
        format!("{three},{}]}}", r#"{"role":"user","content":"\ud800"}"#),
        format!("{three},{}]}}", r#"{"role":"user","content":1e400}"#),
        format!("{three},]}}"),
        format!("{three}]}} x"),
        format!("{three}],\"messages\":[]}}"),
        format!("{three},{}", msgs[3]),
        format!("{three}{}]}}", msgs[3]),
        format!("{three},{}]}}", &msgs[3][..msgs[3].len() - 1]),
        format!("{},{}]}}", &three[..three.len() - 2], msgs[3]),
        format!("{three},\"s\"]}}"),
        format!("{three},{}]}}", nested(127)),
        format!(
            "{three},{}]}}",
            json!({"role": "user", "content": nested(126)})
        ),
        format!(
            "{three},{}]}}",
            json!({"role": "user", "content": nested(127)})
        ),
        format!("{three}]}}"),
    ] {
        out.push(format!("{head}{tail}"));
    }
    out.push(format!("{head}]}}"));
    out.push(format!("{head} ]}}"));
    out
}

#[test]
fn prefix_reuse_parses_as_the_whole_prompt_does() {
    let prompts = growing_prompts();
    for p in &prompts {
        check(p, &mut ReadCx::default());
    }
    // In order, reversed, and repeated through one memo: every tail is read
    // against every earlier prompt it shares a prefix with.
    let mut cx = ReadCx::default();
    for p in prompts.iter().chain(prompts.iter().rev()).chain(&prompts) {
        check(p, &mut cx);
    }
    // The shortcut, not the fallback, read the well-formed ones.
    let mut cx = ReadCx::default();
    for p in &prompts[..8] {
        assert!(cx.prompt(p).is_some(), "{p:.200}");
    }
    for p in fixture_prompts()
        .iter()
        .filter(|p| p.starts_with(r#"{"messages":["#))
    {
        assert!(ReadCx::default().prompt(p).is_some(), "{p:.200}");
    }
}

fn edge_completions() -> Vec<String> {
    let tools = r#"[{"type":"function","function":{"name":"Bash","parameters":{"type":"object","properties":{"n":{"type":"number","default":1.0}}}}}]"#;
    let mut out = vec![
        format!(r#"{{"completion":"ok","toolCalls":[],"tools":{tools}}}"#),
        format!(
            r#"{{"completion":"ok","tools":null,"rawRequest":{{"tools":{tools},"model":"m","session_id":"s"}}}}"#
        ),
        format!(
            r#"{{"rawRequest":{{"messages":[{{"role":"user","content":"x"}}],"tools":{tools},"input":"i","stream":true,"session_id":7}}}}"#
        ),
        format!(
            r#"{{"tools":{tools},"tools":null,"rawRequest":{{"tools":{tools},"tools":null}}}}"#
        ),
        format!(r#"{{"tools":null,"tools":{tools}}}"#),
        r#"{"rawRequest":null}"#.to_string(),
        r#"{"rawRequest":[1,2]}"#.to_string(),
        r#"{"rawRequest":"s"}"#.to_string(),
        r#"{"rawRequest":{}}"#.to_string(),
        r#"{"rawRequest":{"a":1},"rawRequest":{"b":2}}"#.to_string(),
        r#"{"rawRequest":{"messages":"\ud800"}}"#.to_string(),
        r#"{"rawRequest":{"input":1e400}}"#.to_string(),
        r#"{"tools":[1e400]}"#.to_string(),
        r#"{"tools":"\x"}"#.to_string(),
        r#"{"completion":"a","completion":"b","reasoning":"r","toolCalls":null}"#.to_string(),
        r#"{"toolCalls":[{"id":"t","function":{"name":"f","arguments":"{}"}}]}"#.to_string(),
        r#"{"compl\u0065tion":"escaped key"}"#.to_string(),
        r#" { "completion" : "spaced" , "tools" : [ ] } "#.to_string(),
        r#"{"completion":"x",}"#.to_string(),
        r#"{"completion":"x"} y"#.to_string(),
        r#"["completion"]"#.to_string(),
        r#""just a string""#.to_string(),
        r#"null"#.to_string(),
        r#"{}"#.to_string(),
        format!(r#"{{"tools":{}}}"#, nested(127)),
        format!(r#"{{"tools":{}}}"#, nested(128)),
        format!(r#"{{"rawRequest":{{"tools":{}}}}}"#, nested(126)),
        format!(r#"{{"rawRequest":{{"tools":{}}}}}"#, nested(127)),
        format!(r#"{{"rawRequest":{{"temperature":{}}}}}"#, nested(127)),
    ];
    // Large enough to be remembered: repeated, altered, and moved between
    // the completion and `rawRequest`.
    let big = format!(
        r#"[{{"name":"Bash","description":"{}"}}]"#,
        "d".repeat(2000)
    );
    let altered = big.replacen("Bash", "Bish", 1);
    for t in [&big, &big, &altered, &big] {
        out.push(format!(
            r#"{{"completion":"x","tools":{t},"rawRequest":{{"tools":{t},"n":1}}}}"#
        ));
        out.push(format!(r#"{{"tools":{t} , "completion":"y"}}"#));
        out.push(format!(r#"{{"rawRequest":{{"tools":{t}}},"tools":null}}"#));
    }
    out.push(format!(r#"{{"tools":{big}x}}"#));
    out.push(format!(r#"{{"tools":{big}]}}"#));
    out.extend(fixture_attrs("gen_ai.completion"));
    out
}

#[test]
fn member_by_member_completions_read_as_the_whole_text_does() {
    let completions = edge_completions();
    assert!(completions.len() > 40, "{}", completions.len());
    let mut shared = ReadCx::default();
    for c in completions.iter().chain(&completions) {
        let whole = completion_whole(c);
        for got in [
            completion_members(c, &mut ReadCx::default()),
            completion_members(c, &mut shared),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(Some(got), whole, "{c:.300}");
        }
        // What the profile does: members, else the whole text.
        let read = completion_members(c, &mut shared).or_else(|| completion_whole(c));
        assert_eq!(read, whole, "{c:.300}");
    }
    for c in fixture_attrs("gen_ai.completion") {
        assert!(
            completion_members(&c, &mut ReadCx::default()).is_some(),
            "{c:.200}"
        );
    }
}
