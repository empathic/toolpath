//! Session → turn DAG with chained content ids. Identical prefixes
//! collapse; divergence forks at the last shared message.

use crate::generation::{Generation, Message};
use crate::hash::{chain_id, root_id};
use crate::normalize::{canonical, completion_message, content_hash, is_dropped, normalize};
use crate::session::Session;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    /// The nearest earlier chain message that is a turn.
    pub parent: Option<String>,
    /// Payload source: the completion for a produced assistant turn,
    /// otherwise the first history occurrence.
    pub message: Message,
    pub content_hash: String,
    /// First generation (index into `session.generations`) containing it.
    pub first_generation: usize,
    /// Generation whose completion produced this assistant turn.
    pub producer: Option<usize>,
    /// Tool results by call id, from the first generation that carried them.
    pub results: BTreeMap<String, ToolOutcome>,
    /// How history first echoed this produced turn, when it differs.
    pub echo: Option<Echo>,
    /// A later generation's prompt has carried this produced turn, so its
    /// echo is final (its tool results may still be missing or fallbacks).
    pub echoed: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Echo {
    /// Raw echoed arguments, only for calls whose parsed arguments differ,
    /// keyed by call id, or by the positional id `"{turn_id}:{index}"` for
    /// an id-less call.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasoning_details: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Dropped {
    pub index: usize,
    pub role: String,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GenerationLinks {
    /// Chain id of the last kept prompt message (may be a tool message).
    pub prompt_tip: String,
    /// Node id of the turn this generation's completion maps to.
    pub completion: String,
    /// System-like messages removed from this generation's prompt.
    pub dropped: Vec<Dropped>,
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct TurnGraph {
    /// View order: each turn where its first generation introduced it.
    pub nodes: Vec<Node>,
    /// One per generation, in `session.generations` order.
    pub links: Vec<GenerationLinks>,
    /// Content hash → (first generation index, text) for dropped messages.
    pub dropped_content: BTreeMap<String, (usize, String)>,
    /// Continuation targets not found earlier in the session (those
    /// generations chained from the root), first-seen order, deduplicated.
    pub missing_continuations: Vec<String>,
}

/// How history echoed a produced turn, when it differs. Same turn id means
/// the same `(id, name)` call sequence, so calls pair by position.
fn echo_of(produced: &Message, echoed: &Message) -> Option<Echo> {
    let mut arguments = BTreeMap::new();
    for (p, e) in produced.tool_calls.iter().zip(&echoed.tool_calls) {
        if !p.id.is_empty() && p.function.parsed_arguments() != e.function.parsed_arguments() {
            arguments.insert(p.id.clone(), e.function.arguments.clone());
        }
    }
    let echo = Echo {
        arguments,
        reasoning_details: echoed.reasoning_details.clone(),
    };
    (!echo.arguments.is_empty() || !echo.reasoning_details.is_empty()).then_some(echo)
}

/// The stable id an id-less tool call gets once its turn id is known,
/// `index` being its position among the message's calls. Never enters
/// canonical bytes.
pub fn positional_call_id(turn_id: &str, index: usize) -> String {
    format!("{turn_id}:{index}")
}

/// Give each id-less call of a new node's message its positional id.
fn assign_positional_ids(turn_id: &str, m: &mut Message) {
    for (i, c) in m.tool_calls.iter_mut().enumerate() {
        if c.id.is_empty() {
            c.id = positional_call_id(turn_id, i);
        }
    }
}

/// `(node, positional id)` for each of a node's calls that got a positional
/// id, in call order: what id-less tool messages pair with.
fn positional_calls(graph: &TurnGraph, ni: usize) -> VecDeque<(usize, String)> {
    let node = &graph.nodes[ni];
    node.message
        .tool_calls
        .iter()
        .enumerate()
        .filter(|(i, c)| c.id == positional_call_id(&node.id, *i))
        .map(|(_, c)| (ni, c.id.clone()))
        .collect()
}

/// Where a generation's chain starts.
enum Target {
    /// `Full`, or a `Delta` with nothing to continue: the session root.
    Root,
    /// The completion node of `links[index]`.
    Found(usize),
    /// `continues` names a generation not earlier in this session.
    Missing(String),
}

/// For `Delta`: `continues` if set, else (a prompt-absent skeleton) the
/// previous generation. A target not earlier in the session is missing.
fn continuation_target(gens: &[Generation], gi: usize, ids: &HashMap<&str, usize>) -> Target {
    let g = &gens[gi];
    if !g.is_delta() {
        return Target::Root;
    }
    if let Some(c) = &g.continues {
        return match ids.get(c.as_str()) {
            Some(&j) if j < gi => Target::Found(j),
            _ => Target::Missing(c.clone()),
        };
    }
    if g.absent.prompt && gi > 0 {
        return Target::Found(gi - 1);
    }
    Target::Root
}

/// What stitching one prompt message did, so an identical prompt prefix
/// that starts from the same chain point can replay it without
/// normalizing and hashing again.
struct Replay {
    /// The chain id after the message (unchanged by a dropped one).
    prev: Rc<str>,
    /// Content hash of a dropped message.
    dropped: Option<String>,
    /// Node of a turn message.
    node: Option<usize>,
}

const NONE: usize = usize::MAX;

/// One prompt prefix: its last message, at `(generation, index)` in the
/// first prompt that reached it, what stitching that message did, and
/// what stitching the whole prefix leaves behind.
struct TrieNode {
    parent: usize,
    at: (usize, usize),
    replay: Replay,
    /// The nearest node, this one included, whose message is a turn.
    turn: usize,
    /// Id-less tool messages since `turn` (since the start without one).
    popped: usize,
    /// The nearest node, this one included, whose message was dropped.
    dropped: usize,
    first_child: usize,
    next_sibling: usize,
}

/// Every prompt stitched so far, as a trie of messages under its chain
/// start `(prev, base)`: a later prompt replays the longest prefix it
/// shares with any earlier one. One node per distinct prompt prefix, so it
/// is never larger than the session's prompts.
struct Trie {
    nodes: Vec<TrieNode>,
    roots: HashMap<(Rc<str>, usize), usize>,
}

impl Trie {
    fn root(&mut self, start: &str, base: usize) -> usize {
        let key = (Rc::from(start), base);
        if let Some(&r) = self.roots.get(&key) {
            return r;
        }
        let r = self.nodes.len();
        self.nodes.push(TrieNode {
            parent: NONE,
            at: (NONE, NONE),
            replay: Replay {
                prev: key.0.clone(),
                dropped: None,
                node: None,
            },
            turn: NONE,
            popped: 0,
            dropped: NONE,
            first_child: NONE,
            next_sibling: NONE,
        });
        self.roots.insert(key, r);
        r
    }

    /// The deepest node under `root` on `prompt`'s path, and its depth.
    fn longest(&self, session: &Session, root: usize, gi: usize) -> (usize, usize) {
        let prompt = &session.generations[gi].messages;
        let (mut cur, mut depth) = (root, 0);
        while depth < prompt.len() {
            let Some(c) = self.child(cur, |(g, m)| {
                session.generations[g].messages[m] == prompt[depth]
            }) else {
                break;
            };
            (cur, depth) = (c, depth + 1);
        }
        (cur, depth)
    }

    fn child(&self, parent: usize, mut same: impl FnMut((usize, usize)) -> bool) -> Option<usize> {
        let mut c = self.nodes[parent].first_child;
        while c != NONE {
            if same(self.nodes[c].at) {
                return Some(c);
            }
            c = self.nodes[c].next_sibling;
        }
        None
    }

    /// Add `replay`, the stitching of prompt message `at` (an id-less
    /// tool message when `idless`), under `parent`.
    fn push(&mut self, parent: usize, at: (usize, usize), replay: Replay, idless: bool) -> usize {
        let n = self.nodes.len();
        let p = &mut self.nodes[parent];
        let next_sibling = std::mem::replace(&mut p.first_child, n);
        let (turn, popped, dropped) = if replay.dropped.is_some() {
            (p.turn, p.popped, n)
        } else if replay.node.is_some() {
            (n, 0, p.dropped)
        } else {
            (p.turn, p.popped + usize::from(idless), p.dropped)
        };
        self.nodes.push(TrieNode {
            parent,
            at,
            replay,
            turn,
            popped,
            dropped,
            first_child: NONE,
            next_sibling,
        });
        n
    }

    /// The `Dropped` entries of the prefix ending at `n`, in prompt order.
    fn dropped(&self, session: &Session, mut n: usize) -> Vec<Dropped> {
        let mut out = Vec::new();
        while n != NONE {
            let node = &self.nodes[n];
            let (g, i) = node.at;
            out.push(Dropped {
                index: i,
                role: session.generations[g].messages[i].role.clone(),
                content_hash: node.replay.dropped.clone().expect("a dropped message"),
            });
            n = self.nodes[node.parent].dropped;
        }
        out.reverse();
        out
    }

    /// The turn node the prefix ending at `n` binds call id `id` to: its
    /// latest turn message carrying a call with that id.
    fn call(&self, session: &Session, n: usize, id: &str) -> Option<usize> {
        let mut t = self.nodes[n].turn;
        while t != NONE {
            let node = &self.nodes[t];
            let (g, i) = node.at;
            if session.generations[g].messages[i]
                .tool_calls
                .iter()
                .any(|c| c.id == id)
            {
                return node.replay.node;
            }
            t = self.nodes[node.parent].turn;
        }
        None
    }
}

/// Stitch a session's generations into one turn DAG.
pub fn stitch(session: &Session) -> TurnGraph {
    stitch_impl(session, true)
}

fn stitch_impl(session: &Session, replay: bool) -> TurnGraph {
    let root = root_id(&session.key);
    let mut graph = TurnGraph {
        nodes: Vec::new(),
        links: Vec::new(),
        dropped_content: BTreeMap::new(),
        missing_continuations: Vec::new(),
    };
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut first_index: HashMap<&str, usize> = HashMap::new();
    for (i, g) in session.generations.iter().enumerate() {
        first_index.entry(g.id.as_str()).or_insert(i);
    }
    let mut trie = Trie {
        nodes: Vec::new(),
        roots: HashMap::new(),
    };
    for (gi, generation) in session.generations.iter().enumerate() {
        let target = continuation_target(&session.generations, gi, &first_index);
        let tip_node = match target {
            Target::Found(tj) => Some(index[&graph.links[tj].completion]),
            _ => None,
        };
        let tip_calls: Vec<String> = tip_node.map_or_else(Vec::new, |ni| {
            graph.nodes[ni]
                .message
                .tool_calls
                .iter()
                .filter(|c| !c.id.is_empty())
                .map(|c| c.id.clone())
                .collect()
        });
        // The most recent turn's id-less calls, for id-less tool messages.
        let mut pending: VecDeque<(usize, String)> = VecDeque::new();
        // `base`: effective index of the first message. A Delta with a found
        // target sits after its completion, so all its system-like messages drop.
        let (mut prev, mut prev_turn, base) = match target {
            Target::Found(tj) => {
                let tip = graph.links[tj].completion.clone();
                pending = positional_calls(&graph, index[&tip]);
                (tip.clone(), Some(tip), 1)
            }
            Target::Missing(id) => {
                if !graph.missing_continuations.contains(&id) {
                    graph.missing_continuations.push(id);
                }
                (root.clone(), None, 0)
            }
            Target::Root => (root.clone(), None, 0),
        };

        // Same start and messages, same effect: everything a replayed
        // message would add to the graph is already there. This holds
        // because `normalize`/`is_dropped` read only `(message, base + mi)`,
        // and a node's `producer`/`message` never change after it is created.
        // What the prefix leaves behind is read off its last trie node.
        let r = if replay { trie.root(&prev, base) } else { NONE };
        let (mut cur, reused) = if replay {
            trie.longest(session, r, gi)
        } else {
            (NONE, 0)
        };
        let shared = cur;
        let mut dropped = Vec::new();
        if reused > 0 {
            let end = &trie.nodes[cur];
            if end.turn != NONE {
                let t = &trie.nodes[end.turn];
                pending = positional_calls(&graph, t.replay.node.expect("a turn"));
                prev_turn = Some(t.replay.prev.to_string());
            }
            pending.drain(..end.popped.min(pending.len()));
            prev = end.replay.prev.to_string();
            dropped = trie.dropped(session, end.dropped);
        }
        // A call id binds to its latest turn: one after the replayed prefix
        // (`calls`), else one in it, else the tip.
        let mut calls: HashMap<&str, usize> = HashMap::new();
        let call = |calls: &HashMap<&str, usize>, trie: &Trie, id: &str| {
            if let Some(&ni) = calls.get(id) {
                return Some(ni);
            }
            if shared != NONE
                && let Some(ni) = trie.call(session, shared, id)
            {
                return Some(ni);
            }
            tip_node.filter(|_| tip_calls.iter().any(|c| c == id))
        };

        for (mi, m) in generation.messages.iter().enumerate().skip(reused) {
            let norm = normalize(m);
            if is_dropped(base + mi, &m.role) {
                let h = content_hash(&norm);
                graph
                    .dropped_content
                    .entry(h.clone())
                    .or_insert((gi, norm.text.clone()));
                if cur != NONE {
                    let replay = Replay {
                        prev: prev.as_str().into(),
                        dropped: Some(h.clone()),
                        node: None,
                    };
                    cur = trie.push(cur, (gi, mi), replay, false);
                }
                dropped.push(Dropped {
                    index: mi,
                    role: m.role.clone(),
                    content_hash: h,
                });
                continue;
            }
            let id = chain_id(&prev, &canonical(&norm));
            if m.role == "tool" {
                let tcid = m.tool_call_id.as_deref().unwrap_or("");
                let hit = if tcid.is_empty() {
                    pending.pop_front()
                } else {
                    call(&calls, &trie, tcid).map(|ni| (ni, tcid.to_string()))
                };
                if let Some((ni, key)) = hit {
                    graph.nodes[ni].results.entry(key).or_insert(ToolOutcome {
                        content: norm.text.clone(),
                        is_error: norm.is_error,
                    });
                }
                if cur != NONE {
                    let replay = Replay {
                        prev: id.as_str().into(),
                        dropped: None,
                        node: None,
                    };
                    cur = trie.push(cur, (gi, mi), replay, tcid.is_empty());
                }
                prev = id;
                continue;
            }
            let ni = match index.get(&id) {
                Some(&ni) => {
                    let node = &mut graph.nodes[ni];
                    if node.producer.is_some() && !node.echoed && m.role == "assistant" {
                        node.echo = echo_of(&node.message, m);
                        node.echoed = true;
                    }
                    ni
                }
                None => {
                    let mut message = m.clone();
                    assign_positional_ids(&id, &mut message);
                    graph.nodes.push(Node {
                        id: id.clone(),
                        parent: prev_turn.clone(),
                        message,
                        content_hash: content_hash(&norm),
                        first_generation: gi,
                        producer: None,
                        results: BTreeMap::new(),
                        echo: None,
                        echoed: false,
                    });
                    index.insert(id.clone(), graph.nodes.len() - 1);
                    graph.nodes.len() - 1
                }
            };
            for c in m.tool_calls.iter().filter(|c| !c.id.is_empty()) {
                calls.insert(&c.id, ni);
            }
            // A user or system turn has no id-less calls, so it clears the queue.
            pending = positional_calls(&graph, ni);
            if cur != NONE {
                let replay = Replay {
                    prev: id.as_str().into(),
                    dropped: None,
                    node: Some(ni),
                };
                cur = trie.push(cur, (gi, mi), replay, false);
            }
            prev_turn = Some(id.clone());
            prev = id;
        }

        let mut cm = completion_message(&generation.completion);
        let norm = normalize(&cm);
        let id = chain_id(&prev, &canonical(&norm));
        if !index.contains_key(&id) {
            // After the id: positional ids never enter canonical bytes.
            assign_positional_ids(&id, &mut cm);
            graph.nodes.push(Node {
                id: id.clone(),
                parent: prev_turn,
                message: cm,
                content_hash: content_hash(&norm),
                first_generation: gi,
                producer: Some(gi),
                results: BTreeMap::new(),
                echo: None,
                echoed: false,
            });
            index.insert(id.clone(), graph.nodes.len() - 1);
        }
        graph.links.push(GenerationLinks {
            prompt_tip: prev,
            completion: id,
            dropped,
        });
    }
    add_fallback_results(&mut graph, session);
    graph
}

/// Fallback results only for calls no prompt answered.
fn add_fallback_results(graph: &mut TurnGraph, session: &Session) {
    for node in graph.nodes.iter_mut() {
        let Some(p) = node.producer else { continue };
        let fallback = &session.generations[p].tool_results;
        for c in &node.message.tool_calls {
            if node.results.contains_key(&c.id) {
                continue;
            }
            if let Some(out) = fallback.get(&c.id) {
                node.results.insert(
                    c.id.clone(),
                    ToolOutcome {
                        content: out.content.clone(),
                        is_error: out.is_error,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{Completion, FunctionCall, Generation, ToolCall, ToolOutput};
    use serde_json::json;

    /// Every session of the committed conversation fixtures, in start
    /// order and reversed.
    fn fixture_sessions() -> Vec<Session> {
        use crate::tests::otel::{decode_input, group_sessions, read_deliveries};
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel");
        let mut files: Vec<std::path::PathBuf> = ["openrouter", "equivalence"]
            .iter()
            .flat_map(|d| std::fs::read_dir(root.join(d)).unwrap())
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().is_some_and(|n| n != "expected.json"))
            .filter(|p| p.extension().is_some_and(|x| x == "ndjson" || x == "json"))
            .collect();
        files.sort();
        let mut sessions = Vec::new();
        for f in files {
            let values = decode_input(&std::fs::read(&f).unwrap(), f.to_str()).unwrap();
            let out = read_deliveries(&values, Default::default()).unwrap();
            for s in group_sessions(out.generations) {
                let mut reversed = s.clone();
                reversed.generations.reverse();
                sessions.push(s);
                sessions.push(reversed);
            }
        }
        assert!(sessions.len() > 10);
        sessions
    }

    fn plain(s: &Session) -> TurnGraph {
        stitch_impl(s, false)
    }

    /// [`super::stitch`], checked against stitching without prompt replay.
    fn stitch(s: &Session) -> TurnGraph {
        let g = super::stitch(s);
        assert_eq!(format!("{g:?}"), format!("{:?}", plain(s)));
        g
    }

    #[test]
    fn prompt_replay_matches_plain_stitching() {
        for s in fixture_sessions() {
            for k in 0..=s.generations.len() {
                let s = Session {
                    generations: s.generations[..k].to_vec(),
                    ..s.clone()
                };
                let want = format!("{:?}", plain(&s));
                let got = format!("{:?}", super::stitch(&s));
                assert_eq!(got, want, "{} prefix {k}", s.key);
            }
        }
    }

    #[test]
    fn many_interleaved_lines_match_plain_stitching() {
        // Twelve lines interleaved, each extending its own history: every
        // prompt's longest earlier match is twelve generations back.
        let mut gens = Vec::new();
        for round in 0..3u64 {
            for line in 0..12u64 {
                let mut messages = vec![msg(
                    json!({"role": "user", "content": format!("line {line}")}),
                )];
                for r in 0..round {
                    messages.push(msg(
                        json!({"role": "assistant", "content": format!("{line}.{r}")}),
                    ));
                    messages.push(msg(
                        json!({"role": "user", "content": format!("more {line}.{r}")}),
                    ));
                }
                let id = format!("g{round:02}{line:02}");
                gens.push(gen_(
                    &id,
                    round * 100 + line,
                    messages,
                    &format!("{line}.{round}"),
                ));
            }
        }
        let s = Session::new("s".into(), None, gens);
        let g = stitch(&s);
        assert_eq!(g.nodes.len(), 12 * 3 * 2);
    }

    fn msg(v: serde_json::Value) -> Message {
        serde_json::from_value(v).unwrap()
    }

    fn generation(
        id: &str,
        start: u64,
        messages: Vec<Message>,
        completion: Completion,
    ) -> Generation {
        Generation {
            id: id.into(),
            trace_id: String::new(),
            start_ns: start,
            end_ns: start + 1,
            session_id: Some("s".into()),
            request_session_id: None,
            user_id: None,
            client_key: None,
            messages,
            completion,
            usage: Default::default(),
            cost: Default::default(),
            request_model: None,
            response_model: None,
            provider: None,
            finish_reason: None,
            profile: String::new(),
            source_meta: Default::default(),
            ..Default::default()
        }
    }

    fn call(id: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: "Bash".into(),
                arguments: json!(args),
            },
        }
    }

    #[test]
    fn extension_adds_only_new_turns_and_attaches_tool_results() {
        let sys = msg(json!({"role": "system", "content": "S"}));
        let user = msg(json!({"role": "user", "content": "go"}));
        let g1 = generation(
            "g1",
            1,
            vec![sys.clone(), user.clone()],
            Completion {
                text: String::new(),
                reasoning: None,
                reasoning_details: Vec::new(),
                tool_calls: vec![call("t1", "{\"command\": \"ls\"}")],
            },
        );
        let g2 = generation(
            "g2",
            2,
            vec![
                sys,
                user,
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}]})),
                msg(json!({"role": "tool", "tool_call_id": "t1", "content": "a.txt"})),
            ],
            Completion {
                text: "done".into(),
                reasoning: None,
                reasoning_details: Vec::new(),
                tool_calls: vec![],
            },
        );
        let session = Session {
            key: "s".into(),
            session_id: Some("s".into()),
            generations: vec![g1, g2],
            truncated: false,
        };
        let g = stitch(&session);
        assert_eq!(g.nodes.len(), 4); // system, user, assistant(t1), assistant(done)
        let a1 = &g.nodes[2];
        assert_eq!(a1.producer, Some(0));
        assert_eq!(a1.results["t1"].content, "a.txt");
        assert!(a1.echo.is_none(), "spacing-only difference is not an echo");
        assert_eq!(g.nodes[3].parent.as_deref(), Some(a1.id.as_str()));
        assert_ne!(
            g.links[1].prompt_tip, a1.id,
            "prompt tip is the tool message, not a turn"
        );
    }

    #[test]
    fn ids_do_not_depend_on_later_generations() {
        let user = msg(json!({"role": "user", "content": "go"}));
        let g1 = generation(
            "g1",
            1,
            vec![user.clone()],
            Completion {
                text: "a".into(),
                ..Default::default()
            },
        );
        let g2 = generation(
            "g2",
            2,
            vec![
                user,
                msg(json!({"role": "assistant", "content": "a"})),
                msg(json!({"role": "user", "content": "more"})),
            ],
            Completion {
                text: "b".into(),
                ..Default::default()
            },
        );
        let one = Session {
            key: "s".into(),
            session_id: None,
            generations: vec![g1.clone()],
            truncated: false,
        };
        let two = Session {
            key: "s".into(),
            session_id: None,
            generations: vec![g1, g2],
            truncated: false,
        };
        let (a, b) = (stitch(&one), stitch(&two));
        assert_eq!(
            a.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
            b.nodes[..a.nodes.len()]
                .iter()
                .map(|n| &n.id)
                .collect::<Vec<_>>()
        );
    }

    fn anon_call(args: &str) -> ToolCall {
        ToolCall {
            id: String::new(),
            function: FunctionCall {
                name: "Bash".into(),
                arguments: json!(args),
            },
        }
    }

    #[test]
    fn echo_pairs_calls_by_position_and_never_keys_on_empty() {
        let user = msg(json!({"role": "user", "content": "go"}));
        let g1 = generation(
            "g1",
            1,
            vec![user.clone()],
            Completion {
                text: String::new(),
                reasoning: None,
                reasoning_details: Vec::new(),
                tool_calls: vec![anon_call("{\"a\":1}"), anon_call("{\"b\":1}")],
            },
        );
        let g2 = generation(
            "g2",
            2,
            vec![
                user,
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"type": "function", "function": {"name": "Bash", "arguments": "{\"a\":2}"}},
                    {"type": "function", "function": {"name": "Bash", "arguments": "{\"b\":2}"}}
                ]})),
            ],
            Completion {
                text: "done".into(),
                ..Default::default()
            },
        );
        let s = Session::new("s".into(), None, vec![g1, g2]);
        let g = stitch(&s);
        let produced = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
        let echo = produced.echo.as_ref().expect("both arguments differ");
        let keys: Vec<&String> = echo.arguments.keys().collect();
        let want = [format!("{}:0", produced.id), format!("{}:1", produced.id)];
        assert_eq!(keys, want.iter().collect::<Vec<_>>());
        assert_eq!(echo.arguments[&want[1]], json!("{\"b\":2}"));
    }

    #[test]
    fn is_error_reaches_the_tool_outcome() {
        let user = msg(json!({"role": "user", "content": "go"}));
        let g1 = generation(
            "g1",
            1,
            vec![user.clone()],
            Completion {
                text: String::new(),
                reasoning: None,
                reasoning_details: Vec::new(),
                tool_calls: vec![call("t1", "{}"), call("t2", "{}")],
            },
        );
        let g2 = generation(
            "g2",
            2,
            vec![
                user,
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
                    {"id": "t2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
                ]})),
                msg(
                    json!({"role": "tool", "tool_call_id": "t1", "content": "boom", "is_error": true}),
                ),
                msg(json!({"role": "tool", "tool_call_id": "t2", "content": "fine"})),
            ],
            Completion {
                text: "done".into(),
                ..Default::default()
            },
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let results = &g
            .nodes
            .iter()
            .find(|n| n.producer == Some(0))
            .unwrap()
            .results;
        assert!(results["t1"].is_error);
        assert_eq!(results["t1"].content, "boom");
        assert!(!results["t2"].is_error);
    }

    fn gen_(id: &str, start: u64, messages: Vec<Message>, text: &str) -> Generation {
        Generation {
            id: id.into(),
            start_ns: start,
            end_ns: start + 1,
            messages,
            completion: Completion {
                text: text.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn delta(mut g: Generation, continues: Option<&str>) -> Generation {
        g.history = crate::generation::History::Delta;
        g.continues = continues.map(str::to_string);
        g
    }

    fn ids(g: &TurnGraph) -> Vec<&str> {
        g.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    // Known answers below are computed independently with python3 hashlib/json.

    #[test]
    fn delta_chains_from_the_continued_completion() {
        let s = Session::new(
            "s-delta".into(),
            None,
            vec![
                gen_(
                    "g1",
                    1,
                    vec![msg(json!({"role": "user", "content": "hi"}))],
                    "hello",
                ),
                delta(
                    gen_(
                        "g2",
                        2,
                        vec![msg(json!({"role": "user", "content": "more"}))],
                        "done",
                    ),
                    Some("g1"),
                ),
            ],
        );
        let g = stitch(&s);
        assert_eq!(
            ids(&g),
            [
                "a3014ed69576c2c1",
                "070372b35a8db102",
                "b0ffb16ffae17e57",
                "894d354f1d50f0d9"
            ]
        );
        assert_eq!(g.nodes[2].parent.as_deref(), Some("070372b35a8db102"));
        assert_eq!(g.links[1].completion, "894d354f1d50f0d9");
        assert!(g.missing_continuations.is_empty());
    }

    #[test]
    fn delta_ids_equal_a_full_restatement() {
        let full = Session::new(
            "s-delta".into(),
            None,
            vec![
                gen_(
                    "g1",
                    1,
                    vec![msg(json!({"role": "user", "content": "hi"}))],
                    "hello",
                ),
                gen_(
                    "g2",
                    2,
                    vec![
                        msg(json!({"role": "user", "content": "hi"})),
                        msg(json!({"role": "assistant", "content": "hello"})),
                        msg(json!({"role": "user", "content": "more"})),
                    ],
                    "done",
                ),
            ],
        );
        assert_eq!(
            ids(&stitch(&full)),
            [
                "a3014ed69576c2c1",
                "070372b35a8db102",
                "b0ffb16ffae17e57",
                "894d354f1d50f0d9"
            ]
        );
    }

    #[test]
    fn delta_drops_repeated_instructions_by_effective_index() {
        let sys = || msg(json!({"role": "system", "content": "I"}));
        let s = Session::new(
            "s-sys".into(),
            None,
            vec![
                gen_(
                    "g1",
                    1,
                    vec![sys(), msg(json!({"role": "user", "content": "hi"}))],
                    "hello",
                ),
                delta(
                    gen_(
                        "g2",
                        2,
                        vec![sys(), msg(json!({"role": "user", "content": "more"}))],
                        "done",
                    ),
                    Some("g1"),
                ),
            ],
        );
        let g = stitch(&s);
        assert_eq!(
            ids(&g),
            [
                "0947878942b92346",
                "7f4d4a2e621c1ae6",
                "aec1f86abcbb4368",
                "f04583f2623f8311",
                "e2a5b3fa95a267c1"
            ]
        );
        assert!(
            g.links[0].dropped.is_empty(),
            "index 0 of a Full prompt is kept"
        );
        assert_eq!(
            g.links[1].dropped,
            vec![Dropped {
                index: 0,
                role: "system".into(),
                content_hash: "5864d6afe8b6ec0d2c5b83c2ea57036adf799378910af0f9cc3d5d09e1b11df0"
                    .into(),
            }]
        );
    }

    #[test]
    fn missing_target_chains_from_root_and_is_recorded() {
        let s = Session::new(
            "s-miss".into(),
            None,
            vec![delta(
                gen_(
                    "g2",
                    2,
                    vec![msg(json!({"role": "user", "content": "more"}))],
                    "done",
                ),
                Some("gen-missing"),
            )],
        );
        let g = stitch(&s);
        assert_eq!(ids(&g), ["ef64d15e72c178a2", "7533a25326a5ab97"]);
        assert_eq!(g.nodes[0].parent, None);
        assert_eq!(g.missing_continuations, vec!["gen-missing".to_string()]);
    }

    #[test]
    fn continuation_to_a_skipped_generation_is_missing() {
        // The target was skipped (Truncated / ErrorStatus): in the batch, not
        // the session.
        let s = Session::new(
            "s-miss".into(),
            None,
            vec![
                gen_(
                    "other",
                    1,
                    vec![msg(json!({"role": "user", "content": "unrelated"}))],
                    "x",
                ),
                delta(
                    gen_(
                        "g2",
                        2,
                        vec![msg(json!({"role": "user", "content": "more"}))],
                        "done",
                    ),
                    Some("skipped"),
                ),
            ],
        );
        let g = stitch(&s);
        let u = g.nodes.iter().find(|n| n.first_generation == 1).unwrap();
        assert_eq!(u.parent, None);
        assert_eq!(u.id, "ef64d15e72c178a2");
        assert_eq!(g.missing_continuations, vec!["skipped".to_string()]);
    }

    #[test]
    fn a_target_sorting_after_is_missing() {
        let s = Session::new(
            "s".into(),
            None,
            vec![
                delta(
                    gen_(
                        "g1",
                        1,
                        vec![msg(json!({"role": "user", "content": "a"}))],
                        "b",
                    ),
                    Some("g2"),
                ),
                gen_(
                    "g2",
                    2,
                    vec![msg(json!({"role": "user", "content": "c"}))],
                    "d",
                ),
            ],
        );
        assert_eq!(stitch(&s).missing_continuations, vec!["g2".to_string()]);
    }

    #[test]
    fn missing_continuations_are_deduplicated_in_first_seen_order() {
        let s = Session::new(
            "s".into(),
            None,
            vec![
                delta(
                    gen_(
                        "g1",
                        1,
                        vec![msg(json!({"role": "user", "content": "a"}))],
                        "b",
                    ),
                    Some("x"),
                ),
                delta(
                    gen_(
                        "g2",
                        2,
                        vec![msg(json!({"role": "user", "content": "c"}))],
                        "d",
                    ),
                    Some("y"),
                ),
                delta(
                    gen_(
                        "g3",
                        3,
                        vec![msg(json!({"role": "user", "content": "e"}))],
                        "f",
                    ),
                    Some("x"),
                ),
            ],
        );
        assert_eq!(
            stitch(&s).missing_continuations,
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn delta_pairs_results_with_the_targets_calls() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![call("c1", "{}")];
        let g2 = delta(
            gen_(
                "g2",
                2,
                vec![msg(
                    json!({"role": "tool", "tool_call_id": "c1", "content": "ok"}),
                )],
                "done",
            ),
            Some("g1"),
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let a1 = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
        assert_eq!(a1.results["c1"].content, "ok");
    }

    #[test]
    fn prompt_absent_skeleton_chains_from_the_previous_generation() {
        let mut sk = gen_("g2", 2, vec![], "");
        sk.absent.prompt = true;
        sk.absent.completion = true;
        let mut sk2 = gen_("g3", 3, vec![], "");
        sk2.absent = sk.absent;
        let s = Session::new(
            "s-delta".into(),
            None,
            vec![
                gen_(
                    "g1",
                    1,
                    vec![msg(json!({"role": "user", "content": "hi"}))],
                    "hello",
                ),
                sk,
                sk2,
            ],
        );
        let g = stitch(&s);
        // chain("070372b35a8db102", {"role":"assistant","text":""}) and again.
        assert_eq!(g.links[1].completion, "254449c86d82e18e");
        assert_eq!(
            g.links[2].completion, "44f714e4a24869b2",
            "consecutive skeletons never collapse"
        );
        assert!(g.missing_continuations.is_empty());
    }

    #[test]
    fn skeleton_inside_a_full_session_is_a_documented_dead_end() {
        let mut sk = gen_("g2", 2, vec![], "");
        sk.absent = crate::generation::Absent {
            prompt: true,
            completion: true,
        };
        let full = gen_(
            "g3",
            3,
            vec![
                msg(json!({"role": "user", "content": "hi"})),
                msg(json!({"role": "assistant", "content": "hello"})),
                msg(json!({"role": "user", "content": "more"})),
            ],
            "done",
        );
        let s = Session::new(
            "s-delta".into(),
            None,
            vec![
                gen_(
                    "g1",
                    1,
                    vec![msg(json!({"role": "user", "content": "hi"}))],
                    "hello",
                ),
                sk,
                full,
            ],
        );
        let g = stitch(&s);
        let skeleton = g.nodes.iter().find(|n| n.id == "254449c86d82e18e").unwrap();
        assert_eq!(skeleton.parent.as_deref(), Some("070372b35a8db102"));
        let u2 = g.nodes.iter().find(|n| n.id == "b0ffb16ffae17e57").unwrap();
        assert_eq!(
            u2.parent.as_deref(),
            Some("070372b35a8db102"),
            "the Full history skips the skeleton"
        );
        assert!(
            g.nodes
                .iter()
                .all(|n| n.parent.as_deref() != Some("254449c86d82e18e"))
        );
    }

    fn idless(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: String::new(),
            function: FunctionCall {
                name: name.into(),
                arguments: json!(args),
            },
        }
    }

    fn positional_session(extra_tool: bool) -> Session {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![
            idless("read_file", "{\"path\":\"a\"}"),
            idless("read_file", "{\"path\":\"b\"}"),
        ];
        let mut prompt = vec![
            msg(json!({"role": "user", "content": "go"})),
            msg(json!({"role": "assistant", "content": null, "tool_calls": [
                {"type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}},
                {"type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"b\"}"}}]})),
            msg(json!({"role": "tool", "tool_call_id": "", "content": "A"})),
            msg(json!({"role": "tool", "content": "B"})),
        ];
        if extra_tool {
            prompt.push(msg(json!({"role": "tool", "content": "C"})));
        }
        Session::new(
            "s-pos".into(),
            None,
            vec![g1, gen_("g2", 2, prompt, "both read")],
        )
    }

    #[test]
    fn idless_calls_get_positional_ids_and_pair_by_position() {
        let g = stitch(&positional_session(false));
        assert_eq!(
            ids(&g),
            ["0f1c24458030794d", "cdb1b25b32c0f53b", "44976e496c0c84f4"]
        );
        let a1 = &g.nodes[1];
        let call_ids: Vec<&str> = a1
            .message
            .tool_calls
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(call_ids, ["cdb1b25b32c0f53b:0", "cdb1b25b32c0f53b:1"]);
        assert_eq!(a1.results["cdb1b25b32c0f53b:0"].content, "A");
        assert_eq!(a1.results["cdb1b25b32c0f53b:1"].content, "B");
        assert_eq!(
            g.links[1].prompt_tip, "989851ecf6fe8eee",
            "tool chain ids are unchanged"
        );
        assert_eq!(
            positional_call_id("cdb1b25b32c0f53b", 1),
            "cdb1b25b32c0f53b:1"
        );
    }

    #[test]
    fn positional_ids_are_stable_across_derives() {
        let one = Session::new(
            "s-pos".into(),
            None,
            positional_session(false).generations[..1].to_vec(),
        );
        let a = stitch(&one);
        let b = stitch(&positional_session(false));
        assert_eq!(a.nodes[1].message.tool_calls, b.nodes[1].message.tool_calls);
    }

    #[test]
    fn a_replayed_prompt_prefix_keeps_the_idless_queue() {
        let mut s = positional_session(false);
        let mut short = s.generations[1].clone();
        short.id = "g2-short".into();
        short.messages.pop();
        s.generations.insert(1, short);
        let g = stitch(&s);
        assert_eq!(g.nodes[1].results["cdb1b25b32c0f53b:0"].content, "A");
        assert_eq!(g.nodes[1].results["cdb1b25b32c0f53b:1"].content, "B");
    }

    #[test]
    fn surplus_idless_results_attach_to_nothing() {
        let g = stitch(&positional_session(true));
        assert_eq!(g.nodes[1].results.len(), 2);
    }

    #[test]
    fn idless_results_do_not_cross_an_intervening_turn() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![idless("read_file", "{}")];
        let g2 = gen_(
            "g2",
            2,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"type": "function", "function": {"name": "read_file", "arguments": "{}"}}]})),
                msg(json!({"role": "user", "content": "interrupt"})),
                msg(json!({"role": "tool", "content": "late"})),
            ],
            "ok",
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        assert!(g.nodes.iter().all(|n| n.results.is_empty()));
    }

    #[test]
    fn mixed_ids_keep_real_ids_and_index_among_all_calls() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![call("c1", "{}"), idless("Read", "{}")];
        let g2 = gen_(
            "g2",
            2,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
                    {"type": "function", "function": {"name": "Read", "arguments": "{}"}}]})),
                msg(json!({"role": "tool", "tool_call_id": "c1", "content": "by id"})),
                msg(json!({"role": "tool", "content": "by position"})),
            ],
            "ok",
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let a = &g.nodes[1];
        let pos = positional_call_id(&a.id, 1);
        assert_eq!(a.message.tool_calls[0].id, "c1");
        assert_eq!(a.message.tool_calls[1].id, pos);
        assert_eq!(a.results["c1"].content, "by id");
        assert_eq!(a.results[&pos].content, "by position");
    }

    #[test]
    fn repeated_call_ids_pair_with_the_most_recent_turn() {
        // google-genai synthesizes "<name>_<part index>", so two turns can both
        // carry "read_file_0".
        let turn = |text: &str| {
            msg(json!({"role": "assistant", "content": text, "tool_calls": [
                {"id": "read_file_0", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}]}))
        };
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "first",
        );
        g1.completion.tool_calls = vec![call("read_file_0", "{}")];
        let mut g2 = gen_(
            "g2",
            2,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                turn("first"),
                msg(json!({"role": "tool", "tool_call_id": "read_file_0", "content": "one"})),
            ],
            "second",
        );
        g2.completion.tool_calls = vec![call("read_file_0", "{}")];
        let g3 = gen_(
            "g3",
            3,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                turn("first"),
                msg(json!({"role": "tool", "tool_call_id": "read_file_0", "content": "one"})),
                turn("second"),
                msg(json!({"role": "tool", "tool_call_id": "read_file_0", "content": "two"})),
            ],
            "done",
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2, g3]));
        let first = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
        let second = g.nodes.iter().find(|n| n.producer == Some(1)).unwrap();
        assert_eq!(first.results["read_file_0"].content, "one");
        assert_eq!(second.results["read_file_0"].content, "two");
    }

    #[test]
    fn results_after_a_replayed_prefix_find_calls_inside_it() {
        let user = msg(json!({"role": "user", "content": "go"}));
        let env = msg(json!({"role": "system", "content": "env"}));
        let asst = msg(json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
            {"id": "t2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}]}));
        let t1 = msg(json!({"role": "tool", "tool_call_id": "t1", "content": "one"}));
        let t2 = msg(json!({"role": "tool", "tool_call_id": "t2", "content": "two"}));
        let zz = msg(json!({"role": "tool", "tool_call_id": "zz", "content": "?"}));
        let mut g1 = gen_("g1", 1, vec![user.clone()], "");
        g1.completion.tool_calls = vec![call("t1", "{}"), call("t2", "{}")];
        let g2 = gen_(
            "g2",
            2,
            vec![user.clone(), asst.clone(), env.clone(), t1.clone()],
            "a",
        );
        let g3 = gen_("g3", 3, vec![user, asst, env, t1, zz, t2], "b");
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2, g3]));
        let produced = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
        assert_eq!(produced.results["t1"].content, "one");
        assert_eq!(produced.results["t2"].content, "two");
        assert!(g.nodes.iter().all(|n| !n.results.contains_key("zz")));
        let dropped: Vec<usize> = g.links[2].dropped.iter().map(|d| d.index).collect();
        assert_eq!(dropped, vec![2]);
    }

    #[test]
    fn echo_keys_idless_calls_by_positional_id() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![idless("read_file", "{\"path\": \"a\"}")];
        let g2 = gen_(
            "g2",
            2,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"b\"}"}}]})),
            ],
            "ok",
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let a = &g.nodes[1];
        let echo = a.echo.as_ref().expect("arguments differ semantically");
        assert_eq!(
            echo.arguments.keys().collect::<Vec<_>>(),
            [&positional_call_id(&a.id, 0)]
        );
    }

    fn fallback(content: &str) -> ToolOutput {
        ToolOutput {
            content: content.into(),
            is_error: true,
        }
    }

    #[test]
    fn fallback_results_fill_calls_no_prompt_answers() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![call("c1", "{}")];
        g1.tool_results.insert("c1".into(), fallback("fb"));
        let g = stitch(&Session::new("s".into(), None, vec![g1]));
        let r = &g.nodes[1].results["c1"];
        assert_eq!((r.content.as_str(), r.is_error), ("fb", true));
    }

    #[test]
    fn prompt_carried_results_win_per_call() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![call("c1", "{}"), call("c2", "{}")];
        g1.tool_results.insert("c1".into(), fallback("fb1"));
        g1.tool_results.insert("c2".into(), fallback("fb2"));
        let g2 = gen_(
            "g2",
            2,
            vec![
                msg(json!({"role": "user", "content": "go"})),
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
                    {"id": "c2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}]})),
                msg(json!({"role": "tool", "tool_call_id": "c1", "content": "real"})),
            ],
            "ok",
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let a = &g.nodes[1];
        assert_eq!(a.results["c1"].content.as_str(), "real");
        assert_eq!(a.results["c2"].content.as_str(), "fb2");
        assert!(!a.results["c1"].is_error);
    }

    #[test]
    fn delta_pairs_idless_results_with_the_targets_positional_calls() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![idless("read_file", "{}")];
        let g2 = delta(
            gen_(
                "g2",
                2,
                vec![msg(json!({"role": "tool", "content": "ok"}))],
                "done",
            ),
            Some("g1"),
        );
        let g = stitch(&Session::new("s".into(), None, vec![g1, g2]));
        let a1 = g.nodes.iter().find(|n| n.producer == Some(0)).unwrap();
        let pos = positional_call_id(&a1.id, 0);
        assert_eq!(a1.message.tool_calls[0].id, pos);
        assert_eq!(a1.results[&pos].content, "ok");
    }

    #[test]
    fn user_turn_between_a_call_and_its_results() {
        // Id-bearing results still pair through `calls`; id-less ones meet a
        // queue the user turn cleared, so they attach to nothing.
        let shape = |call_id: &str| {
            let tool_call = if call_id.is_empty() {
                json!({"type": "function", "function": {"name": "Bash", "arguments": "{}"}})
            } else {
                json!({"id": call_id, "type": "function", "function": {"name": "Bash", "arguments": "{}"}})
            };
            let mut g1 = gen_(
                "g1",
                1,
                vec![msg(json!({"role": "user", "content": "go"}))],
                "",
            );
            g1.completion.tool_calls = vec![call(call_id, "{}")];
            let g2 = gen_(
                "g2",
                2,
                vec![
                    msg(json!({"role": "user", "content": "go"})),
                    msg(json!({"role": "assistant", "content": null, "tool_calls": [tool_call]})),
                    msg(json!({"role": "user", "content": "note"})),
                    msg(json!({"role": "tool", "tool_call_id": call_id, "content": "r"})),
                ],
                "ok",
            );
            stitch(&Session::new("s".into(), None, vec![g1, g2]))
        };
        let with_id = shape("c1");
        assert_eq!(with_id.nodes[1].results["c1"].content, "r");
        let idless = shape("");
        assert!(idless.nodes[1].message.tool_calls[0].id.ends_with(":0"));
        assert!(idless.nodes.iter().all(|n| n.results.is_empty()));
    }
}
