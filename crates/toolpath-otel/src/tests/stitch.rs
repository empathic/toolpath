//! Stitch regression gate. The literals were computed independently from
//! the spec formulas with python3 hashlib/json (not the crate). A failure
//! means turn ids moved: every imported id would re-key. Never edit a
//! literal to make this pass.

use super::common::*;
use crate::tests::otel::normalize::{content_hash, is_dropped, normalize};
use crate::tests::otel::stitch;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn claude_code_turn_ids_are_pinned() {
    let s = session("claude-code.ndjson");
    assert_eq!(s.key, "177b923f-8cf6-42fc-9f30-9a7b86236265");
    let g = stitch(&s);
    let got: Vec<(&str, &str, Option<usize>)> = g
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.message.role.as_str(), n.producer))
        .collect();
    assert_eq!(
        got,
        vec![
            ("ceb2cfda6f6fa12e", "system", None),
            ("719aa21384e4678a", "user", None),
            ("38fbf5615ad8f1dd", "assistant", Some(0)),
            ("0900aa436b0a088c", "assistant", Some(1)),
            ("4073dbeee8183dd7", "assistant", Some(2)),
            ("62ea3bd0bbd089c3", "assistant", Some(3)),
            ("483278d02765976e", "assistant", Some(4)),
        ]
    );
}

#[test]
fn pi_cluster_key_is_pinned() {
    assert_eq!(session("pi.ndjson").key, "otel-cluster:4c9f0c410e076f96");
}

/// Oracle: expected.json's tool-call ids. Every call before the session's
/// final completion has a result; the final completion's calls have none
/// in the capture.
#[test]
fn every_expected_tool_call_before_the_final_completion_has_a_result() {
    let exp = expected();
    for (file, key) in REAL {
        let g = stitch(&session(file));
        let calls: Vec<String> = g
            .nodes
            .iter()
            .flat_map(|n| n.message.tool_calls.iter().map(|c| c.id.clone()))
            .collect();
        assert_eq!(
            calls,
            strings(&exp["sessions"][key]["tool_call_ids"]),
            "{key}"
        );
        let head = g
            .nodes
            .iter()
            .find(|n| n.id == g.links.last().unwrap().completion)
            .unwrap();
        let open: BTreeSet<&str> = head
            .message
            .tool_calls
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let answered: BTreeSet<&str> = g
            .nodes
            .iter()
            .flat_map(|n| n.results.keys().map(String::as_str))
            .collect();
        for id in &calls {
            assert_eq!(
                answered.contains(id.as_str()),
                !open.contains(id.as_str()),
                "{key}: {id}"
            );
        }
    }
}

#[test]
fn no_m0_call_or_tool_message_has_an_empty_id() {
    for file in CONVERSATIONS {
        for generation in session(file).generations {
            let calls = generation
                .completion
                .tool_calls
                .iter()
                .chain(generation.messages.iter().flat_map(|m| &m.tool_calls));
            for c in calls {
                assert!(!c.id.is_empty(), "{file} {}: id-less call", generation.id);
            }
            for m in generation.messages.iter().filter(|m| m.role == "tool") {
                assert!(
                    m.tool_call_id.as_deref().is_some_and(|id| !id.is_empty()),
                    "{file} {}",
                    generation.id
                );
            }
        }
    }
}

/// Trailing system blocks: `dropped` records each at its prompt index
/// with its role and content hash, and `dropped_content` stores each
/// text once, from the first generation carrying it.
#[test]
fn dropped_system_blocks_record_index_role_and_one_copy() {
    let s = session("claude-code.ndjson");
    let g = stitch(&s);
    let mut first_seen: BTreeMap<String, usize> = BTreeMap::new();
    for (gi, (generation, link)) in s.generations.iter().zip(&g.links).enumerate() {
        let want: Vec<(usize, String, String)> = generation
            .messages
            .iter()
            .enumerate()
            .filter(|(i, m)| is_dropped(*i, &m.role))
            .map(|(i, m)| (i, m.role.clone(), content_hash(&normalize(m))))
            .collect();
        assert!(!want.is_empty(), "generation {gi} drops nothing");
        let got: Vec<(usize, String, String)> = link
            .dropped
            .iter()
            .map(|d| (d.index, d.role.clone(), d.content_hash.clone()))
            .collect();
        assert_eq!(got, want, "generation {gi}");
        for (_, _, h) in want {
            first_seen.entry(h).or_insert(gi);
        }
    }
    assert_eq!(g.dropped_content.len(), first_seen.len());
    for (h, gi) in &first_seen {
        assert_eq!(g.dropped_content[h].0, *gi, "{h}");
    }
}
