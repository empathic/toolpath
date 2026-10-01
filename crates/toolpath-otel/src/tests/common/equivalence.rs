//! The spec's comparison set, shared by the cross-profile equivalence tests
//! and the event-mode captures.

use serde_json::{Map, Value, json};
use toolpath::v1::Path;
use toolpath::v1::query::dead_ends;

/// A string holding JSON compares as the parsed value (spec: "parsed input").
fn parsed(v: &Value) -> Value {
    v.as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| v.clone())
}

/// One `conversation.append` structural change, reduced to the spec's set.
/// `StructuralChange.extra` is `#[serde(flatten)]`, so `type`, `role`,
/// `text`, `thinking`, `tool_uses`, `token_usage` and `otel` sit side by side.
pub fn conversation(x: &Value) -> Value {
    // Thinking counts only on turns with a producing generation; an empty
    // reasoning string and no reasoning are the same turn.
    let producer = x["otel"].get("generation_id").is_some();
    let thinking = x
        .get("thinking")
        .filter(|t| producer && t.as_str() != Some(""))
        .cloned()
        .unwrap_or(Value::Null);
    let tool_uses: Vec<Value> = x["tool_uses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|u| {
            let result = match &u["result"] {
                Value::Null => Value::Null,
                r => json!({"content": r["content"], "is_error": r["is_error"].as_bool().unwrap_or(false)}),
            };
            json!({"id": u["id"], "name": u["name"], "input": parsed(&u["input"]), "result": result})
        })
        .collect();
    let u = &x["token_usage"];
    json!({
        "role": x["role"],
        "text": x["text"],
        "thinking": thinking,
        "tool_uses": tool_uses,
        "token_usage": {
            "input_tokens": u["input_tokens"],
            "output_tokens": u["output_tokens"],
            "cache_read_tokens": u["cache_read_tokens"],
            "cache_write_tokens": u["cache_write_tokens"],
            "breakdowns": u["breakdowns"],
        },
    })
}

/// The spec's comparison set: head, dead-end set, and per step the id,
/// parents, actor, conversation fields and file changes (artifact key → raw).
pub fn comparison_set(p: &Path) -> Value {
    let mut dead: Vec<&str> = dead_ends(&p.steps, &p.path.head)
        .into_iter()
        .map(|s| s.step.id.as_str())
        .collect();
    dead.sort_unstable();
    let steps: Vec<Value> = p
        .steps
        .iter()
        .map(|st| {
            let change = serde_json::to_value(&st.change).unwrap();
            let mut conv = Value::Null;
            let mut files = Map::new();
            for (key, c) in change.as_object().unwrap() {
                match c["structural"]["type"].as_str() {
                    Some("conversation.append") => conv = conversation(&c["structural"]),
                    Some("file.write") => {
                        files.insert(key.clone(), c["raw"].clone());
                    }
                    _ => {}
                }
            }
            json!({"id": st.step.id, "parents": st.step.parents, "actor": st.step.actor,
                   "conversation": conv, "files": files})
        })
        .collect();
    json!({"head": p.path.head, "dead_ends": dead, "steps": steps})
}
