//! What a round trip lost: a document before and after `project -> read ->
//! derive`, compared as sets of generalised key paths and as counts.

use serde_json::Value;

use crate::keypaths::{counts, key_paths, leaf_values};
use crate::model::{RoundTrip, TwiceAround};

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

/// Pass 2 of a twice-around round trip against pass 1: `first` is the document the target's
/// adapter derives from the projection of the source document, `second` the one it derives from
/// the projection of `first`. A stable conversion changes nothing between them.
///
/// * `lost` / `gained`: key paths present in only one of the two (compared as sets);
/// * `changed`: key paths present in both whose values differ (compared as sorted lists, so a
///   reordering is not a change; ids, timestamps, escaping and path spelling are).
///
/// Only paths are returned, never values: a value that changes on every pass (a minted id)
/// would make the snapshot flap.
pub fn compare_passes(first: &Value, second: &Value) -> TwiceAround {
    let (v1, v2) = (leaf_values(first), leaf_values(second));
    let lost = v1
        .keys()
        .filter(|k| !v2.contains_key(*k))
        .cloned()
        .collect();
    let gained = v2
        .keys()
        .filter(|k| !v1.contains_key(*k))
        .cloned()
        .collect();
    let changed = v1
        .iter()
        .filter(|(k, a)| v2.get(*k).is_some_and(|b| b != *a))
        .map(|(k, _)| k.clone())
        .collect();
    TwiceAround::Compared {
        lost,
        gained,
        changed,
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
