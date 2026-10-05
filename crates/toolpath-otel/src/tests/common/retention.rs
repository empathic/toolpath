//! Retention round-trip oracle (see "Retention" in `docs/agents/formats/otel.md`).

use crate::tests::otel::derive::conversation_key;
use crate::tests::otel::hash::{chain_id, root_id};
use crate::tests::otel::normalize::{canonical, is_dropped, normalize};
use crate::tests::otel::{
    DeriveConfig, Message, ProfileSelection, Session, derive_path, group_sessions,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use toolpath::v1::{Path, Step};

pub fn meta(p: &Path) -> &Value {
    &p.meta.as_ref().unwrap().extra["otel"]
}

pub fn conv_extra(p: &Path, s: &Step) -> Option<HashMap<String, Value>> {
    let key = conversation_key(meta(p)["derived_session_id"].as_str().unwrap());
    Some(s.change.get(&key)?.structural.as_ref()?.extra.clone())
}

pub fn otel_extra(p: &Path, s: &Step) -> Option<Value> {
    conv_extra(p, s)?.get("otel").cloned()
}

/// The semantic form a message is compared in: everything retention
/// keeps, nothing it deliberately drops (content encoding, `name`,
/// argument formatting).
pub fn semantic(m: &Value) -> Value {
    let calls: Vec<Value> = m["tool_calls"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    let args = &c["function"]["arguments"];
                    let parsed = args
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(args.clone());
                    json!([c["id"], c["function"]["name"], parsed])
                })
                .collect()
        })
        .unwrap_or_default();
    let text = match &m["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("text") | None => p["text"].as_str().map(str::to_string),
                Some("thinking" | "redacted_thinking" | "reasoning") => None,
                Some(other) => Some(format!("[{other}]")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    json!({
        "role": m["role"], "text": text, "calls": calls,
        "tool_call_id": m["tool_call_id"],
        "is_error": m["is_error"].as_bool().unwrap_or(false),
        "reasoning_details": m.get("reasoning_details").cloned().unwrap_or(json!([])),
    })
}

/// One generation rebuilt from the Path alone.
pub struct Rebuilt {
    pub prompt: Vec<Value>,
    pub completion: Value,
    /// The continuation target's completion step id when the prompt chain
    /// starts there (`Delta` with its target in the session); `None` = root.
    pub base_tip: Option<String>,
}

/// The step holding `gid`'s completion and its producer record: the placed
/// producer step, else the unplaced step's record and the step it names.
fn completion_of<'p>(p: &'p Path, gid: &str) -> Option<(Value, &'p Step)> {
    let s = p
        .steps
        .iter()
        .find(|s| otel_extra(p, s).is_some_and(|x| x["generation_id"] == gid))?;
    let x = otel_extra(p, s).unwrap();
    match x["completion"].as_str() {
        None => Some((x, s)),
        Some(id) => {
            let completion = p.steps.iter().find(|s| s.step.id == id)?;
            Some((x, completion))
        }
    }
}

/// Append one chain step to a rebuilt prompt: its message (when
/// `with_message`), then its tool results.
fn push_turn(p: &Path, s: &Step, with_message: bool, prompt: &mut Vec<Value>) {
    let x = otel_extra(p, s).unwrap();
    let c = conv_extra(p, s).unwrap();
    let uses = c
        .get("tool_uses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if with_message {
        let echo_args = x["echo"]["arguments"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let calls: Vec<Value> = uses
            .iter()
            .map(|u| {
                let args = echo_args
                    .get(u["id"].as_str().unwrap())
                    .cloned()
                    .unwrap_or(u["input"].clone());
                json!({"id": u["id"], "function": {"name": u["name"], "arguments": args}})
            })
            .collect();
        let mut msg = json!({"role": x["message_role"], "content": c["text"], "tool_calls": calls});
        if let Some(rd) = x["echo"].get("reasoning_details") {
            msg["reasoning_details"] = rd.clone();
        }
        prompt.push(msg);
    }
    for u in &uses {
        if let Some(r) = u.get("result").filter(|r| !r.is_null()) {
            prompt.push(json!({"role": "tool", "tool_call_id": u["id"], "content": r["content"], "is_error": r["is_error"]}));
        }
    }
}

/// `(prompt, completion)` of one generation, rebuilt from the Path alone.
/// See "Retention" in `docs/agents/formats/otel.md` for the algorithm.
/// Dropped messages go back in at their raw `Dropped.index` (index into
/// this generation's own `messages`, which the rebuilt prompt lines up with).
pub fn rebuild(p: &Path, gid: &str) -> Rebuilt {
    let by_id: HashMap<&str, &Step> = p.steps.iter().map(|s| (s.step.id.as_str(), s)).collect();
    let (record, completion_step) = completion_of(p, gid).expect("placed or unplaced");
    assert!(
        record.get("absent").is_none(),
        "{gid}: skeleton generations are outside the round-trip"
    );
    let mut dropped_text: HashMap<String, String> = HashMap::new();
    for x in p.steps.iter().filter_map(|s| otel_extra(p, s)) {
        for (h, t) in x["dropped_content"].as_object().into_iter().flatten() {
            dropped_text.insert(h.clone(), t.as_str().unwrap().to_string());
        }
    }
    // Delta: stop at the continuation target's completion step.
    let target = record["continues"]
        .as_str()
        .and_then(|c| completion_of(p, c))
        .map(|(_, s)| s.step.id.clone());
    let mut chain: Vec<&Step> = Vec::new();
    let mut base: Option<&Step> = None;
    let mut cur = completion_step.step.parents.first().cloned();
    while let Some(id) = cur {
        let s = by_id[id.as_str()];
        if target.as_deref() == Some(id.as_str()) {
            base = Some(s);
            break;
        }
        chain.push(s);
        cur = s.step.parents.first().cloned();
    }
    chain.reverse();
    let mut prompt = Vec::new();
    if let Some(t) = base {
        push_turn(p, t, false, &mut prompt);
    }
    for s in chain {
        push_turn(p, s, true, &mut prompt);
    }
    for d in record["dropped"].as_array().unwrap() {
        let i = d["index"].as_u64().unwrap() as usize;
        let text = &dropped_text[d["content_hash"].as_str().unwrap()];
        prompt.insert(i, json!({"role": d["role"], "content": text}));
    }
    let pc = conv_extra(p, completion_step).unwrap();
    let calls: Vec<Value> = pc
        .get("tool_uses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|u| json!({"id": u["id"], "function": {"name": u["name"], "arguments": u["input"]}}))
        .collect();
    let completion = json!({
        "role": "assistant", "content": pc["text"], "tool_calls": calls,
        "reasoning": pc.get("thinking").cloned().unwrap_or(Value::Null),
    });
    Rebuilt {
        prompt,
        completion,
        base_tip: base.map(|s| s.step.id.clone()),
    }
}

/// What `assert_retains` checked: every generation, how many of them
/// rebuilt from a continuation target instead of the root, and how many were
/// skeletons (an `absent` side: nothing to rebuild, skipped and counted).
#[derive(Debug, Default, PartialEq)]
pub struct Retained {
    pub generations: usize,
    pub continued: usize,
    pub skeletons: usize,
}

/// Retention round-trip over any input: read `values` under `sel`, group,
/// derive, and [`assert_session_retains`] every session. Totals over all
/// sessions.
pub fn assert_retains(values: &[Value], sel: ProfileSelection) -> Retained {
    let out = crate::tests::otel::read_deliveries(values, sel).unwrap();
    let sessions = group_sessions(out.generations);
    assert!(!sessions.is_empty(), "no sessions to round-trip");
    let mut total = Retained::default();
    for s in &sessions {
        let r = assert_session_retains(s);
        total.generations += r.generations;
        total.continued += r.continued;
        total.skeletons += r.skeletons;
    }
    total
}

/// Rebuild every generation of one session from its derived Path alone.
/// Equal after the four drops; the rebuilt prompt re-chains to the recorded
/// `prompt_tip`. A skeleton generation (its record carries `absent`) is
/// skipped and counted in `skeletons`; the generations around it are still
/// checked, including a `Delta` that continues from it.
pub fn assert_session_retains(s: &Session) -> Retained {
    let p = derive_path(s, &DeriveConfig::default());
    let mut checked = Retained::default();
    for g in &s.generations {
        let (record, _) = completion_of(&p, &g.id).expect("placed or unplaced");
        if record.get("absent").is_some() {
            checked.skeletons += 1;
            continue;
        }
        // Positional ids never equal the source's "": this oracle compares
        // id-bearing inputs only.
        let raw = json!([
            serde_json::to_value(&g.messages).unwrap(),
            serde_json::to_value(&g.completion.tool_calls).unwrap()
        ]);
        assert!(
            !raw.to_string().contains(r#""id":"""#)
                && !raw.to_string().contains(r#""tool_call_id":"""#),
            "{} {}: id-less calls are outside the round-trip",
            s.key,
            g.id
        );
        let Rebuilt {
            prompt: rebuilt,
            completion,
            base_tip,
        } = rebuild(&p, &g.id);
        let want: Vec<Value> = g
            .messages
            .iter()
            .map(|m| semantic(&serde_json::to_value(m).unwrap()))
            .collect();
        let got: Vec<Value> = rebuilt.iter().map(semantic).collect();
        assert_eq!(got, want, "{} {}", s.key, g.id);
        let want_c = semantic(
            &json!({"role": "assistant", "content": g.completion.text, "tool_calls": serde_json::to_value(&g.completion.tool_calls).unwrap()}),
        );
        let mut got_c = semantic(&completion);
        got_c["reasoning_details"] = json!([]);
        assert_eq!(got_c, want_c, "{} {} completion", s.key, g.id);
        let want_reasoning = g.completion.reasoning.clone().filter(|r| !r.is_empty());
        assert_eq!(
            completion["reasoning"].as_str().map(str::to_string),
            want_reasoning,
            "{} {}",
            s.key,
            g.id
        );
        // A Delta with its target chains from the target's completion
        // node, effective index = 1 + message index.
        let (mut tip, offset) = match &base_tip {
            Some(t) => {
                checked.continued += 1;
                (t.clone(), 1)
            }
            None => (root_id(&s.key), 0),
        };
        for (i, m) in rebuilt.iter().enumerate() {
            let msg: Message = serde_json::from_value(m.clone()).unwrap();
            if !is_dropped(offset + i, &msg.role) {
                tip = chain_id(&tip, &canonical(&normalize(&msg)));
            }
        }
        // The producer record: the placed or unplaced step's extras (both
        // carry `prompt_tip`).
        assert_eq!(json!(tip), record["prompt_tip"], "{} {}", s.key, g.id);
        checked.generations += 1;
    }
    checked
}
