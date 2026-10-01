//! The settled frontier: which turns an incremental send may emit.

use super::{Node, ToolOutcome, TurnGraph, stitch};
use crate::ToolClassifier;
use crate::branch::{Branches, classify};
use crate::derive::categories;
use crate::harness::shell_writes::may_move_shell;
use crate::harness::{SourceHarness, infer_harness, prefix_harnesses, signals};
use crate::session::Session;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use toolpath_convo::ToolCategory;

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
///
/// Each run of one harness along the feed is searched by doubling, then
/// bisecting: emission is monotone in the prefix length within a run, not
/// across runs (`docs/agents/formats/otel.md`, "Harness and working
/// directory"). Only the whole feed is probed with `final_`.
pub fn first_settle(
    session: &Session,
    final_: bool,
    classifier: Option<&ToolClassifier>,
) -> (usize, SourceHarness) {
    let settled = search_settle(session, final_, classifier);
    #[cfg(test)]
    if session.generations.len() <= ORACLE_MAX {
        assert_eq!(
            settled,
            naive_first_settle(session, final_, classifier),
            "{}",
            session.key
        );
    }
    settled
}

fn search_settle(
    session: &Session,
    final_: bool,
    classifier: Option<&ToolClassifier>,
) -> (usize, SourceHarness) {
    let n = session.generations.len();
    let harnesses = prefix_harnesses(session);
    let mut start = 1;
    while start <= n {
        let h = harnesses[start - 1];
        let end = (start..=n)
            .take_while(|&k| harnesses[k - 1] == h)
            .last()
            .unwrap_or(start);
        if let Some(k) = first_true(start, end, |k| emits_at(session, k, false, h, classifier)) {
            return (k, h);
        }
        if end == n && final_ && emits_at(session, n, true, h, classifier) {
            return (n, h);
        }
        start = end + 1;
    }
    (n, infer_harness(&signals(session)))
}

/// The first `k` in `start..=end` where `pred`, monotone there, holds.
fn first_true(start: usize, end: usize, mut pred: impl FnMut(usize) -> bool) -> Option<usize> {
    let (mut lo, mut step) = (start - 1, 1);
    let mut hi = loop {
        let k = (lo + step).min(end);
        if pred(k) {
            break k;
        }
        if k == end {
            return None;
        }
        lo = k;
        step *= 2;
    };
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// Whether the first `k` generations, whose harness is `harness`, emit a
/// turn.
fn emits_at(
    session: &Session,
    k: usize,
    final_: bool,
    harness: SourceHarness,
    classifier: Option<&ToolClassifier>,
) -> bool {
    let feed = prefix(session, k);
    #[cfg(test)]
    STITCHED.with(|c| c.set(c.get() + k));
    debug_assert_eq!(harness, infer_harness(&signals(&feed)));
    let graph = stitch(&feed);
    let category = categories(classifier, harness);
    let branches = classify(&graph, &category);
    !emitted_turns(&graph, &branches, final_, &category).is_empty()
}

/// Sessions up to this long are checked against the naive search on every
/// `first_settle` call under test.
#[cfg(test)]
const ORACLE_MAX: usize = 64;

/// The settle search as first written: every prefix, shortest first.
#[cfg(test)]
fn naive_first_settle(
    session: &Session,
    final_: bool,
    classifier: Option<&ToolClassifier>,
) -> (usize, SourceHarness) {
    let n = session.generations.len();
    for k in 1..=n {
        let feed = prefix(session, k);
        let harness = infer_harness(&signals(&feed));
        let graph = stitch(&feed);
        let category = categories(classifier, harness);
        let branches = classify(&graph, &category);
        if !emitted_turns(&graph, &branches, final_ && k == n, &category).is_empty() {
            return (k, harness);
        }
    }
    (n, infer_harness(&signals(session)))
}

#[cfg(test)]
thread_local! {
    /// Generations stitched by settle probes on this thread.
    static STITCHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
/// extra parents included, are all emitted. A turn's shell-write stamps
/// also read where calls off its ancestry, from generations before its
/// first, left a persistent shell, so a turn also waits for every
/// unsettled turn that may move one ([`shell_generation`]) and comes from
/// an earlier generation. `final_` settles every turn. Relies on
/// `graph.nodes` being in view order, parents first, and on `branches`
/// being the classification of `graph` under `category`.
pub fn emitted_turns(
    graph: &TurnGraph,
    branches: &Branches,
    final_: bool,
    category: &dyn Fn(&str) -> Option<ToolCategory>,
) -> HashSet<String> {
    let unsettled_move = (!final_)
        .then(|| {
            graph
                .nodes
                .iter()
                .filter(|n| !is_settled(n) && moves_shell(n, None, category))
                .map(shell_generation)
                .min()
        })
        .flatten();
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
        let unblocked = unsettled_move.is_none_or(|g| node.first_generation <= g);
        if settled && parents_emitted && unblocked {
            emitted.insert(node.id.clone());
        }
    }
    emitted
}

/// Whether an answer to one of the turn's calls, the one it has (`results`,
/// else its own) or one still to come, can decide where it leaves a
/// persistent shell ([`may_move_shell`]).
pub(crate) fn moves_shell(
    node: &Node,
    results: Option<&BTreeMap<String, ToolOutcome>>,
    category: &dyn Fn(&str) -> Option<ToolCategory>,
) -> bool {
    let results = results.unwrap_or(&node.results);
    node.message.tool_calls.iter().any(|c| {
        let result = results.get(&c.id).map(|r| r.content.as_str());
        let name = &c.function.name;
        may_move_shell(name, category(name), &c.function.parsed_arguments(), result)
    })
}

/// The earliest generation a shell move of this turn can be attributed to:
/// turns whose first generation is later read it off their ancestry.
pub(crate) fn shell_generation(node: &Node) -> usize {
    node.producer
        .map_or(node.first_generation, |p| p.min(node.first_generation))
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
    use crate::harness::SourceHarness;
    use crate::normalize::content_text;
    use crate::session::Session;
    use crate::stitch::stitch;
    use crate::tests::otel::tools;
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
        super::emitted_turns(g, &b, final_, &unknown)
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

    /// Every prefix of `session`, both settles: the search equals the naive
    /// one. Returns how many harness runs the whole feed has.
    fn assert_settles_like_naive(session: &Session) -> usize {
        for k in 0..=session.generations.len() {
            let p = prefix(session, k);
            for final_ in [false, true] {
                assert_eq!(
                    search_settle(&p, final_, tools()),
                    naive_first_settle(&p, final_, tools()),
                    "{} prefix {k} final {final_}",
                    session.key
                );
            }
        }
        let mut runs = prefix_harnesses(session);
        runs.dedup();
        runs.len()
    }

    #[test]
    fn the_settle_search_equals_the_naive_one_on_every_fixture() {
        for s in crate::stitch::tests::fixture_sessions() {
            assert_settles_like_naive(&s);
        }
    }

    /// xorshift64*: deterministic, no dependency.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n
        }

        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            &xs[self.below(xs.len())]
        }
    }

    /// A session mixing shared and independent histories, sub-agent
    /// prompts and delegation calls, side system prompts, tool results in
    /// prompts or only as fallbacks, deltas (found and missing targets),
    /// and every harness signal, so prefixes change harness.
    fn random_session(rng: &mut Rng) -> Session {
        let sid = *rng.pick(&[
            "s",
            "3f2b1c4e-1a2b-4c3d-8e9f-0a1b2c3d4e5f",
            "01890a5d-ac96-774b-bcce-b302099a8057",
            "ses_abc",
        ]);
        let n = 1 + rng.below(16);
        let mut gens: Vec<Generation> = Vec::new();
        let mut histories: Vec<Vec<Value>> = Vec::new();
        for i in 0..n {
            let mut prompt: Vec<Value> = if histories.is_empty() || rng.below(3) == 0 {
                let mut p = Vec::new();
                if let Some(sys) =
                    rng.pick(&[None, Some("MAIN"), Some("SUB"), Some("You are Claude Code")])
                {
                    p.push(json!({"role": "system", "content": sys}));
                }
                if rng.below(6) == 0 {
                    p.push(json!({"role": "developer", "content": "dev"}));
                }
                let text = match rng.below(3) {
                    0 => format!("sub {}", rng.below(3)),
                    _ => format!("q{}", rng.below(4)),
                };
                p.push(user(&text));
                p
            } else {
                let j = rng.below(histories.len());
                let mut p = histories[j].clone();
                if rng.below(2) == 0 {
                    p.push(user(&format!("more {}", rng.below(3))));
                }
                p
            };
            let mut g = generation(&format!("g{i}"), i as u64, json!([]), "");
            g.session_id = Some(sid.into());
            if rng.below(5) == 0 {
                g.request_session_id = Some("r".into());
            }
            if !gens.is_empty() && rng.below(6) == 0 {
                let target = rng.below(gens.len() + 1);
                g.continues = Some(gens.get(target).map_or("gone".into(), |t| t.id.clone()));
                g.history = serde_json::from_value(json!("delta")).unwrap();
                prompt = vec![user(&format!("delta {}", rng.below(2)))];
            }
            g.messages = serde_json::from_value(Value::Array(prompt.clone())).unwrap();
            let mut calls = Vec::new();
            for c in 0..rng.below(3) {
                let name = *rng.pick(&[
                    "read",
                    "bash",
                    "Agent",
                    "Task",
                    "task",
                    "Grep",
                    "spawn_agent",
                ]);
                let args = json!({"prompt": format!("sub {}", rng.below(3))}).to_string();
                calls.push(json!({"id": format!("c{i}-{c}"), "type": "function",
                    "function": {"name": name, "arguments": args}}));
            }
            if calls.is_empty() {
                g.completion.text = format!("a{}", rng.below(4));
            }
            g.completion.tool_calls = serde_json::from_value(Value::Array(calls.clone())).unwrap();
            let mut history = prompt;
            if calls.is_empty() {
                history.push(assistant(&g.completion.text));
            } else {
                history.push(json!({"role": "assistant", "content": null, "tool_calls": calls}));
                for c in &calls {
                    let id = c["id"].as_str().unwrap();
                    match rng.below(3) {
                        0 => {}
                        1 => history.push(json!({"role": "tool", "tool_call_id": id,
                            "content": format!("a{}", rng.below(4))})),
                        _ => {
                            g.tool_results.insert(
                                id.into(),
                                serde_json::from_value(json!({"content": "fb", "is_error": false}))
                                    .unwrap(),
                            );
                        }
                    }
                }
            }
            histories.push(history);
            gens.push(g);
        }
        Session::new(sid.into(), Some(sid.into()), gens)
    }

    #[test]
    fn the_settle_search_equals_the_naive_one_on_random_sessions() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut changed_harness, mut late, mut never) = (0, 0, 0);
        for _ in 0..400 {
            let s = random_session(&mut rng);
            let n = s.generations.len();
            if assert_settles_like_naive(&s) > 1 {
                changed_harness += 1;
            }
            match naive_first_settle(&s, false, tools()).0 {
                k if k == n => never += 1,
                k if k > 1 => late += 1,
                _ => {}
            }
            let mut reversed = s.clone();
            reversed.generations.reverse();
            assert_settles_like_naive(&reversed);
        }
        assert!(
            changed_harness > 20 && late > 20 && never > 20,
            "{changed_harness} {late} {never}"
        );
    }

    /// Emission is not monotone across a harness change: a sub-agent's
    /// root settles under Claude Code's `Agent`, and a developer message
    /// then makes the session codex's, where `Agent` delegates nothing.
    #[test]
    fn a_harness_change_restarts_the_search() {
        let v7 = "01890a5d-ac96-774b-bcce-b302099a8057";
        let system = |t: &str| json!({"role": "system", "content": t});
        let mut g0 = generation(
            "g0",
            0,
            json!([system("You are Claude Code"), user("go")]),
            "",
        );
        g0.completion.tool_calls = serde_json::from_value(json!([{"id": "c1", "type": "function",
            "function": {"name": "Agent", "arguments": "{\"prompt\": \"sub 0\"}"}}]))
        .unwrap();
        let g1 = generation("g1", 1, json!([system("SUB"), user("sub 0")]), "done");
        let g2 = generation(
            "g2",
            2,
            json!([{"role": "developer", "content": "dev"}, user("other")]),
            "x",
        );
        let mut s = Session::new(v7.into(), Some(v7.into()), vec![g0, g1, g2]);
        for g in &mut s.generations {
            g.session_id = Some(v7.into());
        }
        assert_eq!(
            prefix_harnesses(&s),
            [
                SourceHarness::ClaudeCode,
                SourceHarness::ClaudeCode,
                SourceHarness::Codex
            ]
        );
        let at = |k: usize, h| {
            let feed = prefix(&s, k);
            let graph = stitch(&feed);
            let category = categories(tools(), h);
            !super::emitted_turns(&graph, &classify(&graph, &category), false, &category).is_empty()
        };
        assert!(at(2, SourceHarness::ClaudeCode));
        assert!(!at(3, SourceHarness::Codex));
        assert_eq!(
            first_settle(&s, false, tools()),
            (2, SourceHarness::ClaudeCode)
        );
        assert_settles_like_naive(&s);
    }

    /// `n` requests of one session that never share history.
    fn independent_prompts(n: usize) -> Session {
        let gens = (0..n)
            .map(|i| {
                let t = i as u64;
                generation(&format!("g{i}"), t, json!([user(&format!("q{i}"))]), "a")
            })
            .collect();
        Session::new("s".into(), Some("s".into()), gens)
    }

    #[test]
    fn settling_a_session_that_never_settles_stitches_near_linearly() {
        let n = 1000;
        let s = independent_prompts(n);
        STITCHED.with(|c| c.set(0));
        assert_eq!(first_settle(&s, true, tools()), (n, SourceHarness::Unknown));
        let stitched = STITCHED.with(|c| c.get());
        assert!(stitched <= 4 * n, "{stitched} generations stitched for {n}");
    }

    /// `cargo test --release -p toolpath-otel --lib settle_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn settle_timing() {
        for n in [250, 500, 1000] {
            let s = independent_prompts(n);
            let t = std::time::Instant::now();
            let settled = first_settle(&s, true, tools());
            let settle = t.elapsed();
            let t = std::time::Instant::now();
            let graph = stitch(&s);
            let once = t.elapsed();
            assert_eq!(settled.0, n);
            assert_eq!(graph.links.len(), n);
            println!("n={n}: first_settle {settle:?}, one stitch {once:?}");
        }
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
