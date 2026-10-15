//! What a round trip lost: a document before and after `project -> read ->
//! derive`, compared as sets of generalised key paths and as counts.

use serde_json::Value;

use crate::keypaths::{counts, key_paths};
use crate::model::RoundTrip;

/// Compare the document a projection was made from with the one its own
/// adapter derives from that projection. Key paths are compared as sets,
/// never index-wise: one defect repeated over every step is one path.
pub fn compare(before: &Value, after: &Value) -> RoundTrip {
    let (k1, k2) = (key_paths(before), key_paths(after));
    RoundTrip::Compared {
        before: counts(before),
        after: counts(after),
        lost: k1.difference(&k2).cloned().collect(),
        gained: k2.difference(&k1).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypaths::KeyPath;
    use serde_json::json;

    fn doc(extra: Value) -> Value {
        json!({ "paths": [{ "steps": [{ "step": {"id": "s"}, "change": { "claude-code://sess-1": { "structural": {
            "type": "conversation.append", "text": "hello", "tool_uses": [{"id": "t"}], "extra": extra } } } }] }] })
    }

    fn paths(v: &[KeyPath]) -> Vec<&str> {
        v.iter().map(KeyPath::as_str).collect()
    }

    #[test]
    fn a_dropped_field_is_one_lost_path_and_the_text_survives() {
        let RoundTrip::Compared {
            before,
            after,
            lost,
            gained,
        } = compare(&doc(json!({"a": 1, "b": 2})), &doc(json!({"a": 1})))
        else {
            unreachable!()
        };
        assert_eq!(
            paths(&lost),
            vec!["paths[].steps[].change.<artifact>.structural.extra.b"]
        );
        assert!(gained.is_empty());
        assert_eq!((before.text_chars, after.text_chars), (5, 5));
        assert_eq!((before.tool_uses, after.tool_uses), (1, 1));
        assert_eq!(before.key_paths, after.key_paths + 1);
    }

    #[test]
    fn an_invented_field_is_one_gained_path() {
        let RoundTrip::Compared { lost, gained, .. } =
            compare(&doc(json!({})), &doc(json!({"vestigial": null})))
        else {
            unreachable!()
        };
        assert!(lost.is_empty());
        assert_eq!(
            paths(&gained),
            vec!["paths[].steps[].change.<artifact>.structural.extra.vestigial"]
        );
    }

    #[test]
    fn identical_documents_lose_and_gain_nothing() {
        let d = doc(json!({"a": [1, 2, 3]}));
        let RoundTrip::Compared {
            before,
            after,
            lost,
            gained,
        } = compare(&d, &d)
        else {
            unreachable!()
        };
        assert_eq!(before, after);
        assert!(lost.is_empty() && gained.is_empty());
    }
}
