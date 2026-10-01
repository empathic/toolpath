//! Which turns are the main line, a sub-agent's work, or a side request,
//! so that work the session did on purpose does not read as a dead end.
//!
//! A *thread* is everything below a first user message (its *anchor*). A
//! thread whose anchor text ends with the `prompt` of an earlier
//! delegation call (`Task`, `Agent`, `task`) is that call's sub-agent;
//! threads take calls first come, first served in feed order, so with
//! duplicate prompts the first thread takes the first call. Its answer (a
//! final assistant turn whose text the call's result or a later turn of
//! the delegating thread carries) becomes an extra parent of the
//! delegating thread's turn that receives it, so the sub-agent's turns are
//! ancestors of the head. A sub-agent that answers twice merges at the
//! first answer received in feed order; a later answer never moves it. A
//! thread under a different leading system message than the main line's is
//! a side request.
//!
//! Every mark depends only on turns that come earlier in feed order, or on
//! the turn's own data, so appending generations never changes a mark a
//! turn already has: the delegating turn's `delegations` come from its own
//! calls and results, a thread is matched only to a call that precedes it,
//! and the main line is the first leading system message to produce two
//! turns.

use crate::harness::SourceHarness;
use crate::harness::tools::tool_category;
use crate::normalize::content_text;
use crate::stitch::TurnGraph;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use toolpath_convo::ToolCategory;

/// What an off-main-line turn is, stamped as `extra.otel.branch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchKind {
    /// Work of the sub-agent started by this tool call.
    Subagent(String),
    /// A request under another system prompt (titles, classifiers, …).
    Side,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    pub call_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Default)]
pub struct Branches {
    /// Per node (graph order): `None` for the main line.
    pub kind: Vec<Option<BranchKind>>,
    /// Delegation calls with a prompt, by calling node index, in call
    /// order, whether or not a sub-agent thread matched them.
    pub delegations: BTreeMap<usize, Vec<Delegation>>,
    /// `(node, extra parent)`: a sub-agent's last turn joins the delegating
    /// thread at `node`.
    pub merges: Vec<(usize, usize)>,
    /// The main line's last turn.
    pub head: Option<usize>,
    /// Per node: held back from a non-final incremental send whatever its
    /// own state, because a later generation could still change its marks
    /// or parents (`docs/agents/formats/otel.md`, "Incremental JSONL").
    pub held: Vec<bool>,
    /// Merged sub-agent answers, by node, with the generation (feed order)
    /// by which the merge is seen. The answer settles there: an echo that
    /// only a later generation carries (a resumed sub-agent) is not part of
    /// its step.
    pub answers: BTreeMap<usize, usize>,
}

impl Branches {
    pub fn branch_name(&self, ni: usize) -> Option<&'static str> {
        match self.kind.get(ni)? {
            Some(BranchKind::Subagent(_)) => Some("subagent"),
            Some(BranchKind::Side) => Some("side"),
            None => None,
        }
    }
}

struct Call {
    node: usize,
    id: String,
    prompt: String,
}

pub fn classify(graph: &TurnGraph, harness: SourceHarness) -> Branches {
    let nodes = &graph.nodes;
    let n = nodes.len();
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, x)| (x.id.as_str(), i))
        .collect();
    let parent: Vec<Option<usize>> = nodes
        .iter()
        .map(|x| x.parent.as_deref().and_then(|p| index.get(p).copied()))
        .collect();

    let mut root = vec![0; n];
    let mut anchor: Vec<Option<usize>> = vec![None; n];
    for i in order_parents_first(&parent) {
        match parent[i] {
            Some(p) => {
                root[i] = root[p];
                anchor[i] = anchor[p];
            }
            None => root[i] = i,
        }
        if anchor[i].is_none() && nodes[i].message.role == "user" {
            anchor[i] = Some(i);
        }
    }

    let mut calls: Vec<Call> = Vec::new();
    for (i, x) in nodes.iter().enumerate() {
        for c in &x.message.tool_calls {
            if tool_category(harness, &c.function.name) != Some(ToolCategory::Delegation) {
                continue;
            }
            let args = c.function.parsed_arguments();
            if let Some(p) = args.get("prompt").and_then(|v| v.as_str()) {
                let p = p.trim();
                if !p.is_empty() {
                    calls.push(Call {
                        node: i,
                        id: c.id.clone(),
                        prompt: p.to_string(),
                    });
                }
            }
        }
    }

    // Threads in feed order, each matched to the earliest call before it
    // that no earlier thread matched: with duplicate prompts the first
    // thread takes the first call, and a later thread or call never moves it.
    let mut delegated: BTreeMap<usize, usize> = BTreeMap::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    for a in (0..n).filter(|&i| anchor[i] == Some(i)) {
        let text = content_text(&nodes[a].message.content);
        let text = text.trim_end();
        let hit = (0..calls.len()).find(|&ci| {
            !used.contains(&ci) && calls[ci].node < a && text.ends_with(calls[ci].prompt.as_str())
        });
        if let Some(ci) = hit {
            used.insert(ci);
            delegated.insert(a, ci);
        }
    }

    let delegated_of = |i: usize| anchor[i].and_then(|a| delegated.get(&a).copied());
    // Produced turns outside sub-agent threads, in feed order. The main line
    // is the first root to produce two of them, else the first one's root:
    // a later generation comes later in feed order, so the choice holds.
    let mut produced: Vec<(usize, usize)> = (0..n)
        .filter(|&i| delegated_of(i).is_none())
        .filter_map(|i| nodes[i].producer.map(|g| (g, root[i])))
        .collect();
    produced.sort_unstable();
    let mut counts: HashMap<usize, usize> = HashMap::new();
    let decided = produced.iter().find(|(_, r)| {
        let c = counts.entry(*r).or_default();
        *c += 1;
        *c == 2
    });
    let main_root = decided.or(produced.first()).map(|(_, r)| *r);

    let mut kind: Vec<Option<BranchKind>> = (0..n)
        .map(|i| match delegated_of(i) {
            Some(ci) => Some(BranchKind::Subagent(calls[ci].id.clone())),
            None if main_root.is_some_and(|m| root[i] != m) => Some(BranchKind::Side),
            None => None,
        })
        .collect();
    // A system turn above only sub-agent threads belongs to them.
    for i in (0..n).rev() {
        if anchor[i].is_none()
            && kind[i] == Some(BranchKind::Side)
            && let Some(sub) =
                (i + 1..n)
                    .filter(|&j| parent[j] == Some(i))
                    .find_map(|j| match &kind[j] {
                        Some(BranchKind::Subagent(c)) => Some(c.clone()),
                        _ => None,
                    })
        {
            kind[i] = Some(BranchKind::Subagent(sub));
        }
    }

    // From the delegating turn's own calls, matched or not, so the turn
    // never changes when a sub-agent's requests arrive. `calls` is in node
    // then call order, so each list is in call order.
    let mut delegations: BTreeMap<usize, Vec<Delegation>> = BTreeMap::new();
    for c in &calls {
        delegations.entry(c.node).or_default().push(Delegation {
            call_id: c.id.clone(),
            prompt: c.prompt.clone(),
        });
    }

    let generation = |i: usize| nodes[i].producer.unwrap_or(nodes[i].first_generation);
    let thread = |i: usize| delegated_of(i).map(|ci| calls[ci].id.as_str());
    let is_descendant = |mut j: usize, of: usize| loop {
        if j == of {
            return true;
        }
        match parent[j] {
            Some(p) => j = p,
            None => return false,
        }
    };
    let texts: Vec<String> = nodes
        .iter()
        .map(|x| content_text(&x.message.content))
        .collect();
    let mut merges = Vec::new();
    let mut answers = BTreeMap::new();
    let mut returned_calls = BTreeSet::new();
    for (&a, &ci) in &delegated {
        let from = calls[ci].node;
        let result = nodes[from].results.get(&calls[ci].id);
        let in_thread = |j: usize, last: usize| {
            j > from.max(last)
                && thread(j) == thread(from)
                && kind[j] == kind[from]
                && is_descendant(j, from)
        };
        // The sub-agent's answer: a final assistant turn whose text comes
        // back in the call's result or in a later delegating-thread turn.
        // A sub-agent that answers twice merges at the turn that receives
        // an answer first in feed order (of the answers it receives, the
        // earliest), so a later answer never moves the merge.
        let finals = (0..n).filter(|&i| {
            anchor[i] == Some(a)
                && nodes[i].producer.is_some()
                && nodes[i].message.tool_calls.is_empty()
                && !texts[i].trim().is_empty()
        });
        // `(receiving turn, answer, generation by which the merge is seen)`.
        let join = finals.filter_map(|last| {
            let answer = texts[last].trim();
            let carried = (from + 1..n)
                .find(|&j| in_thread(j, last) && texts[j].contains(answer))
                .map(|j| (j, nodes[j].first_generation));
            let returned = result.filter(|r| r.content.contains(answer)).and_then(|r| {
                (from + 1..n)
                    .find(|&j| in_thread(j, last) && nodes[j].first_generation > generation(last))
                    .map(|j| (j, nodes[j].first_generation.max(r.generation)))
            });
            [carried, returned]
                .into_iter()
                .flatten()
                .min()
                .map(|(j, seen)| (j, last, seen.max(generation(last))))
        });
        if let Some((j, last, seen)) = join.min() {
            merges.push((j, last));
            answers.insert(last, seen);
            returned_calls.insert(ci);
        }
    }
    merges.sort_unstable();

    // Until the main line is decided every turn may still change kind; a
    // side request waits for `final_`; a system turn above sub-agent
    // threads takes its first thread's call, so it waits for that one.
    let mut held = vec![decided.is_none(); n];
    for i in 0..n {
        let first_child = || (i + 1..n).find(|&j| parent[j] == Some(i));
        held[i] |= match &kind[i] {
            Some(BranchKind::Side) => true,
            Some(BranchKind::Subagent(_)) if anchor[i].is_none() => {
                first_child().is_none_or(|j| !matches!(kind[j], Some(BranchKind::Subagent(_))))
            }
            _ => false,
        };
    }
    // A call whose sub-agent has not answered yet may still give any later
    // turn of its thread an extra parent.
    for (ci, c) in calls.iter().enumerate() {
        if returned_calls.contains(&ci) {
            continue;
        }
        let from = c.node;
        // Parents come before their children in graph order.
        let mut below = vec![false; n];
        below[from] = true;
        for j in from + 1..n {
            below[j] = parent[j].is_some_and(|p| below[p]);
            if below[j] && thread(j) == thread(from) && kind[j] == kind[from] {
                held[j] = true;
            }
        }
    }
    let head = (0..n)
        .rev()
        .find(|&i| kind[i].is_none())
        .or(n.checked_sub(1));
    Branches {
        kind,
        delegations,
        merges,
        head,
        held,
        answers,
    }
}

/// Node indices with every parent before its children (graph order when
/// it already is).
fn order_parents_first(parent: &[Option<usize>]) -> Vec<usize> {
    let n = parent.len();
    let mut seen = vec![false; n];
    let mut out = Vec::with_capacity(n);
    for start in 0..n {
        let mut chain = Vec::new();
        let mut i = start;
        while !seen[i] {
            seen[i] = true;
            chain.push(i);
            match parent[i] {
                Some(p) => i = p,
                None => break,
            }
        }
        out.extend(chain.into_iter().rev());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::derive_session;
    use crate::generation::{Completion, Cost, FunctionCall, Generation, Message, ToolCall};
    use crate::session::Session;
    use crate::stitch::stitch;
    use serde_json::{Value, json};
    use toolpath::v1::query;

    fn m(v: Value) -> Message {
        serde_json::from_value(v).unwrap()
    }

    fn agent(id: &str, prompt: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: "Agent".into(),
                arguments: Value::String(json!({"description": "d", "prompt": prompt}).to_string()),
            },
        }
    }

    fn generation(
        id: &str,
        start: u64,
        messages: Vec<Message>,
        completion: Completion,
    ) -> Generation {
        Generation {
            id: id.into(),
            start_ns: start,
            end_ns: start + 1,
            messages: messages.into(),
            completion,
            cost: Cost {
                total: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn text(t: &str) -> Completion {
        Completion {
            text: t.into(),
            ..Default::default()
        }
    }

    /// The live shape: one main request fans out three parallel `Agent`
    /// sub-agents (shared sub-agent system prompt, the call's prompt as the
    /// last text part of the first user message), their results come back
    /// as tool messages, and a side request under another system prompt
    /// arrives last.
    fn fan_out() -> Session {
        let sys = |t: &str| m(json!({"role": "system", "content": t}));
        let user = || m(json!({"role": "user", "content": "do it"}));
        let calls = vec![
            agent("c1", "sub A"),
            agent("c2", "sub B"),
            agent("c3", "sub C"),
        ];
        let fan = Completion {
            tool_calls: calls.clone(),
            ..Default::default()
        };
        let sub = |id: &str, start, p: &str, out: &str| {
            let first = m(json!({"role": "user", "content": [
                {"type": "text", "text": "<system-reminder>ctx</system-reminder>"},
                {"type": "text", "text": p}]}));
            generation(id, start, vec![sys("SUB"), first], text(out))
        };
        let mut resumed = vec![sys("MAIN"), user()];
        resumed.push(Message {
            role: "assistant".into(),
            tool_calls: calls,
            ..Default::default()
        });
        for (c, r) in [("c1", "A done"), ("c2", "B done"), ("c3", "C done")] {
            resumed.push(m(json!({"role": "tool", "tool_call_id": c, "content": r})));
        }
        Session::new(
            "s-fan".into(),
            Some("s-fan".into()),
            vec![
                generation("g0", 10, vec![sys("MAIN"), user()], fan),
                sub("g1", 20, "sub A", "A done"),
                sub("g2", 21, "sub B", "B done"),
                sub("g3", 22, "sub C", "C done"),
                generation("g4", 30, resumed, text("all done")),
                generation(
                    "g5",
                    40,
                    vec![
                        sys("TITLE"),
                        m(json!({"role": "user", "content": "name it"})),
                    ],
                    text("Title"),
                ),
            ],
        )
    }

    fn produced_by(g: &TurnGraph, gi: usize) -> usize {
        g.nodes.iter().position(|n| n.producer == Some(gi)).unwrap()
    }

    #[test]
    fn parallel_sub_agents_are_attributed_and_merge_back() {
        let s = fan_out();
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        let fan = produced_by(&g, 0);
        let calls: Vec<&str> = b.delegations[&fan]
            .iter()
            .map(|d| d.call_id.as_str())
            .collect();
        assert_eq!(calls, ["c1", "c2", "c3"]);
        for (gi, call) in [(1, "c1"), (2, "c2"), (3, "c3")] {
            assert_eq!(
                b.kind[produced_by(&g, gi)],
                Some(BranchKind::Subagent(call.into()))
            );
        }
        let done = produced_by(&g, 4);
        assert_eq!(b.head, Some(done), "the side request is not the head");
        assert_eq!(b.kind[produced_by(&g, 5)], Some(BranchKind::Side));
        let merged: Vec<usize> = b
            .merges
            .iter()
            .filter(|(n, _)| *n == done)
            .map(|(_, p)| *p)
            .collect();
        assert_eq!(merged, [1, 2, 3].map(|gi| produced_by(&g, gi)));
    }

    #[test]
    fn sub_agent_steps_are_not_dead_ends_and_side_steps_say_so() {
        let s = fan_out();
        let g = stitch(&s);
        let p = derive_session(&s, &Default::default());
        assert_eq!(p.path.head, g.nodes[produced_by(&g, 4)].id);
        let dead: Vec<&str> = query::dead_ends(&p.steps, &p.path.head)
            .into_iter()
            .map(|s| s.step.id.as_str())
            .collect();
        let side: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.first_generation == 5)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            dead, side,
            "only the side request is off the head's ancestry"
        );
        let extra = |id: &str| {
            let step = p.steps.iter().find(|s| s.step.id == id).unwrap();
            let v = serde_json::to_value(step).unwrap();
            v["change"].as_object().unwrap().values().next().unwrap()["structural"].clone()
        };
        for id in &side {
            assert_eq!(extra(id)["otel"]["branch"], "side");
        }
        let sub = extra(&g.nodes[produced_by(&g, 2)].id);
        assert_eq!(sub["otel"]["branch"], "subagent");
        assert_eq!(sub["otel"]["delegation"], "c2");
        let fan = extra(&g.nodes[produced_by(&g, 0)].id);
        assert_eq!(
            fan["delegations"][1],
            json!({"agent_id": "c2", "prompt": "sub B", "result": "B done"})
        );
        assert!(extra(&p.path.head)["otel"].get("branch").is_none());
    }

    /// Background agents: the call's result is only an acknowledgement and
    /// each answer arrives later in a user notification.
    #[test]
    fn a_background_sub_agent_joins_where_its_answer_arrives() {
        let mut s = fan_out();
        let mut resumed = s.generations[4].messages.to_vec();
        for t in resumed.iter_mut().filter(|t| t.role == "tool") {
            t.content = json!("launched");
        }
        resumed.push(m(json!({"role": "assistant", "content": "waiting"})));
        resumed.push(m(
            json!({"role": "user", "content": "<task-notification>B done</task-notification>"}),
        ));
        s.generations[4].messages = resumed.into();
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        let note = g
            .nodes
            .iter()
            .position(|n| n.message.role == "user" && texts_of(n).contains("B done"))
            .unwrap();
        assert_eq!(b.merges, [(note, produced_by(&g, 2))]);
    }

    fn texts_of(n: &crate::stitch::Node) -> String {
        crate::normalize::content_text(&n.message.content)
    }

    #[test]
    fn an_unfinished_sub_agent_does_not_merge() {
        let mut s = fan_out();
        s.generations.truncate(4);
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        assert!(b.merges.is_empty());
        assert_eq!(b.head, Some(produced_by(&g, 0)));
    }

    #[test]
    fn a_prompt_without_a_matching_thread_still_lists_its_delegation() {
        let s = Session::new(
            "s".into(),
            None,
            vec![generation(
                "g0",
                1,
                vec![m(json!({"role": "user", "content": "go"}))],
                Completion {
                    tool_calls: vec![agent("c1", "never started")],
                    ..Default::default()
                },
            )],
        );
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        assert_eq!(
            b.delegations[&produced_by(&g, 0)],
            [Delegation {
                call_id: "c1".into(),
                prompt: "never started".into()
            }]
        );
        assert!(b.kind.iter().all(Option::is_none));
        assert!(b.merges.is_empty());
    }

    /// A side request that starts first does not take the main line: the
    /// first leading system message to produce two turns does.
    #[test]
    fn the_main_line_is_the_first_to_produce_two_turns() {
        let mut s = fan_out();
        let title = s.generations.pop().unwrap();
        s.generations.insert(0, title);
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        assert_eq!(b.kind[produced_by(&g, 0)], Some(BranchKind::Side));
        assert_eq!(b.head, Some(produced_by(&g, 5)));
    }

    /// Appending generations never changes a mark a turn already has.
    #[test]
    fn marks_hold_as_generations_are_appended() {
        let s = fan_out();
        type Marks = HashMap<String, (Option<BranchKind>, Vec<Delegation>)>;
        let marks = |s: &Session| -> Marks {
            let g = stitch(s);
            let mut b = classify(&g, SourceHarness::Unknown);
            g.nodes
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let d = b.delegations.remove(&i).unwrap_or_default();
                    (x.id.clone(), (b.kind[i].clone(), d))
                })
                .collect()
        };
        let all = marks(&s);
        for k in 1..s.generations.len() {
            let mut prefix = s.clone();
            prefix.generations.truncate(k);
            for (id, mark) in marks(&prefix) {
                assert_eq!(all[&id], mark, "after {k} generations: {id}");
            }
        }
    }

    fn id_merges(g: &TurnGraph, b: &Branches) -> Vec<(String, String)> {
        b.merges
            .iter()
            .map(|&(n, p)| (g.nodes[n].id.clone(), g.nodes[p].id.clone()))
            .collect()
    }

    /// Every prefix of `s` agrees with the whole on the marks and merges
    /// it already has.
    fn assert_prefixes_agree(s: &Session) {
        let whole = stitch(s);
        let all = classify(&whole, SourceHarness::Unknown);
        let full_merges = id_merges(&whole, &all);
        let kind: HashMap<&str, &Option<BranchKind>> = whole
            .nodes
            .iter()
            .zip(&all.kind)
            .map(|(x, k)| (x.id.as_str(), k))
            .collect();
        for k in 1..s.generations.len() {
            let mut prefix = s.clone();
            prefix.generations.truncate(k);
            let g = stitch(&prefix);
            let b = classify(&g, SourceHarness::Unknown);
            for (x, mark) in g.nodes.iter().zip(&b.kind) {
                assert_eq!(kind[x.id.as_str()], mark, "after {k}: {}", x.id);
            }
            for m in id_merges(&g, &b) {
                assert!(full_merges.contains(&m), "after {k}: {m:?} moved");
            }
        }
    }

    /// Two sub-agents started with the same prompt: the first thread in
    /// feed order takes the first call.
    #[test]
    fn duplicate_delegation_prompts_match_first_thread_to_first_call() {
        let sys = |t: &str| m(json!({"role": "system", "content": t}));
        let calls = vec![agent("c1", "same"), agent("c2", "same")];
        let sub = |id: &str, start, ctx: &str, out: &str| {
            let first = m(json!({"role": "user", "content": [
                {"type": "text", "text": format!("<system-reminder>{ctx}</system-reminder>")},
                {"type": "text", "text": "same"}]}));
            generation(id, start, vec![sys("SUB"), first], text(out))
        };
        let main = generation(
            "g0",
            10,
            vec![sys("MAIN"), m(json!({"role": "user", "content": "do it"}))],
            Completion {
                tool_calls: calls,
                ..Default::default()
            },
        );
        for (x, y) in [(20, 21), (21, 20)] {
            let s = Session::new(
                "s".into(),
                None,
                vec![
                    main.clone(),
                    sub("gx", x, "x", "X done"),
                    sub("gy", y, "y", "Y done"),
                ],
            );
            let g = stitch(&s);
            let b = classify(&g, SourceHarness::Unknown);
            let call = |id: &str| {
                let gi = s.generations.iter().position(|x| x.id == id).unwrap();
                b.kind[produced_by(&g, gi)].clone()
            };
            let (first, second) = if x < y { ("gx", "gy") } else { ("gy", "gx") };
            assert_eq!(call(first), Some(BranchKind::Subagent("c1".into())));
            assert_eq!(call(second), Some(BranchKind::Subagent("c2".into())));
            assert_prefixes_agree(&s);
        }
    }

    /// A sub-agent that answers, is resumed and answers again merges where
    /// its first answer is received; the second answer never moves it.
    #[test]
    fn a_sub_agent_answering_twice_merges_at_its_first_answer() {
        let s = answering_twice();
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        let noted = produced_by(&g, 2);
        assert_eq!(b.merges, [(noted, produced_by(&g, 1))]);
        assert_prefixes_agree(&s);
    }

    /// The answer settles at the generation that shows its merge (m1
    /// returns it); the resumed sub-agent's request a2 echoes it later.
    #[test]
    fn a_merged_answer_settles_at_the_generation_that_shows_its_merge() {
        let s = answering_twice();
        let g = stitch(&s);
        let b = classify(&g, SourceHarness::Unknown);
        let answer = produced_by(&g, 1);
        assert_eq!(b.answers.get(&answer), Some(&2), "m1 carries the result");
        assert_eq!(
            g.nodes[answer].echoed_by,
            Some(3),
            "a2 resumes the sub-agent"
        );
    }

    fn answering_twice() -> Session {
        let sys = |t: &str| m(json!({"role": "system", "content": t}));
        let call = vec![agent("c1", "sub A")];
        let anchor = m(json!({"role": "user", "content": [
            {"type": "text", "text": "<system-reminder>ctx</system-reminder>"},
            {"type": "text", "text": "sub A"}]}));
        let mut main = vec![sys("MAIN"), m(json!({"role": "user", "content": "do it"}))];
        let m0 = generation(
            "m0",
            10,
            main.clone(),
            Completion {
                tool_calls: call.clone(),
                ..Default::default()
            },
        );
        let a1 = generation(
            "a1",
            20,
            vec![sys("SUB"), anchor.clone()],
            text("first answer"),
        );
        main.push(Message {
            role: "assistant".into(),
            tool_calls: call,
            ..Default::default()
        });
        main.push(m(
            json!({"role": "tool", "tool_call_id": "c1", "content": "first answer"}),
        ));
        let m1 = generation("m1", 30, main.clone(), text("noted"));
        let a2 = generation(
            "a2",
            35,
            vec![
                sys("SUB"),
                anchor,
                m(json!({"role": "assistant", "content": "first answer"})),
                m(json!({"role": "user", "content": "continue"})),
            ],
            text("second answer"),
        );
        main.push(m(json!({"role": "assistant", "content": "noted"})));
        main.push(m(
            json!({"role": "user", "content": "<task-notification>second answer</task-notification>"}),
        ));
        let m2 = generation("m2", 40, main, text("done"));
        Session::new("s".into(), None, vec![m0, a1, m1, a2, m2])
    }
}
