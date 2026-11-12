//! The accounting contract of agent-coding-session v1.1.0: a session's token
//! usage has three witnesses that must agree, field by field. The
//! `token_usage` of every `conversation.append` step summed; the
//! `total_usage` the adapter's `provider::to_view` reports; and the
//! `total_usage` [`extract_conversation`] rebuilds from the document.
//! Counters add, and breakdowns add class by class (never into the
//! counters). The breakdowns half is the contract c65a6b6 restored: extract
//! rebuilt the total from the four counters alone and lost every
//! per-turn breakdown.

use std::collections::BTreeSet;

use serde_json::Value;
use toolpath::v1::Graph;
use toolpath_convo::{ConversationView, TokenUsage, extract_conversation};

use crate::model::Mismatch;

/// Every field on which the three witnesses disagree; empty when the
/// contract holds. `Err` when the document is not a single-path graph.
pub fn check(document: &Value, ir: &ConversationView) -> Result<Vec<Mismatch>, String> {
    let steps = sum_steps(document);
    let path = Graph::from_json(&document.to_string())
        .map_err(|e| format!("parse document: {e}"))?
        .into_single_path()
        .ok_or("expected a single-path graph")?;
    let extracted = extract_conversation(&path).total_usage;
    Ok(mismatches(&steps, &ir.total_usage, &extracted))
}

/// The `token_usage` of every `conversation.append` step, added up; `None`
/// when no step carries one.
pub fn sum_steps(document: &Value) -> Option<TokenUsage> {
    let mut total: Option<TokenUsage> = None;
    let paths = document["paths"].as_array()?;
    for change in paths
        .iter()
        .flat_map(|p| p["steps"].as_array().into_iter().flatten())
        .flat_map(|s| s["change"].as_object().into_iter().flatten())
        .map(|(_, c)| &c["structural"])
        .filter(|s| s["type"] == "conversation.append")
    {
        let Some(raw) = change.get("token_usage") else {
            continue;
        };
        let Ok(usage) = serde_json::from_value::<TokenUsage>(raw.clone()) else {
            continue;
        };
        let t = total.get_or_insert_with(TokenUsage::default);
        t.input_tokens = add(t.input_tokens, usage.input_tokens);
        t.output_tokens = add(t.output_tokens, usage.output_tokens);
        t.cache_read_tokens = add(t.cache_read_tokens, usage.cache_read_tokens);
        t.cache_write_tokens = add(t.cache_write_tokens, usage.cache_write_tokens);
        for (class, inner) in usage.breakdowns {
            let target = t.breakdowns.entry(class).or_default();
            for (sub, n) in inner {
                *target.entry(sub).or_insert(0) += n;
            }
        }
    }
    total
}

fn add(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// The fields on which `steps`, `ir` and `extracted` are not all equal: the
/// four counters, then every breakdown any of the three carries.
pub fn mismatches(
    steps: &Option<TokenUsage>,
    ir: &Option<TokenUsage>,
    extracted: &Option<TokenUsage>,
) -> Vec<Mismatch> {
    type Counter = fn(&TokenUsage) -> Option<u32>;
    let counters: [(&str, Counter); 4] = [
        ("input_tokens", |u| u.input_tokens),
        ("output_tokens", |u| u.output_tokens),
        ("cache_read_tokens", |u| u.cache_read_tokens),
        ("cache_write_tokens", |u| u.cache_write_tokens),
    ];
    let witnesses = [steps, ir, extracted];
    let mut out = Vec::new();
    for (field, get) in counters {
        let [s, i, e] = witnesses.map(|w| w.as_ref().and_then(get));
        if s != i || i != e {
            out.push(Mismatch {
                field: field.to_owned(),
                steps: s,
                ir: i,
                extracted: e,
            });
        }
    }
    let breakdown_fields: BTreeSet<(String, String)> = witnesses
        .iter()
        .flat_map(|w| w.iter())
        .flat_map(|u| {
            u.breakdowns.iter().flat_map(|(class, inner)| {
                inner.keys().map(move |sub| (class.clone(), sub.clone()))
            })
        })
        .collect();
    for (class, sub) in breakdown_fields {
        let [s, i, e] = witnesses.map(|w| {
            w.as_ref()
                .and_then(|u| u.breakdowns.get(&class)?.get(&sub).copied())
        });
        if s != i || i != e {
            out.push(Mismatch {
                field: format!("breakdowns.{class}.{sub}"),
                steps: s,
                ir: i,
                extracted: e,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn usage(input: u32, output: u32, reasoning: Option<u32>) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            breakdowns: reasoning
                .map(|r| {
                    BTreeMap::from([(
                        "output".to_owned(),
                        BTreeMap::from([("reasoning".to_owned(), r)]),
                    )])
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    fn document(usages: &[Value]) -> Value {
        let steps: Vec<Value> = usages
            .iter()
            .enumerate()
            .map(|(i, u)| {
                json!({
                    "step": {"id": format!("s{i}"), "actor": "agent:x", "timestamp": "2026-01-01T00:00:00Z"},
                    "change": {"agent://x/s": {"structural": {"type": "conversation.append", "role": "assistant", "text": "t", "token_usage": u}}}
                })
            })
            .collect();
        json!({"graph": {"id": "g"}, "paths": [{"path": {"id": "p", "head": "s0"}, "steps": steps}]})
    }

    #[test]
    fn steps_add_counters_and_breakdowns_class_by_class() {
        let doc = document(&[
            json!({"input_tokens": 10, "output_tokens": 5, "breakdowns": {"output": {"reasoning": 2}}}),
            json!({"input_tokens": 1, "output_tokens": 1, "cache_read_tokens": 7, "breakdowns": {"output": {"reasoning": 3}}}),
        ]);
        let total = sum_steps(&doc).expect("two steps carry usage");
        assert_eq!(total.input_tokens, Some(11));
        assert_eq!(total.output_tokens, Some(6));
        assert_eq!(total.cache_read_tokens, Some(7));
        assert_eq!(total.cache_write_tokens, None);
        assert_eq!(total.breakdowns["output"]["reasoning"], 5);
    }

    #[test]
    fn no_step_usage_is_none() {
        let doc = json!({"graph": {"id": "g"}, "paths": [{"path": {"id": "p", "head": "s0"}, "steps": [
            {"step": {"id": "s0", "actor": "human:x", "timestamp": "2026-01-01T00:00:00Z"},
             "change": {"agent://x/s": {"structural": {"type": "conversation.append", "role": "user", "text": "hi"}}}}
        ]}]});
        assert!(sum_steps(&doc).is_none());
    }

    #[test]
    fn agreeing_witnesses_have_no_mismatch() {
        let u = Some(usage(10, 5, Some(2)));
        assert!(mismatches(&u, &u, &u).is_empty());
    }

    #[test]
    fn each_differing_field_is_named_with_the_three_values() {
        let steps = Some(usage(10, 5, Some(2)));
        let ir = Some(usage(12, 5, Some(2)));
        let extracted = Some(usage(10, 5, None));
        let found = mismatches(&steps, &ir, &extracted);
        let fields: Vec<&str> = found.iter().map(|m| m.field.as_str()).collect();
        assert_eq!(fields, ["input_tokens", "breakdowns.output.reasoning"]);
        assert_eq!(
            (found[0].steps, found[0].ir, found[0].extracted),
            (Some(10), Some(12), Some(10))
        );
        assert_eq!(
            (found[1].steps, found[1].ir, found[1].extracted),
            (Some(2), Some(2), None)
        );
    }

    #[test]
    fn a_missing_witness_differs_on_every_field_the_others_carry() {
        let found = mismatches(&Some(usage(1, 1, None)), &None, &Some(usage(1, 1, None)));
        let fields: Vec<&str> = found.iter().map(|m| m.field.as_str()).collect();
        assert_eq!(fields, ["input_tokens", "output_tokens"]);
    }

    #[test]
    fn check_reads_the_three_witnesses_from_a_document_and_its_ir() {
        let doc = document(&[json!({"input_tokens": 10, "output_tokens": 5})]);
        let mut ir = ConversationView {
            total_usage: Some(usage(10, 5, None)),
            ..Default::default()
        };
        assert!(check(&doc, &ir).unwrap().is_empty());
        ir.total_usage = Some(usage(10, 6, None));
        let found = check(&doc, &ir).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].to_string(),
            "total_usage.output_tokens: steps 5, ir 6, extracted 5"
        );
    }
}
