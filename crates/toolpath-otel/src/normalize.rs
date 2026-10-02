//! Canonical message form for comparison and ids. What this discards is
//! preserved elsewhere (per-step `dropped` / `echo` extras), not lost.

use crate::generation::{Completion, Message};
use crate::hash::{canonical_json, sha256_hex};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NormMessage {
    pub role: String,
    pub text: String,
    /// Assistant tool calls as `(id, name)`; arguments are never compared.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// Text of a message's content: a string as-is; text parts joined with
/// `\n`; thinking parts skipped; other part types as `[<type>]`.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") | None => p.get("text").and_then(Value::as_str).map(str::to_string),
                Some("thinking" | "redacted_thinking" | "reasoning") => None,
                Some(other) => Some(format!("[{other}]")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `system` or `developer`: the roles harnesses use for instructions
/// rather than conversation. Only the leading one is compared.
pub fn is_system_like(role: &str) -> bool {
    role == "system" || role == "developer"
}

/// The one drop rule: a system-like message anywhere but index 0 is left
/// out of the comparison key (harnesses inject transient system blocks).
pub fn is_dropped(index: usize, role: &str) -> bool {
    index > 0 && is_system_like(role)
}

/// The comparison form of a message; reasoning, `name`, tool arguments and
/// part metadata such as `cache_control` are not part of it.
pub fn normalize(m: &Message) -> NormMessage {
    NormMessage {
        role: m.role.clone(),
        text: content_text(&m.content),
        calls: m
            .tool_calls
            .iter()
            .map(|c| (c.id.clone(), c.function.name.clone()))
            .collect(),
        tool_call_id: m.tool_call_id.clone(),
        is_error: m.is_error.unwrap_or(false),
    }
}

/// The assistant message a generation's completion becomes.
pub fn completion_message(c: &Completion) -> Message {
    Message {
        role: "assistant".to_string(),
        content: Value::String(c.text.clone()),
        tool_calls: c.tool_calls.clone(),
        ..Default::default()
    }
}

/// The id-bearing byte form of a normalized message: its
/// [`canonical_json`] (JCS). Chained turn ids hash these bytes.
pub fn canonical(n: &NormMessage) -> Vec<u8> {
    canonical_json(n).into_bytes()
}

/// Full sha256 (64 lowercase hex) of [`canonical`]: a content id that does
/// not depend on the message's position in the chain.
pub fn content_hash(n: &NormMessage) -> String {
    sha256_hex(&[&canonical(n)])
}

/// The prompt as it enters comparison: every message [`is_dropped`] keeps.
pub fn kept_prompt(messages: &[Message]) -> Vec<NormMessage> {
    messages
        .iter()
        .enumerate()
        .filter(|(i, m)| !is_dropped(*i, &m.role))
        .map(|(_, m)| normalize(m))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{FunctionCall, ToolCall};
    use serde_json::json;

    fn msg(v: serde_json::Value) -> Message {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn string_and_parts_content_normalize_equal() {
        let a = msg(json!({"role": "user", "content": "one\ntwo"}));
        let b = msg(json!({"role": "user", "content": [
            {"type": "text", "text": "one", "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": "two"}
        ]}));
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn thinking_parts_and_reasoning_details_are_excluded() {
        let a = msg(json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "ok"}
        ], "reasoning_details": [{"type": "reasoning.text", "text": "", "signature": "sig"}]}));
        let b = msg(json!({"role": "assistant", "content": "ok"}));
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn assistant_key_ignores_tool_arguments_and_null_content() {
        let echo = msg(json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}
        ]}));
        let completion = Completion {
            text: String::new(),
            reasoning: None,
            reasoning_details: Vec::new(),
            tool_calls: vec![ToolCall {
                id: "t1".into(),
                function: FunctionCall {
                    name: "Bash".into(),
                    arguments: json!("{\"command\": \"cd /work && ls\"}"),
                },
            }],
        };
        assert_eq!(
            normalize(&echo),
            normalize(&completion_message(&completion))
        );
    }

    #[test]
    fn null_content_tool_call_only_assistant_normalizes_to_empty_text() {
        let m = msg(json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
        ]}));
        let n = normalize(&m);
        assert_eq!(n.text, "");
        assert_eq!(n.calls, vec![("t1".to_string(), "Bash".to_string())]);
    }

    #[test]
    fn tool_messages_key_on_call_id_text_and_error() {
        let ok = msg(json!({"role": "tool", "tool_call_id": "t1", "content": "done"}));
        let err =
            msg(json!({"role": "tool", "tool_call_id": "t1", "content": "done", "is_error": true}));
        assert_ne!(normalize(&ok), normalize(&err));
    }

    #[test]
    fn is_dropped_is_non_leading_system_like() {
        assert!(!is_dropped(0, "system"));
        assert!(!is_dropped(0, "developer"));
        assert!(is_dropped(1, "system"));
        assert!(is_dropped(3, "developer"));
        assert!(!is_dropped(1, "user"));
        assert!(!is_dropped(2, "assistant"));
        assert!(!is_dropped(5, "tool"));
    }

    #[test]
    fn kept_prompt_drops_non_leading_system_like_messages() {
        let prompt = vec![
            msg(json!({"role": "developer", "content": "d0"})),
            msg(json!({"role": "developer", "content": "d1"})),
            msg(json!({"role": "user", "content": "u"})),
            msg(json!({"role": "system", "content": "env"})),
        ];
        let kept: Vec<String> = kept_prompt(&prompt).into_iter().map(|n| n.text).collect();
        assert_eq!(kept, vec!["d0", "u"]);
    }

    #[test]
    fn canonical_bytes_are_stable_and_hash_is_64_hex() {
        let n = normalize(&msg(json!({"role": "user", "content": "hi"})));
        assert_eq!(canonical(&n), canonical(&n.clone()));
        assert_eq!(content_hash(&n).len(), 64);
    }

    // Exact JCS bytes and hashes, computed outside the crate
    // (python3 json.dumps(sort_keys=True, separators=(",", ":")) + hashlib).
    #[test]
    fn canonical_assistant_with_text_and_call_exact_bytes() {
        let n = normalize(&msg(json!({"role": "assistant", "content": "Listing.",
            "tool_calls": [{"id": "toolu_01", "type": "function",
                "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}]})));
        assert_eq!(
            String::from_utf8(canonical(&n)).unwrap(),
            r#"{"calls":[["toolu_01","Bash"]],"role":"assistant","text":"Listing."}"#
        );
        assert_eq!(
            content_hash(&n),
            "df77a8585400a52c957e62757d593cbacc2cc841ae33f3df6895649a94c542e2"
        );
    }

    #[test]
    fn canonical_tool_call_only_assistant_keeps_empty_text() {
        let n = normalize(&msg(json!({"role": "assistant", "content": null,
            "tool_calls": [{"id": "toolu_02", "type": "function",
                "function": {"name": "Read", "arguments": "{}"}}]})));
        assert_eq!(
            String::from_utf8(canonical(&n)).unwrap(),
            r#"{"calls":[["toolu_02","Read"]],"role":"assistant","text":""}"#
        );
        assert_eq!(
            content_hash(&n),
            "263eb673030619da01ffa4ee5e49f1041c32c3ebf3632ab708456c332b9128ca"
        );
    }

    #[test]
    fn canonical_tool_error_exact_bytes() {
        let n = normalize(&msg(json!({"role": "tool", "tool_call_id": "toolu_01",
            "content": "boom", "is_error": true})));
        assert_eq!(
            String::from_utf8(canonical(&n)).unwrap(),
            r#"{"is_error":true,"role":"tool","text":"boom","tool_call_id":"toolu_01"}"#
        );
        assert_eq!(
            content_hash(&n),
            "8b454d958033620fecf761060cf19ddafd7150f698ef59c4ae0435d868175b11"
        );
    }

    #[test]
    fn canonical_user_exact_bytes_and_hash() {
        let n = normalize(&msg(json!({"role": "user", "content": "hi"})));
        assert_eq!(
            String::from_utf8(canonical(&n)).unwrap(),
            r#"{"role":"user","text":"hi"}"#
        );
        assert_eq!(
            content_hash(&n),
            "1b0a09edd31484a31e977a2e266c127feba83b2f41061b6686bb61fa78b6edfc"
        );
    }

    #[test]
    fn content_text_edge_shapes() {
        assert_eq!(content_text(&json!([])), "");
        assert_eq!(
            content_text(&json!([{"type": "image_url", "image_url": {"url": "x"}}])),
            "[image_url]"
        );
        assert_eq!(
            content_text(&json!([{"type": "text"}, {"type": "text", "text": "a"}])),
            "a"
        );
        assert_eq!(content_text(&json!({"type": "text", "text": "a"})), "");
        assert_eq!(content_text(&json!(null)), "");
        // A part with no `type` counts as text.
        assert_eq!(
            content_text(&json!([{"text": "untyped"}, {"type": "text", "text": "b"}])),
            "untyped\nb"
        );
        let n = normalize(&msg(json!({"role": "user", "content": [
            {"type": "text", "text": "see"}, {"type": "image", "source": {}}
        ]})));
        assert_eq!(n.text, "see\n[image]");
    }

    #[test]
    fn tool_messages_differing_only_in_call_id_are_unequal() {
        let a = msg(json!({"role": "tool", "tool_call_id": "t1", "content": "done"}));
        let b = msg(json!({"role": "tool", "tool_call_id": "t2", "content": "done"}));
        assert_ne!(normalize(&a), normalize(&b));
        assert_ne!(canonical(&normalize(&a)), canonical(&normalize(&b)));
    }
}
