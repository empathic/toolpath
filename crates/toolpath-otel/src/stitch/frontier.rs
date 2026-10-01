//! The settled frontier: which turns an incremental send may emit.

use super::{Node, TurnGraph, stitch};
use crate::ToolClassifier;
use crate::branch::{Branches, classify};
use crate::derive::categories;
use crate::harness::{SourceHarness, infer_harness, signals};
use crate::session::Session;
use std::borrow::Cow;
use std::collections::HashSet;

/// The session's harness, decided when its first turn settles: inferred
/// from the shortest prefix of `session.generations` (feed order) whose
/// stitch emits a turn, the whole session counting as final when
/// `final_`. Later generations never change it, and a caller holding only
/// the feed order gets the same answer. Which turns emit depends on the
/// tool categories `classifier` names.
pub fn settled_harness(
    session: &Session,
    final_: bool,
    classifier: Option<&ToolClassifier>,
) -> SourceHarness {
    first_settle(session, final_, classifier).1
}

/// The harness of the first `held` generations, as a send that held only
/// them decided it: `first` (from [`first_settle`] of the whole feed) when
/// it settled within them.
pub fn held_harness(
    session: &Session,
    held: usize,
    first: (usize, SourceHarness),
    classifier: Option<&ToolClassifier>,
) -> SourceHarness {
    if first.0 <= held {
        first.1
    } else {
        settled_harness(&prefix(session, held), true, classifier)
    }
}

/// How many generations settle the first turn, and the harness they show.
pub fn first_settle(
    session: &Session,
    final_: bool,
    classifier: Option<&ToolClassifier>,
) -> (usize, SourceHarness) {
    let n = session.generations.len();
    for k in 1..=n {
        let feed = prefix(session, k);
        let harness = infer_harness(&signals(&feed));
        let graph = stitch(&feed);
        let branches = classify(&graph, &categories(classifier, harness));
        if !emitted_turns(&graph, &branches, final_ && k == n).is_empty() {
            return (k, harness);
        }
    }
    (n, infer_harness(&signals(session)))
}

fn prefix(session: &Session, k: usize) -> Cow<'_, Session> {
    if k == session.generations.len() {
        return Cow::Borrowed(session);
    }
    Cow::Owned(Session {
        key: session.key.clone(),
        session_id: session.session_id.clone(),
        generations: session.generations[..k].to_vec(),
        truncated: session.truncated,
        prompt_hashes: session.prompt_hashes.clone(),
    })
}

/// Ids of the turns an incremental send may emit: settled turns (no later
/// generation can change their payload, marks or parents) whose parents,
/// extra parents included, are all emitted. `final_` settles every turn.
/// Relies on `graph.nodes` being in view order, parents first, and on
/// `branches` being the classification of `graph`.
pub fn emitted_turns(graph: &TurnGraph, branches: &Branches, final_: bool) -> HashSet<String> {
    let mut extra: Vec<Vec<usize>> = vec![Vec::new(); graph.nodes.len()];
    for &(node, parent) in &branches.merges {
        extra[node].push(parent);
    }
    let mut emitted = HashSet::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        let settled =
            final_ || !branches.held[i] && (is_settled(node) || branches.answers.contains_key(&i));
        let parents_emitted = node.parent.as_ref().is_none_or(|p| emitted.contains(p))
            && extra[i]
                .iter()
                .all(|&p| emitted.contains(&graph.nodes[p].id));
        if settled && parents_emitted {
            emitted.insert(node.id.clone());
        }
    }
    emitted
}

/// Every call has a result no later prompt can replace, and a produced turn
/// has been echoed. A merged answer settles without an echo (`answers`),
/// and its step then leaves out an echo only a later generation carries.
fn is_settled(node: &Node) -> bool {
    let answered = node.message.tool_calls.iter().all(|c| {
        node.results
            .get(&c.id)
            .is_some_and(|r| node.producer != Some(r.generation))
    });
    answered && (node.producer.is_none() || node.echoed_by.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Generation;
    use crate::normalize::content_text;
    use crate::session::Session;
    use crate::stitch::stitch;
    use serde_json::{Value, json};

    fn generation(id: &str, start: u64, messages: Value, text: &str) -> Generation {
        let mut g = Generation {
            id: id.into(),
            trace_id: format!("trace-{id}"),
            start_ns: start,
            end_ns: start + 1,
            session_id: Some("s".into()),
            messages: serde_json::from_value(messages).unwrap(),
            ..Default::default()
        };
        g.completion.text = text.into();
        g
    }

    fn emitted_turns(g: &TurnGraph, final_: bool) -> HashSet<String> {
        let unknown =
            |name: &str| crate::tests::classifier::provider_tool_category("unknown", name);
        let b = crate::branch::classify(g, &unknown);
        super::emitted_turns(g, &b, final_)
    }

    fn graph(gens: Vec<Generation>) -> TurnGraph {
        stitch(&Session::new("s".into(), Some("s".into()), gens))
    }

    /// The id of the one turn with this role and text.
    fn turn(g: &TurnGraph, role: &str, text: &str) -> String {
        let hits: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.message.role == role && content_text(&n.message.content) == text)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(hits.len(), 1, "{role} {text:?}: {hits:?}");
        hits[0].to_string()
    }

    fn position(g: &TurnGraph, id: &str) -> usize {
        g.nodes.iter().position(|n| n.id == id).unwrap()
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn assistant(text: &str) -> Value {
        json!({"role": "assistant", "content": text})
    }

    #[test]
    fn a_produced_turn_waits_for_its_echo() {
        let g1 = generation("g1", 1, json!([user("go")]), "a");
        let one = graph(vec![g1.clone()]);
        assert!(
            emitted_turns(&one, false).is_empty(),
            "nothing goes out before the main line is decided"
        );

        let g2 = generation(
            "g2",
            2,
            json!([user("go"), assistant("a"), user("more")]),
            "b",
        );
        let two = graph(vec![g1, g2]);
        let open = emitted_turns(&two, false);
        for (role, text) in [("user", "go"), ("assistant", "a"), ("user", "more")] {
            assert!(open.contains(&turn(&two, role, text)), "{role} {text}");
        }
        assert!(!open.contains(&turn(&two, "assistant", "b")));
    }

    #[test]
    fn the_first_echo_names_its_generation() {
        let g = graph(vec![
            generation("g1", 1, json!([user("go")]), "a"),
            generation(
                "g2",
                2,
                json!([user("go"), assistant("a"), user("more")]),
                "b",
            ),
            generation(
                "g3",
                3,
                json!([
                    user("go"),
                    assistant("a"),
                    user("more"),
                    assistant("b"),
                    user("again")
                ]),
                "c",
            ),
        ]);
        let by = |t: &str| g.nodes[position(&g, &turn(&g, "assistant", t))].echoed_by;
        assert_eq!(
            by("a"),
            Some(1),
            "g2 echoes it first; g3 again changes nothing"
        );
        assert_eq!(by("b"), Some(2));
        assert_eq!(by("c"), None);
    }

    #[test]
    fn final_settles_every_turn() {
        let g = graph(vec![
            generation("g1", 1, json!([user("go")]), "a"),
            generation(
                "g2",
                2,
                json!([user("go"), assistant("a"), user("more")]),
                "b",
            ),
        ]);
        let all: HashSet<String> = g.nodes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(emitted_turns(&g, true), all);
    }

    #[test]
    fn a_history_only_assistant_turn_settles_on_sight() {
        let g = graph(vec![
            generation(
                "g1",
                1,
                json!([user("go"), assistant("earlier"), user("next")]),
                "a",
            ),
            generation(
                "g2",
                2,
                json!([
                    user("go"),
                    assistant("earlier"),
                    user("next"),
                    assistant("a"),
                    user("more")
                ]),
                "b",
            ),
        ]);
        let earlier = turn(&g, "assistant", "earlier");
        assert!(g.nodes[position(&g, &earlier)].producer.is_none());
        let open = emitted_turns(&g, false);
        assert!(open.contains(&earlier));
        assert!(open.contains(&turn(&g, "user", "next")));
    }

    #[test]
    fn fallback_tool_results_do_not_settle_a_turn() {
        let mut g1 = generation("g1", 1, json!([user("go")]), "");
        g1.completion.tool_calls = serde_json::from_value(json!([
            {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
        ]))
        .unwrap();
        g1.tool_results.insert(
            "t1".into(),
            serde_json::from_value(json!({"content": "out", "is_error": false})).unwrap(),
        );
        let g = graph(vec![g1]);
        let called = g
            .nodes
            .iter()
            .find(|n| n.producer == Some(0))
            .unwrap()
            .id
            .clone();
        assert!(!emitted_turns(&g, false).contains(&called));
        assert!(emitted_turns(&g, true).contains(&called));
    }

    #[test]
    fn a_settled_turn_under_a_withheld_parent_is_withheld() {
        // g2 continues g1 server-side: its Delta prompt chains from g1's
        // completion without carrying it, so that completion never settles.
        let g1 = generation("g1", 1, json!([user("go")]), "a");
        let mut g2 = generation("g2", 2, json!([user("more")]), "b");
        g2.continues = Some("g1".into());
        g2.history = serde_json::from_value(json!("delta")).unwrap();
        let g = graph(vec![g1, g2]);
        let a = turn(&g, "assistant", "a");
        let more = turn(&g, "user", "more");
        assert_eq!(
            g.nodes[position(&g, &more)].parent.as_deref(),
            Some(a.as_str())
        );
        assert!(g.nodes[position(&g, &a)].echoed_by.is_none());
        let open = emitted_turns(&g, false);
        assert!(!open.contains(&a));
        assert!(!open.contains(&more), "settled, but its parent is withheld");
        assert!(emitted_turns(&g, true).contains(&more));
    }

    #[test]
    fn an_abandoned_retry_holds_back_only_its_own_branch() {
        let g = graph(vec![
            generation("g1", 1, json!([user("go")]), "a"),
            generation(
                "g2",
                2,
                json!([user("go"), assistant("a"), user("more")]),
                "b",
            ),
            generation(
                "g2r",
                3,
                json!([user("go"), assistant("a"), user("more")]),
                "b-retry",
            ),
            generation(
                "g3",
                4,
                json!([
                    user("go"),
                    assistant("a"),
                    user("more"),
                    assistant("b"),
                    user("again")
                ]),
                "c",
            ),
        ]);
        let retry = turn(&g, "assistant", "b-retry");
        let again = turn(&g, "user", "again");
        // In view order the retry comes before `again`: a prefix cut would
        // hold `again` back until `final`.
        assert!(position(&g, &retry) < position(&g, &again));
        let open = emitted_turns(&g, false);
        assert!(!open.contains(&retry));
        assert!(open.contains(&turn(&g, "assistant", "b")), "echoed by g3");
        assert!(open.contains(&again));
        assert!(!open.contains(&turn(&g, "assistant", "c")));
    }

    fn two_calls() -> Value {
        json!([
            {"id": "c1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
            {"id": "c2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
        ])
    }

    fn tool(id: &str, text: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": text})
    }

    /// g1 calls c1 and c2 and holds a fallback for c2; g2 echoes the call
    /// turn but answers only c1.
    fn partially_answered() -> (Generation, Generation) {
        let mut g1 = generation("g1", 1, json!([user("go")]), "");
        g1.completion.tool_calls = serde_json::from_value(two_calls()).unwrap();
        g1.tool_results.insert(
            "c2".into(),
            serde_json::from_value(json!({"content": "fb2", "is_error": false})).unwrap(),
        );
        let echo = json!({"role": "assistant", "content": null, "tool_calls": two_calls()});
        let g2 = generation(
            "g2",
            2,
            json!([user("go"), echo, tool("c1", "real1")]),
            "ok",
        );
        (g1, g2)
    }

    #[test]
    fn an_echoed_turn_with_a_fallback_result_is_withheld() {
        let (g1, g2) = partially_answered();
        let g = graph(vec![g1, g2]);
        let called = &g.nodes[g.nodes.iter().position(|n| n.producer == Some(0)).unwrap()];
        assert!(called.echoed_by.is_some());
        assert_eq!(called.results["c2"].content, "fb2");
        let open = emitted_turns(&g, false);
        assert!(!open.contains(&called.id), "c2 still holds a fallback");
        assert!(
            !open.contains(&turn(&g, "assistant", "ok")),
            "under a withheld parent"
        );
        assert!(emitted_turns(&g, true).contains(&called.id));
    }

    #[test]
    fn an_echoed_turn_settles_once_every_result_is_prompt_carried() {
        let (g1, g2) = partially_answered();
        let echo = json!({"role": "assistant", "content": null, "tool_calls": two_calls()});
        let g3 = generation(
            "g3",
            3,
            json!([user("go"), echo, tool("c1", "real1"), tool("c2", "real2")]),
            "ok2",
        );
        let g = graph(vec![g1, g2, g3]);
        let called = &g.nodes[g.nodes.iter().position(|n| n.producer == Some(0)).unwrap()];
        assert_eq!(called.results["c2"].content, "real2");
        assert!(emitted_turns(&g, false).contains(&called.id));
    }

    #[test]
    fn a_history_only_turn_with_an_unanswered_call_is_withheld() {
        let earlier = json!({"role": "assistant", "content": null, "tool_calls": two_calls()});
        let g1 = generation(
            "g1",
            1,
            json!([user("go"), earlier.clone(), tool("c1", "real1")]),
            "a",
        );
        let one = graph(vec![g1.clone()]);
        let hist = one
            .nodes
            .iter()
            .find(|n| n.message.tool_calls.len() == 2)
            .unwrap();
        assert!(hist.producer.is_none());
        assert!(!emitted_turns(&one, false).contains(&hist.id));

        let g2 = generation(
            "g2",
            2,
            json!([
                user("go"),
                earlier,
                tool("c1", "real1"),
                tool("c2", "real2")
            ]),
            "b",
        );
        let two = graph(vec![g1, g2]);
        let hist = two
            .nodes
            .iter()
            .find(|n| n.message.tool_calls.len() == 2)
            .unwrap();
        assert!(emitted_turns(&two, false).contains(&hist.id));
    }
}
