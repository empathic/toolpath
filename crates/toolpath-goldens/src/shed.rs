//! What is shed between a transcript, the toolpath document, and a
//! projection: events and bytes at each stage, and on the same-format round
//! trip the key paths lost, renamed, or dropped (with one example each).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::keypaths::{counts, json_lines, raw_paths, snake_case, string_chars};
use crate::model::{
    Agent, DocumentStats, Dropped, Kept, KeyPathTally, Rename, SameFormat, Shed, TranscriptStats,
};

/// The shed report of one run: the transcript, the document (as the compact
/// JSON `path p derive` prints), the projections, and the transcript
/// projected back to its own format.
pub fn report(
    source: &str,
    document_json: &str,
    document: &Value,
    projections: &BTreeMap<Agent, String>,
    same_format: &str,
) -> Shed {
    let c = counts(document);
    Shed {
        source: transcript_stats(source),
        ir: DocumentStats {
            bytes: document_json.len(),
            string_chars: string_chars(document),
            steps: c.steps,
            text_chars: c.text_chars,
            tool_uses: c.tool_uses,
        },
        projections: projections
            .iter()
            .map(|(agent, text)| (*agent, transcript_stats(text)))
            .collect(),
        same_format: compare_same_format(source, same_format),
    }
}

/// Lines, bytes, string characters and event types of a JSONL transcript.
pub fn transcript_stats(text: &str) -> TranscriptStats {
    TranscriptStats {
        lines: text.lines().count(),
        bytes: text.len(),
        string_chars: json_lines(text).map(|v| string_chars(&v)).sum(),
        by_type: type_counts(text),
    }
}

/// The transcript against its same-format projection, field by field.
pub fn compare_same_format(source: &str, back: &str) -> SameFormat {
    let (before, after) = (raw_paths(source), raw_paths(back));
    let (tb, ta) = (type_counts(source), type_counts(back));
    let event_types_shed = tb
        .iter()
        .filter_map(|(t, n)| {
            let m = ta.get(t).copied().unwrap_or(0);
            (m < *n).then(|| {
                (
                    t.clone(),
                    Kept {
                        source: *n,
                        after: m,
                    },
                )
            })
        })
        .collect();
    let lost: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let gained: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    SameFormat {
        lines: back.lines().count(),
        bytes: back.len(),
        event_types_shed,
        key_paths: KeyPathTally {
            source: before.len(),
            after: after.len(),
            lost: lost.len(),
            gained: gained.len(),
        },
        renamed: renames(&lost, &gained),
        dropped: lost
            .iter()
            .map(|k| Dropped {
                path: (*k).clone(),
                example: example_value(k, &before[*k]),
            })
            .collect(),
    }
}

/// Lost paths that reappear among the gained ones under the other spelling
/// (`stop_reason` / `stopReason`).
pub fn renames(lost: &[&String], gained: &[&String]) -> Vec<Rename> {
    lost.iter()
        .filter_map(|l| {
            gained
                .iter()
                .find(|g| **g != *l && snake_case(g) == snake_case(l))
                .map(|g| Rename {
                    from: (*l).clone(),
                    to: (*g).clone(),
                })
        })
        .collect()
}

/// `type`, or `type/payload.type`, the way a transcript line names its event.
pub fn event_type(v: &Value) -> String {
    let t = v["type"].as_str().unwrap_or("?");
    match v["payload"]["type"].as_str() {
        Some(p) => format!("{t}/{p}"),
        None => t.to_owned(),
    }
}

/// How many lines of each event type a transcript has.
pub fn type_counts(text: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for l in json_lines(text) {
        *m.entry(event_type(&l)).or_default() += 1;
    }
    m
}

/// An example value for a dropped field, clipped to 40 characters; withheld
/// when the key names a credential or an identifier, or the value is one
/// long token.
pub fn example_value(path: &str, value: &str) -> String {
    let last = path.rsplit(['.', ']']).next().unwrap_or("").to_lowercase();
    let sensitive = [
        "token",
        "secret",
        "password",
        "signature",
        "apikey",
        "auth",
        "cookie",
    ]
    .iter()
    .any(|s| last.contains(s))
        || is_identifier_key(&last)
        || last.ends_with("id")
        || last.ends_with("uuid")
        || (value.len() > 40 && !value.contains(' '));
    if sensitive {
        return "<withheld>".into();
    }
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() > 40 {
        format!("{}…", flat.chars().take(40).collect::<String>())
    } else {
        flat
    }
}

/// A key that holds an account or user identifier, in any spelling.
fn is_identifier_key(key: &str) -> bool {
    let k: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_lowercase();
    matches!(
        k.as_str(),
        "accountid"
            | "userid"
            | "creatoraccountid"
            | "creatoruserid"
            | "organizationid"
            | "orgid"
            | "email"
            | "useremail"
            | "accountemail"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_field_a_rename_and_a_shed_event_type_are_each_reported() {
        let source = "{\"type\":\"user\",\"message\":{\"stop_reason\":\"end\",\"gone\":\"bye\"}}\n{\"type\":\"attachment\",\"x\":1}\n";
        let back = "{\"type\":\"user\",\"message\":{\"stopReason\":\"end\"}}\n";
        let s = compare_same_format(source, back);
        assert_eq!(
            s.renamed,
            vec![Rename {
                from: "message.stop_reason".into(),
                to: "message.stopReason".into()
            }]
        );
        let dropped: Vec<&str> = s.dropped.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(dropped, vec!["message.gone", "message.stop_reason", "x"]);
        assert_eq!(
            s.event_types_shed.get("attachment"),
            Some(&Kept {
                source: 1,
                after: 0
            })
        );
        assert_eq!(s.key_paths.lost, 3);
        assert_eq!(s.key_paths.gained, 1);
    }

    #[test]
    fn event_types_fold_the_payload_type_in() {
        let text = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\"}}\n{\"type\":\"user\"}\n{\"type\":\"user\"}\n";
        let c = type_counts(text);
        assert_eq!(c.get("event_msg/agent_message"), Some(&1));
        assert_eq!(c.get("user"), Some(&2));
    }

    #[test]
    fn examples_withhold_identifiers_and_secrets_and_clip_long_text() {
        assert_eq!(example_value("message.token", "abc"), "<withheld>");
        assert_eq!(example_value("sessionId", "abc"), "<withheld>");
        assert_eq!(example_value("user_email", "a@b.c"), "<withheld>");
        assert_eq!(
            example_value("x", &"y".repeat(50)),
            "<withheld>",
            "one long token is withheld"
        );
        let prose = "the quick brown fox jumps over the lazy dog again and again";
        assert_eq!(example_value("x", prose), format!("{}…", &prose[..40]));
        assert_eq!(example_value("x", "line\nbreak"), "line break");
    }
}
