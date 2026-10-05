//! Session → turn DAG with chained content ids. Identical prefixes
//! collapse; divergence forks at the last shared message.

use crate::generation::{Generation, Message};
use crate::hash::{chain_id, root_id};
use crate::normalize::{canonical, completion_message, content_hash, is_dropped, normalize};
use crate::session::Session;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
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
    /// keyed by call id. An id-less call's arguments are part of its turn
    /// id, so its echo never lands here.
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
/// the same normalized call sequence, so calls pair by position.
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

/// A persistent stack, newest first: clones share their tails.
#[derive(Debug)]
struct Link<T> {
    head: T,
    tail: Stack<T>,
}

type Stack<T> = Option<Rc<Link<T>>>;

fn push<T>(stack: &mut Stack<T>, head: T) {
    let tail = stack.take();
    *stack = Some(Rc::new(Link { head, tail }));
}

fn iter<T>(stack: &Stack<T>) -> impl Iterator<Item = &T> {
    std::iter::successors(stack.as_deref(), |l| l.tail.as_deref()).map(|l| &l.head)
}

/// The walk state after some prompt messages: everything stitching the
/// next message reads besides the graph. Cheap to clone, so a trie node
/// keeps the cursor its prefix leaves behind.
#[derive(Debug, Clone)]
struct Cursor {
    /// Chain id of the last kept message.
    prev: Rc<str>,
    /// Id of the last turn message.
    prev_turn: Option<Rc<str>>,
    /// The latest turn, whose id-less calls id-less tool messages pair
    /// with, and how many of them have been taken.
    pending: Option<usize>,
    popped: usize,
    /// The prompt's dropped messages so far.
    dropped: Stack<Dropped>,
    /// Each turn so far with the ids of its calls: a call id binds to the
    /// latest turn carrying it.
    calls: Stack<(usize, Vec<String>)>,
}

impl Cursor {
    fn new(prev: &str) -> Self {
        Cursor {
            prev: prev.into(),
            prev_turn: None,
            pending: None,
            popped: 0,
            dropped: None,
            calls: None,
        }
    }

    /// The cursor just after turn `ni`, of id `id`, whose message is `m`.
    fn enter_turn(&mut self, ni: usize, id: &str, m: &Message) {
        let ids = m
            .tool_calls
            .iter()
            .filter(|c| !c.id.is_empty())
            .map(|c| c.id.clone())
            .collect();
        push(&mut self.calls, (ni, ids));
        self.pending = Some(ni);
        self.popped = 0;
        self.prev = id.into();
        self.prev_turn = Some(id.into());
    }

    /// The turn node call id `id` binds to.
    fn call(&self, id: &str) -> Option<usize> {
        iter(&self.calls)
            .find(|(_, ids)| ids.iter().any(|c| c == id))
            .map(|(ni, _)| *ni)
    }

    /// The next of the latest turn's id-less calls, as `(node, positional id)`.
    fn pop_pending(&mut self, graph: &TurnGraph) -> Option<(usize, String)> {
        let ni = self.pending?;
        let node = &graph.nodes[ni];
        let hit = node
            .message
            .tool_calls
            .iter()
            .enumerate()
            .filter(|(i, c)| c.id == positional_call_id(&node.id, *i))
            .nth(self.popped)
            .map(|(_, c)| (ni, c.id.clone()));
        self.popped += 1;
        hit
    }

    /// Stitch prompt message `mi` of generation `gi`, `m`, whose effective
    /// index is `base + mi`.
    fn step(&mut self, st: &mut Stitcher, gi: usize, mi: usize, base: usize, m: &Message) {
        let norm = normalize(m);
        if is_dropped(base + mi, &m.role) {
            let h = content_hash(&norm);
            st.graph
                .dropped_content
                .entry(h.clone())
                .or_insert((gi, norm.text.clone()));
            push(
                &mut self.dropped,
                Dropped {
                    index: mi,
                    role: m.role.clone(),
                    content_hash: h,
                },
            );
            return;
        }
        let id = chain_id(&self.prev, &canonical(&norm));
        if m.role == "tool" {
            let tcid = m.tool_call_id.as_deref().unwrap_or("");
            let hit = if tcid.is_empty() {
                self.pop_pending(&st.graph)
            } else {
                self.call(tcid).map(|ni| (ni, tcid.to_string()))
            };
            if let Some((ni, key)) = hit {
                st.graph.nodes[ni]
                    .results
                    .entry(key)
                    .or_insert(ToolOutcome {
                        content: norm.text.clone(),
                        is_error: norm.is_error,
                    });
            }
            self.prev = id.into();
            return;
        }
        let ni = match st.index.get(&id) {
            Some(&ni) => {
                let node = &mut st.graph.nodes[ni];
                if node.producer.is_some() && !node.echoed && m.role == "assistant" {
                    node.echo = echo_of(&node.message, m);
                    node.echoed = true;
                }
                ni
            }
            None => st.add_node(&id, self, m.clone(), content_hash(&norm), gi, None),
        };
        self.enter_turn(ni, &id, m);
    }
}

/// The graph under construction and its id index.
#[derive(Clone)]
struct Stitcher<'s> {
    session: &'s Session,
    root: String,
    graph: TurnGraph,
    index: HashMap<String, usize>,
    /// Generation id → its first index in the session.
    first_index: HashMap<&'s str, usize>,
}

impl<'s> Stitcher<'s> {
    fn new(session: &'s Session) -> Self {
        let mut first_index = HashMap::new();
        for (i, g) in session.generations.iter().enumerate() {
            first_index.entry(g.id.as_str()).or_insert(i);
        }
        Stitcher {
            session,
            root: root_id(&session.key),
            graph: TurnGraph {
                nodes: Vec::new(),
                links: Vec::new(),
                dropped_content: BTreeMap::new(),
                missing_continuations: Vec::new(),
            },
            index: HashMap::new(),
            first_index,
        }
    }

    /// Where generation `gi`'s chain starts, and `base`, the effective
    /// index of its first message. A Delta with a found target sits after
    /// its completion, so all its system-like messages drop.
    fn start(&mut self, gi: usize) -> (Cursor, usize) {
        match continuation_target(&self.session.generations, gi, &self.first_index) {
            Target::Found(tj) => {
                let tip = &self.graph.links[tj].completion;
                let ni = self.index[tip];
                let mut cursor = Cursor::new(tip);
                cursor.enter_turn(ni, tip, &self.graph.nodes[ni].message);
                (cursor, 1)
            }
            Target::Missing(id) => {
                if !self.graph.missing_continuations.contains(&id) {
                    self.graph.missing_continuations.push(id);
                }
                (Cursor::new(&self.root), 0)
            }
            Target::Root => (Cursor::new(&self.root), 0),
        }
    }

    /// A new turn node under `cursor`'s last turn; id-less calls get their
    /// positional ids, which never enter canonical bytes.
    fn add_node(
        &mut self,
        id: &str,
        cursor: &Cursor,
        mut message: Message,
        content_hash: String,
        gi: usize,
        producer: Option<usize>,
    ) -> usize {
        assign_positional_ids(id, &mut message);
        self.graph.nodes.push(Node {
            id: id.to_string(),
            parent: cursor.prev_turn.as_deref().map(str::to_string),
            message,
            content_hash,
            first_generation: gi,
            producer,
            results: BTreeMap::new(),
            echo: None,
            echoed: false,
        });
        let ni = self.graph.nodes.len() - 1;
        self.index.insert(id.to_string(), ni);
        ni
    }

    /// Map generation `gi`'s completion to its turn, after its prompt
    /// left `cursor`.
    fn complete(&mut self, gi: usize, cursor: Cursor) {
        let cm = completion_message(&self.session.generations[gi].completion);
        let norm = normalize(&cm);
        let id = chain_id(&cursor.prev, &canonical(&norm));
        if !self.index.contains_key(&id) {
            self.add_node(&id, &cursor, cm, content_hash(&norm), gi, Some(gi));
        }
        let mut dropped: Vec<Dropped> = iter(&cursor.dropped).cloned().collect();
        dropped.reverse();
        self.graph.links.push(GenerationLinks {
            prompt_tip: cursor.prev.to_string(),
            completion: id,
            dropped,
        });
    }
}

const NONE: usize = usize::MAX;

/// One prompt prefix: its last message, at `(generation, index)` in the
/// first prompt that reached it, and the cursor stitching the prefix
/// leaves behind.
struct TrieNode {
    at: (usize, usize),
    cursor: Cursor,
    first_child: usize,
    next_sibling: usize,
}

/// Every prompt stitched so far, as a trie of messages under its chain
/// start `(prev, base)`: a later prompt replays the longest prefix it
/// shares with any earlier one. One node per distinct prompt prefix, so it
/// is never larger than the session's prompts.
#[derive(Default)]
struct Trie {
    nodes: Vec<TrieNode>,
    roots: HashMap<(Rc<str>, usize), usize>,
}

impl Trie {
    /// The cursor after the longest stitched prefix of generation `gi`'s
    /// prompt from `start`, the trie node of that prefix, and its length.
    ///
    /// Same start and messages, same effect: everything a replayed message
    /// would add to the graph is already there. This holds because
    /// `normalize`/`is_dropped` read only `(message, base + mi)`, a
    /// cursor's start is a function of `(prev, base)`, and a node's
    /// `producer`/`message` never change after it is created.
    fn restore(
        &mut self,
        session: &Session,
        gi: usize,
        start: Cursor,
        base: usize,
    ) -> (Cursor, usize, usize) {
        let root = self.root(start, base);
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
        (self.nodes[cur].cursor.clone(), cur, depth)
    }

    fn root(&mut self, start: Cursor, base: usize) -> usize {
        let key = (start.prev.clone(), base);
        if let Some(&r) = self.roots.get(&key) {
            return r;
        }
        let r = self.nodes.len();
        self.nodes.push(TrieNode {
            at: (NONE, NONE),
            cursor: start,
            first_child: NONE,
            next_sibling: NONE,
        });
        self.roots.insert(key, r);
        r
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

    /// Add prompt message `at`, which left `cursor`, under `parent`.
    fn push(&mut self, parent: usize, at: (usize, usize), cursor: Cursor) -> usize {
        let n = self.nodes.len();
        let next_sibling = std::mem::replace(&mut self.nodes[parent].first_child, n);
        self.nodes.push(TrieNode {
            at,
            cursor,
            first_child: NONE,
            next_sibling,
        });
        n
    }
}

/// Stitch a session's generations into one turn DAG.
pub fn stitch(session: &Session) -> TurnGraph {
    stitch_impl(session, true)
}

fn stitch_impl(session: &Session, replay: bool) -> TurnGraph {
    let mut st = Stitcher::new(session);
    let mut trie = Trie::default();
    for (gi, generation) in session.generations.iter().enumerate() {
        let (start, base) = st.start(gi);
        let (mut cursor, mut cur, reused) = if replay {
            trie.restore(session, gi, start, base)
        } else {
            (start, NONE, 0)
        };
        for (mi, m) in generation.messages.iter().enumerate().skip(reused) {
            cursor.step(&mut st, gi, mi, base, m);
            if cur != NONE {
                cur = trie.push(cur, (gi, mi), cursor.clone());
            }
        }
        st.complete(gi, cursor);
    }
    let mut graph = st.graph;
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

    /// The cursor a trie node restores is the one stepping its prefix
    /// again from the same start leaves.
    #[test]
    fn a_restored_cursor_equals_stepping_the_prefix() {
        let mut replayed = 0;
        for s in fixture_sessions() {
            let mut st = Stitcher::new(&s);
            let mut trie = Trie::default();
            for (gi, generation) in s.generations.iter().enumerate() {
                let (start, base) = st.start(gi);
                let (mut cursor, mut cur, reused) = trie.restore(&s, gi, start.clone(), base);
                let mut scratch = st.clone();
                let mut stepped = start;
                for (mi, m) in generation.messages[..reused].iter().enumerate() {
                    stepped.step(&mut scratch, gi, mi, base, m);
                }
                assert_eq!(
                    format!("{cursor:?}"),
                    format!("{stepped:?}"),
                    "{} generation {gi}",
                    s.key
                );
                replayed += reused;
                for (mi, m) in generation.messages.iter().enumerate().skip(reused) {
                    cursor.step(&mut st, gi, mi, base, m);
                    cur = trie.push(cur, (gi, mi), cursor.clone());
                }
                st.complete(gi, cursor);
            }
        }
        assert!(replayed > 0);
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

    #[test]
    fn echo_pairs_calls_by_position() {
        let user = msg(json!({"role": "user", "content": "go"}));
        let g1 = generation(
            "g1",
            1,
            vec![user.clone()],
            Completion {
                text: String::new(),
                reasoning: None,
                reasoning_details: Vec::new(),
                tool_calls: vec![call("t1", "{\"a\":1}"), call("t2", "{\"b\":1}")],
            },
        );
        let g2 = generation(
            "g2",
            2,
            vec![
                user,
                msg(json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{\"a\":2}"}},
                    {"id": "t2", "type": "function", "function": {"name": "Bash", "arguments": "{\"b\":2}"}}
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
        let keys: Vec<&str> = echo.arguments.keys().map(String::as_str).collect();
        assert_eq!(keys, ["t1", "t2"]);
        assert_eq!(echo.arguments["t2"], json!("{\"b\":2}"));
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
            ["0f1c24458030794d", "7c1078e6b56b2d8e", "589a6dc5a7a75211"]
        );
        let a1 = &g.nodes[1];
        let call_ids: Vec<&str> = a1
            .message
            .tool_calls
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(call_ids, ["7c1078e6b56b2d8e:0", "7c1078e6b56b2d8e:1"]);
        assert_eq!(a1.results["7c1078e6b56b2d8e:0"].content, "A");
        assert_eq!(a1.results["7c1078e6b56b2d8e:1"].content, "B");
        assert_eq!(
            g.links[1].prompt_tip, "b664f3f0d520e4af",
            "tool chain ids are unchanged"
        );
        assert_eq!(
            positional_call_id("7c1078e6b56b2d8e", 1),
            "7c1078e6b56b2d8e:1"
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
        assert_eq!(g.nodes[1].results["7c1078e6b56b2d8e:0"].content, "A");
        assert_eq!(g.nodes[1].results["7c1078e6b56b2d8e:1"].content, "B");
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

    /// An id-less call's arguments are part of its turn id: history that
    /// echoes different arguments is another turn, and formatting alone is
    /// not a difference.
    #[test]
    fn idless_calls_fork_on_arguments_not_formatting() {
        let mut g1 = gen_(
            "g1",
            1,
            vec![msg(json!({"role": "user", "content": "go"}))],
            "",
        );
        g1.completion.tool_calls = vec![idless("read_file", "{\"path\": \"a\"}")];
        let continued = |args: &str| {
            gen_(
                "g2",
                2,
                vec![
                    msg(json!({"role": "user", "content": "go"})),
                    msg(json!({"role": "assistant", "content": null, "tool_calls": [
                        {"type": "function", "function": {"name": "read_file", "arguments": args}}]})),
                ],
                "ok",
            )
        };
        let same = stitch(&Session::new(
            "s".into(),
            None,
            vec![g1.clone(), continued("{\"path\":\"a\"}")],
        ));
        assert_eq!(same.nodes.len(), 3);
        assert!(same.nodes[1].echoed && same.nodes[1].echo.is_none());

        let other = stitch(&Session::new(
            "s".into(),
            None,
            vec![g1, continued("{\"path\":\"b\"}")],
        ));
        assert_eq!(other.nodes.len(), 4);
        assert!(!other.nodes[1].echoed);
        assert_eq!(other.nodes[2].producer, None);
        assert_eq!(other.nodes[2].parent, other.nodes[1].parent);
    }

    #[test]
    fn same_prompt_idless_completions_differing_in_arguments_are_two_turns() {
        let attempt = |id: &str, start: u64, path: &str| {
            let mut g = gen_(
                id,
                start,
                vec![msg(json!({"role": "user", "content": "go"}))],
                "",
            );
            g.completion.tool_calls =
                vec![idless("read_file", &format!("{{\"path\":\"{path}\"}}"))];
            g
        };
        let g = stitch(&Session::new(
            "s".into(),
            None,
            vec![attempt("g1", 1, "a"), attempt("g2", 2, "b")],
        ));
        assert_eq!(g.nodes.len(), 3);
        assert_ne!(g.links[0].completion, g.links[1].completion);
        for (gi, path) in [(0, "a"), (1, "b")] {
            let n = g
                .nodes
                .iter()
                .find(|n| n.id == g.links[gi].completion)
                .unwrap();
            assert_eq!(n.producer, Some(gi));
            assert_eq!(
                n.message.tool_calls[0].function.parsed_arguments(),
                json!({ "path": path })
            );
        }
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
