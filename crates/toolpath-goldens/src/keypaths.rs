//! Key paths: where a value sits in a JSON document, with everything that
//! varies between two derivations of the same session collapsed, so documents
//! compare by structure, not by identity.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use crate::model::Counts;

/// A generalised path into a toolpath document: array indices collapsed to
/// `[]`, artifact-URL keys (whose text embeds a session id) to `<artifact>`,
/// actor-id keys to `<actor>`. `paths[].steps[].change.<artifact>.structural.text`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct KeyPath(String);

impl KeyPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A key that is a file path under a `changes` map (codex keys a patch's files by absolute path,
/// which names the recording machine) is `<path>`.
fn is_file_path_key(prefix: &str, k: &str) -> bool {
    prefix.ends_with("changes") && k.starts_with('/')
}

/// Every leaf's generalised path in a toolpath document, once each.
pub fn key_paths(doc: &Value) -> BTreeSet<KeyPath> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeSet<KeyPath>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let seg = if k.contains("://") {
                        "<artifact>"
                    } else if prefix.ends_with("actors") {
                        "<actor>"
                    } else if is_file_path_key(prefix, k) {
                        "<path>"
                    } else {
                        k.as_str()
                    };
                    walk(&format!("{prefix}.{seg}"), x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(&format!("{prefix}[]"), x, out)),
            _ => {
                out.insert(KeyPath(prefix.trim_start_matches('.').to_string()));
            }
        }
    }
    let mut out = BTreeSet::new();
    walk("", doc, &mut out);
    out
}

/// Every leaf's generalised path with the values found there, sorted, so two documents compare
/// by what each path holds and not by where in an array it sat. Strings are taken as they are,
/// other leaves as their JSON text.
pub fn leaf_values(doc: &Value) -> BTreeMap<KeyPath, Vec<String>> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<KeyPath, Vec<String>>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let seg = if k.contains("://") {
                        "<artifact>"
                    } else if prefix.ends_with("actors") {
                        "<actor>"
                    } else if is_file_path_key(prefix, k) {
                        "<path>"
                    } else {
                        k.as_str()
                    };
                    walk(&format!("{prefix}.{seg}"), x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(&format!("{prefix}[]"), x, out)),
            leaf => {
                let s = match leaf {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.entry(KeyPath(prefix.trim_start_matches('.').to_string()))
                    .or_default()
                    .push(s);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk("", doc, &mut out);
    out.values_mut().for_each(|v| v.sort());
    out
}

/// Leaf paths (arrays collapsed, keys kept) of every JSON value of a
/// transcript, each with the first example value seen. Lines that are not
/// JSON are skipped.
pub fn raw_paths(text: &str) -> BTreeMap<String, String> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
        match v {
            Value::Object(m) => m.iter().for_each(|(k, x)| {
                let seg = if is_file_path_key(prefix, k) {
                    "<path>"
                } else {
                    k.as_str()
                };
                walk(&format!("{prefix}.{seg}"), x, out)
            }),
            Value::Array(a) => a.iter().for_each(|x| walk(&format!("{prefix}[]"), x, out)),
            leaf => {
                let s = match leaf {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.entry(prefix.trim_start_matches('.').to_string())
                    .or_insert(s);
            }
        }
    }
    let mut out = BTreeMap::new();
    for value in json_values(text) {
        walk("", &value, &mut out);
    }
    out
}

/// Every JSON value of `text`: the text itself when it is one JSON document
/// (a Gemini chat file, an opencode or Cursor session as `p export -o`
/// writes it), else every line that parses as JSON (a JSONL transcript).
pub fn json_values(text: &str) -> impl Iterator<Item = Value> + '_ {
    let whole = serde_json::from_str::<Value>(text).ok();
    let by_line = whole.is_none();
    whole.into_iter().chain(
        text.lines()
            .filter(move |_| by_line)
            .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
    )
}

/// `camelCase` -> `camel_case`, so a rename between the two spellings
/// compares equal.
pub fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Steps, conversation text characters, tool uses and distinct key paths of
/// a toolpath document.
pub fn counts(doc: &Value) -> Counts {
    fn walk(v: &Value, c: &mut Counts) {
        match v {
            Value::Object(m) => {
                if m.get("type").and_then(Value::as_str) == Some("conversation.append") {
                    c.text_chars += m
                        .get("text")
                        .and_then(Value::as_str)
                        .map_or(0, |t| t.chars().count());
                    c.tool_uses += m
                        .get("tool_uses")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                }
                if m.contains_key("step") && m.contains_key("change") {
                    c.steps += 1;
                }
                m.values().for_each(|x| walk(x, c));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, c)),
            _ => {}
        }
    }
    let mut c = Counts::default();
    walk(doc, &mut c);
    c.key_paths = key_paths(doc).len();
    c
}

/// Characters in every string value of a JSON value, summed.
pub fn string_chars(v: &Value) -> usize {
    match v {
        Value::String(s) => s.chars().count(),
        Value::Array(a) => a.iter().map(string_chars).sum(),
        Value::Object(m) => m.values().map(string_chars).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(v: &Value) -> Vec<String> {
        key_paths(v).iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn indices_artifact_urls_and_actor_ids_are_collapsed() {
        let doc = json!({
            "paths": [{
                "steps": [
                    { "change": { "claude-code://sess-1": { "structural": { "text": "a" } } } },
                    { "change": { "claude-code://sess-2": { "structural": { "text": "b" } } } }
                ],
                "actors": { "user:alice": { "role": "user" }, "agent:claude": { "role": "agent" } }
            }]
        });
        assert_eq!(
            paths(&doc),
            vec![
                "paths[].actors.<actor>.role",
                "paths[].steps[].change.<artifact>.structural.text",
            ]
        );
    }

    /// A key that is a file path (codex's `changes` map is keyed by the absolute path of each
    /// patched file, which names the recording machine) collapses to `<path>`, in a document and in
    /// a transcript alike, so a re-recording on another machine does not churn the tables.
    #[test]
    fn file_path_keys_under_changes_are_collapsed() {
        let doc = json!({ "paths": [{ "steps": [{ "change": { "claude-code://s": {
            "structural": { "changes": {
                "/var/folders/xx/T/run.A1/codex/a.md": { "type": "add" },
                "/var/folders/xx/T/run.A1/codex/b.sh": { "type": "add" }
            } } } } }] }] });
        assert_eq!(
            paths(&doc),
            vec!["paths[].steps[].change.<artifact>.structural.changes.<path>.type"]
        );
        let raw = raw_paths(
            "{\"payload\":{\"changes\":{\"/var/folders/xx/a.md\":{\"type\":\"add\"}}}}\n",
        );
        assert_eq!(
            raw.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["payload.changes.<path>.type"]
        );
    }

    #[test]
    fn raw_paths_keep_the_first_example_per_path() {
        let text = "{\"a\":{\"b\":[1,2]},\"t\":\"x\"}\n{\"a\":{\"b\":[3]},\"t\":\"y\"}\nnot json\n";
        let got = raw_paths(text);
        assert_eq!(got.get("a.b[]").map(String::as_str), Some("1"));
        assert_eq!(got.get("t").map(String::as_str), Some("x"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn a_whole_document_is_one_value_and_a_stream_is_one_per_line() {
        let document = "{\n  \"a\": 1,\n  \"b\": [2]\n}\n";
        assert_eq!(json_values(document).count(), 1);
        let stream = "{\"a\":1}\n{\"a\":2}\nnot json\n";
        assert_eq!(json_values(stream).count(), 2);
        assert_eq!(json_values("{\"a\":1}\n").count(), 1);
        assert_eq!(json_values("").count(), 0);
    }

    #[test]
    fn counts_see_steps_text_and_tool_uses() {
        let doc = json!({ "paths": [{ "steps": [{
            "step": { "id": "s" },
            "change": { "claude-code://s": { "structural": {
                "type": "conversation.append", "text": "hello", "tool_uses": [{ "id": "t" }]
            } } }
        }] }] });
        let c = counts(&doc);
        assert_eq!((c.steps, c.text_chars, c.tool_uses), (1, 5, 1));
        assert_eq!(c.key_paths, key_paths(&doc).len());
    }

    #[test]
    fn snake_case_and_string_chars() {
        assert_eq!(snake_case("stopReason"), "stop_reason");
        assert_eq!(snake_case("stop_reason"), "stop_reason");
        assert_eq!(string_chars(&json!({"a": ["xy", 1], "b": {"c": "z"}})), 3);
    }
}
