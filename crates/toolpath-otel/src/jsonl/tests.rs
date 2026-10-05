use super::*;
use crate::SkipCounts;
use crate::derive::{canonical_step_json, derive_session};
use crate::read_session;
use crate::record::GenerationBatch;
use crate::tests::common::{CONVERSATIONS, REAL, deliveries};
use crate::tests::otel::{classified, classifier};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use toolpath::v1::jsonl::JsonlLine;

mod regressions;

/// At most `max_steps` steps per body (`0`: no limit).
fn limits(max_steps: usize) -> BatchLimits {
    BatchLimits::new(None, (max_steps > 0).then_some(max_steps))
}

/// Each body's lines, parsed back from its text, checked against the
/// body's own `step_ids` and `head`.
fn lines(bodies: Vec<Body>) -> Vec<Vec<JsonlLine>> {
    bodies
        .into_iter()
        .map(|b| {
            let lines: Vec<JsonlLine> = b
                .text
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(step_ids(&lines), b.step_ids);
            assert_eq!(head_of(&lines).as_ref(), Some(&b.head));
            lines
        })
        .collect()
}

fn send(
    s: &Session,
    config: &toolpath_convo::DeriveConfig,
    remote: &Remote,
    final_: bool,
    max_steps: usize,
) -> Result<Vec<Vec<JsonlLine>>> {
    super::send(
        s,
        config,
        Some(&classifier()),
        remote,
        final_,
        limits(max_steps),
    )
    .map(lines)
}

fn derive_jsonl<'m>(
    records: &[GenerationRecord],
    messages: impl Fn(&MessageHash) -> Option<&'m StoredMessage>,
    config: &DeriveConfig,
    remote: &Remote,
    final_: bool,
    max_steps: usize,
) -> Result<Derived<Vec<Vec<JsonlLine>>>> {
    let settle = if final_ {
        Settle::Final
    } else {
        Settle::Settled
    };
    let d = super::derive_jsonl(records, messages, config, remote, settle, limits(max_steps))?;
    Ok(Derived {
        output: lines(d.output),
        skipped: d.skipped,
    })
}

fn convo() -> toolpath_convo::DeriveConfig {
    toolpath_convo::DeriveConfig::default()
}

fn derive_path(s: &Session) -> Path {
    derive_session(s, &convo(), Some(&classifier()))
}

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

fn session(gens: Vec<Generation>) -> Session {
    Session::new("s".into(), Some("s".into()), gens)
}

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn assistant(text: &str) -> Value {
    json!({"role": "assistant", "content": text})
}

fn system(text: &str) -> Value {
    json!({"role": "system", "content": text})
}

/// `generation` with usage and cost of its own, so its record is
/// recognisable. Tenths of a dollar sum differently in different orders.
fn billed(id: &str, start: u64, messages: Value, text: &str) -> Generation {
    let mut g = generation(id, start, messages, text);
    g.end_ns = start + 10;
    g.usage.input_tokens = Some(start * 100 + 7);
    g.usage.output_tokens = Some(3);
    g.cost.total = Some(start as f64 / 10.0);
    g
}

/// The latest generation's completion: `derive_path` makes it `head`.
fn completion_id(s: &Session) -> String {
    derive_path(s).path.head
}

fn value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn step_ids(lines: &[JsonlLine]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| match l {
            JsonlLine::Step(s) => Some(s.0.step.id.clone()),
            _ => None,
        })
        .collect()
}

fn sent_step(lines: &[JsonlLine], id: &str) -> Option<Value> {
    lines.iter().find_map(|l| match l {
        JsonlLine::Step(s) if s.0.step.id == id => Some(value(&s.0)),
        _ => None,
    })
}

fn path_step(p: &Path, id: &str) -> Value {
    value(p.steps.iter().find(|s| s.step.id == id).unwrap())
}

/// `tool_uses[0].result.content` of the sent step with this id.
fn first_result(lines: &[JsonlLine], id: &str) -> Value {
    let step = sent_step(lines, id).expect("step sent");
    let convo = step["change"]
        .as_object()
        .unwrap()
        .values()
        .find(|c| c["structural"]["type"] == "conversation.append")
        .expect("conversation change")
        .clone();
    convo["structural"]["tool_uses"][0]["result"]["content"].clone()
}

/// The last `Head` line's step.
fn head_of(lines: &[JsonlLine]) -> Option<String> {
    lines.iter().rev().find_map(|l| match l {
        JsonlLine::Head(h) => Some(h.step_id.clone()),
        _ => None,
    })
}

/// What the caller passes as `Remote::stored`, read from the server.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hint {
    /// Every stored id.
    Exact,
    /// None: every settled step goes out again.
    Empty,
    /// Every other stored id, by id.
    Alternate,
}

const HINTS: [Hint; 3] = [Hint::Exact, Hint::Empty, Hint::Alternate];

/// A Pathbase #486 path: the steps it holds and the rules it applies to
/// each appended body, all or nothing.
#[derive(Debug, Default, Clone)]
struct Server {
    opened: bool,
    steps: HashMap<String, Value>,
    /// The accepted lines, without resent steps (stored once, as #486 does).
    text: String,
    head: Option<String>,
    /// Step ids of the frozen path this one continues: parents only.
    base: HashSet<String>,
    /// That path's feed order: the `fed` of the first send.
    base_fed: Vec<String>,
}

impl Server {
    /// #486's checks, plus ours: parents before children (#486 drops the
    /// edge silently), every body ends with a `Head`, the meta line first.
    fn apply(&mut self, body: &[JsonlLine]) -> std::result::Result<(), String> {
        let mut next = self.clone();
        for (i, line) in body.iter().enumerate() {
            let mut keep = true;
            match line {
                JsonlLine::PathOpen(open) => {
                    if next.opened || i != 0 {
                        return Err(format!("400 bad_request: PathOpen at line {i}"));
                    }
                    let meta = value(&open.meta);
                    assert!(meta.get("actors").is_none() && meta.get("signatures").is_none());
                    next.opened = true;
                }
                _ if !next.opened => return Err("400 bad_request: open needs a PathOpen".into()),
                JsonlLine::Step(st) => {
                    let id = st.0.step.id.clone();
                    if next.base.contains(&id) {
                        return Err(format!("400: frozen step {id} sent again"));
                    }
                    let payload = value(&st.0);
                    match next.steps.get(&id) {
                        Some(p) if *p != payload => {
                            return Err(format!("400 invalid_document: {id} changed"));
                        }
                        Some(_) => keep = false,
                        None => {
                            for p in &st.0.step.parents {
                                assert!(
                                    next.steps.contains_key(p) || next.base.contains(p),
                                    "{id} before its parent {p}"
                                );
                            }
                            next.steps.insert(id, payload);
                        }
                    }
                }
                JsonlLine::Head(h) => {
                    if !next.steps.contains_key(&h.step_id) {
                        return Err(format!("400 bad_request: Head {} not stored", h.step_id));
                    }
                    next.head = Some(h.step_id.clone());
                }
                JsonlLine::PathMeta(m) => {
                    assert_eq!(i, 0, "PathMeta is the first line");
                    let patch = value(&m.patch);
                    assert!(patch.get("actors").is_none() && patch.get("signatures").is_none());
                }
                JsonlLine::Signature(sig) => {
                    let target = sig.target.strip_prefix("step:").expect("step signature");
                    assert!(next.steps.contains_key(target));
                }
                JsonlLine::ActorDef(_) => {}
                JsonlLine::PathClose(_) => panic!("PathClose sent"),
            }
            if keep {
                next.text += &serde_json::to_string(line).unwrap();
                next.text.push('\n');
            }
        }
        assert!(
            matches!(body.last(), Some(JsonlLine::Head(_))),
            "a body ends with a Head"
        );
        *self = next;
        Ok(())
    }

    fn path(&self) -> Path {
        Path::from_jsonl_str(&self.text).unwrap()
    }

    /// What a stateless caller reads back: whether the path exists, the
    /// feed order from its meta, and (some of) its step ids.
    fn remote(&self, hint: Hint) -> Remote {
        let mut ids: Vec<String> = self.steps.keys().cloned().collect();
        ids.sort();
        let stored = match hint {
            Hint::Exact => ids.into_iter().collect(),
            Hint::Empty => HashSet::new(),
            Hint::Alternate => ids.into_iter().step_by(2).collect(),
        };
        let meta = |key: &str| {
            let path = self.path();
            path.meta
                .and_then(|m| m.extra.get("otel").map(|o| o[key].clone()))
        };
        Remote {
            opened: self.opened,
            fed: if self.opened {
                fed_of(&self.path())
            } else {
                self.base_fed.clone()
            },
            stored,
            base: self.base.clone(),
            harness: if self.opened {
                meta("harness").and_then(|h| h.as_str().map(str::to_string))
            } else {
                None
            },
        }
    }
}

#[derive(Debug)]
enum Failure {
    Derive(OtelError),
    Rejected(String),
}

/// A stateless caller in front of a [`Server`].
struct Reader {
    server: Server,
    hint: Hint,
    max_steps: usize,
    /// The last public-API call's skip counts.
    last_skipped: SkipCounts,
}

impl Default for Reader {
    fn default() -> Self {
        Reader::new(Hint::Exact, 0)
    }
}

impl Reader {
    fn new(hint: Hint, max_steps: usize) -> Self {
        Reader {
            server: Server::default(),
            hint,
            max_steps,
            last_skipped: SkipCounts::default(),
        }
    }

    fn apply(
        &mut self,
        bodies: Vec<Vec<JsonlLine>>,
    ) -> std::result::Result<Vec<JsonlLine>, Failure> {
        let mut all = Vec::new();
        for body in bodies {
            if self.max_steps > 0 {
                assert!(
                    step_ids(&body).len() <= self.max_steps,
                    "body over max_steps"
                );
            }
            self.server.apply(&body).map_err(Failure::Rejected)?;
            all.extend(body);
        }
        Ok(all)
    }

    fn try_send(
        &mut self,
        s: &Session,
        final_: bool,
    ) -> std::result::Result<Vec<JsonlLine>, Failure> {
        let remote = self.server.remote(self.hint);
        let bodies = send(s, &convo(), &remote, final_, self.max_steps).map_err(Failure::Derive)?;
        self.apply(bodies)
    }

    fn send(&mut self, s: &Session, final_: bool) -> Vec<JsonlLine> {
        self.try_send(s, final_).unwrap()
    }

    /// Through the public API, from request bodies read one delivery at a
    /// time and round-tripped through JSON.
    fn send_requests(&mut self, requests: &[Value], final_: bool) -> Vec<JsonlLine> {
        let batch = stored(requests);
        match self.try_send_records(&batch.records, &batch.messages, final_) {
            Ok(lines) => lines,
            Err(OtelError::NoGenerations { .. }) => Vec::new(),
            Err(e) => panic!("{e}"),
        }
    }

    /// Through the public API, from records in the order the store
    /// received them.
    fn try_send_records(
        &mut self,
        records: &[GenerationRecord],
        messages: &BTreeMap<MessageHash, StoredMessage>,
        final_: bool,
    ) -> Result<Vec<JsonlLine>> {
        let remote = self.server.remote(self.hint);
        let d = derive_jsonl(
            records,
            |h| messages.get(h),
            &classified(),
            &remote,
            final_,
            self.max_steps,
        )?;
        self.last_skipped = d.skipped;
        Ok(self.apply(d.output).unwrap())
    }

    fn stored(&self) -> HashSet<String> {
        self.server.steps.keys().cloned().collect()
    }

    fn fed(&self) -> Vec<String> {
        self.server.remote(Hint::Empty).fed
    }

    fn path(&self) -> Path {
        self.server.path()
    }
}

/// The path with its steps in id order and without its head: send order
/// differs from view order once a withheld turn goes out later.
fn by_id(p: &Path) -> Value {
    let mut p = p.clone();
    p.steps.sort_by(|a, b| a.step.id.cmp(&b.step.id));
    p.path.head.clear();
    value(&p)
}

/// The final head is the one-shot's (start order) whenever the stream
/// holds that step, and cost totals do not depend on arrival order.
fn assert_start_order_head_and_totals(back: &Path, one_shot: &Path, what: &str) {
    let shared = back.steps.iter().any(|s| s.step.id == one_shot.path.head);
    assert!(
        !shared || back.path.head == one_shot.path.head,
        "{what}: head {} is not the one-shot's {}",
        back.path.head,
        one_shot.path.head
    );
    let cost = |p: &Path| p.meta.as_ref().unwrap().extra["otel"]["cost_usd"].clone();
    assert_eq!(cost(back), cost(one_shot), "{what}: cost_usd");
}

/// Whether the generation's own record (usage, cost, trace id) reached
/// the reader: on the step it produced, or on its unplaced step.
fn carries_record(p: &Path, g: &Generation) -> bool {
    let is_record = |o: &Value| {
        o["generation_id"] == json!(g.id)
            && o["trace_id"] == json!(g.trace_id)
            && o["usage"] == json!(g.usage)
            && o["cost"] == json!(g.cost)
    };
    p.steps.iter().any(|st| {
        st.change.values().any(|c| {
            c.structural
                .as_ref()
                .and_then(|s| s.extra.get("otel"))
                .is_some_and(is_record)
        })
    })
}

/// `gens` with the `fed` ones first, in that order.
fn in_fed_order(gens: &[Generation], fed: &[String]) -> Session {
    let mut s = session(gens.to_vec());
    s.generations
        .sort_by_key(|g| fed.iter().position(|id| *id == g.id));
    s
}

/// The same path, whatever order its steps were sent in.
fn assert_same(back: &Path, want: &Path, what: &str) {
    assert_eq!(by_id(back), by_id(want), "{what}");
    assert_eq!(back.path.head, want.path.head, "{what}");
}

/// Every sent step equals its step in `full`.
fn assert_unchanged(sent: &[JsonlLine], full: &Path, what: &str) {
    for line in sent {
        if let JsonlLine::Step(st) = line {
            assert_eq!(
                value(&st.0),
                path_step(full, &st.0.step.id),
                "{what}: {} changed after it was sent",
                st.0.step.id
            );
        }
    }
}

/// Every order of `items`.
fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first);
            out.push(p);
        }
    }
    out
}

/// One call per chunk of `batch` arrivals, then a final call.
fn arrive(
    gens: &[Generation],
    order: &[usize],
    batch: usize,
    reader: &mut Reader,
) -> Vec<JsonlLine> {
    let mut arrived = Vec::new();
    let mut sent = Vec::new();
    for chunk in order.chunks(batch) {
        arrived.extend(chunk.iter().map(|&i| gens[i].clone()));
        sent.extend(reader.send(&session(arrived.clone()), false));
    }
    sent.extend(reader.send(&session(arrived), true));
    sent
}

/// The union of the sends: sent steps never change, it reads back to the
/// feed-order derivation, every record arrives, and the head and cost
/// total follow start order.
fn assert_union(
    reader: &Reader,
    sent: &[JsonlLine],
    gens: &[Generation],
    one_shot: &Path,
    what: &str,
) {
    let full = derive_path(&in_fed_order(gens, &reader.fed()));
    assert_unchanged(sent, &full, what);
    let back = reader.path();
    assert_eq!(by_id(&back), by_id(&full), "{what}");
    for g in gens {
        assert!(carries_record(&back, g), "{what}: {}", g.id);
    }
    assert_start_order_head_and_totals(&back, one_shot, what);
}

#[test]
fn a_held_step_changes_only_where_its_results_or_echo_do() {
    let steps = |p: &Path| -> HashMap<String, String> {
        p.steps
            .iter()
            .map(|s| (s.step.id.clone(), canonical_step_json(s)))
            .collect()
    };
    let tools = classifier();
    for s in crate::stitch::tests::fixture_sessions() {
        let graph = stitch(&s);
        let full = steps(&derive_path(&s));
        for k in 0..=s.generations.len() {
            let prefix = stitch_with_prefix(&s, Some(k)).1.unwrap();
            let held = steps(&derive_path(&Session {
                generations: s.generations[..k].to_vec(),
                ..s.clone()
            }));
            let c = Some(&tools);
            let harness = held_harness(&s, k, first_settle(&s, true, c), c);
            assert_eq!(
                steps(&derive_steps(&s, &prefix, &convo(), harness, c).0),
                held
            );
            let now: HashMap<&str, &crate::stitch::Node> =
                graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            for h in &prefix.nodes {
                let n = now[h.id.as_str()];
                if n.results == h.results && n.echo == h.echo {
                    assert_eq!(full[&h.id], held[&h.id], "{} prefix {k}", s.key);
                }
            }
        }
    }
}

/// `held_stored` compares every trusted step, whether or not its turn
/// gained results or an echo.
#[test]
fn held_stored_reports_any_changed_stored_step() {
    let feed = session(vec![generation("g0", 0, json!([user("hi")]), "now")]);
    let graph = stitch(&feed);
    let mut held = graph.clone();
    held.nodes[0].message.content = json!("was");
    let path = derive_path(&feed);
    let stored: HashSet<String> = [graph.nodes[0].id.clone()].into_iter().collect();
    let err = held_stored(
        &path,
        &feed,
        &held,
        SourceHarness::Unknown,
        Some(&classifier()),
        &convo(),
        &stored,
    )
    .unwrap_err();
    assert!(
        matches!(&err, OtelError::Delta(DeltaError::Amended { steps }) if *steps == [graph.nodes[0].id.clone()]),
        "{err:?}"
    );
}

#[test]
fn a_first_send_waits_for_the_main_line_and_withholds_the_pending_completion() {
    let g1 = generation("g1", 1, json!([system("S"), user("go")]), "a");
    let g2 = generation(
        "g2",
        2,
        json!([system("S"), user("go"), assistant("a"), user("more")]),
        "b",
    );
    let one = session(vec![g1.clone()]);
    assert!(
        send(&one, &convo(), &Remote::default(), false, 0)
            .unwrap()
            .is_empty(),
        "one produced turn does not decide the main line"
    );
    let s = session(vec![g1, g2]);
    let full = derive_path(&s);
    let bodies = send(&s, &convo(), &Remote::default(), false, 0).unwrap();
    assert_eq!(bodies.len(), 1);
    let lines = &bodies[0];
    let Some(JsonlLine::PathOpen(open)) = lines.first() else {
        panic!("no PathOpen first");
    };
    assert_eq!(
        value(&open.meta)["otel"]["generation_ids"],
        json!(["g1", "g2"]),
        "the feed order rides in the open"
    );
    let want: Vec<String> = full
        .steps
        .iter()
        .map(|st| st.step.id.clone())
        .filter(|id| *id != full.path.head)
        .collect();
    assert_eq!(want.len(), 4, "system, user, a and the next user turn");
    assert_eq!(step_ids(lines), want);
    assert!(matches!(lines.last(), Some(JsonlLine::Head(h)) if h.step_id == want[3]));
}

#[test]
fn the_next_generation_settles_the_turn_and_final_sends_the_rest() {
    let g1 = generation("g1", 1, json!([user("go")]), "a");
    let g2 = generation(
        "g2",
        2,
        json!([user("go"), assistant("a"), user("more")]),
        "b",
    );
    let g3 = generation(
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
    );
    let s2 = session(vec![g1.clone(), g2.clone()]);
    let s3 = session(vec![g1, g2, g3]);
    let (b, c) = (completion_id(&s2), completion_id(&s3));
    let full = derive_path(&s3);

    let mut reader = Reader::default();
    reader.send(&s2, false);
    assert!(!reader.stored().contains(&b));
    let second = reader.send(&s3, false);
    assert!(matches!(second.first(), Some(JsonlLine::PathMeta(_))));
    let second_ids = step_ids(&second);
    assert_eq!(second_ids.len(), 2, "b and the new user turn");
    assert_eq!(second_ids[0], b, "echoed by g3, sent first");
    assert!(!second_ids.contains(&c));
    assert_eq!(sent_step(&second, &b).unwrap(), path_step(&full, &b));

    let last = reader.send(&s3, true);
    assert_eq!(step_ids(&last), [c]);
    assert_eq!(value(&reader.path()), value(&full));
}

#[test]
fn fallback_tool_results_wait_for_the_echo_or_final() {
    let call = json!([
        {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}}
    ]);
    let mut g1 = generation("g1", 1, json!([user("go")]), "");
    g1.completion.tool_calls = serde_json::from_value(call.clone()).unwrap();
    g1.tool_results.insert(
        "t1".into(),
        serde_json::from_value(json!({"content": "fallback", "is_error": false})).unwrap(),
    );
    let s1 = session(vec![g1.clone()]);
    let called = completion_id(&s1);

    let mut reader = Reader::default();
    let open = reader.send(&s1, false);
    assert!(
        !step_ids(&open).contains(&called),
        "a fallback result does not settle the turn"
    );
    let closed = Reader::default().send(&s1, true);
    assert_eq!(first_result(&closed, &called), json!("fallback"));

    let g2 = generation(
        "g2",
        2,
        json!([
            user("go"),
            {"role": "assistant", "content": null, "tool_calls": call},
            {"role": "tool", "tool_call_id": "t1", "content": "real"}
        ]),
        "done",
    );
    let s2 = session(vec![g1, g2]);
    let next = reader.send(&s2, false);
    assert_eq!(
        first_result(&next, &called),
        json!("real"),
        "the prompt-carried result wins"
    );
    assert_eq!(
        sent_step(&next, &called).unwrap(),
        path_step(&derive_path(&s2), &called)
    );
}

#[test]
fn nothing_settled_sends_nothing() {
    // No prompt, so the only turn is the pending completion.
    let s = session(vec![generation("g1", 1, json!([]), "a")]);
    assert!(
        send(&s, &convo(), &Remote::default(), false, 0)
            .unwrap()
            .is_empty()
    );
    let closed = Reader::default().send(&s, true);
    assert!(matches!(closed.first(), Some(JsonlLine::PathOpen(_))));
    assert_eq!(step_ids(&closed).len(), 1);
}

#[test]
fn a_repeat_send_sends_no_steps() {
    let s = session(vec![generation("g1", 1, json!([user("go")]), "a")]);
    let mut reader = Reader::default();
    reader.send(&s, true);
    let again = reader.send(&s, true);
    assert!(step_ids(&again).is_empty());
    assert!(matches!(again[0], JsonlLine::PathMeta(_)));
    assert!(
        matches!(&again[again.len() - 1], JsonlLine::Head(h) if h.step_id == completion_id(&s))
    );
}

/// `early` is sent, then `late` (which started before them) arrives: a
/// non-final send, then the final one. No stored step may change, every
/// step the late generation adds is appended, its record reaches the
/// reader, and the sends read back to the session derived in feed order.
fn assert_late_arrival_appends(early: Vec<Generation>, late: Generation) {
    let s1 = session(early.clone());
    let mut all = early;
    all.push(late.clone());
    let s2 = session(all);
    assert_eq!(s2.generations[0].id, late.id, "the late one sorts first");

    let mut reader = Reader::default();
    let first = reader.send(&s1, false);
    assert!(!first.is_empty());
    let before = reader.stored();
    let second = reader.send(&s2, false);
    let resent: Vec<String> = step_ids(&second)
        .into_iter()
        .filter(|id| before.contains(id))
        .collect();
    assert!(resent.is_empty(), "stored steps sent again: {resent:?}");
    reader.send(&s2, true);

    let mut fed_order = s2.clone();
    fed_order.generations.rotate_left(1);
    let ids: Vec<&str> = fed_order
        .generations
        .iter()
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(reader.fed(), ids);
    let full = derive_path(&fed_order);
    assert_unchanged(&first, &full, &late.id);
    let back = reader.path();
    assert!(
        carries_record(&back, &late),
        "{}'s record never reached the reader",
        late.id
    );
    assert_eq!(by_id(&back), by_id(&full));
    assert_start_order_head_and_totals(&back, &derive_path(&s2), &late.id);
}

#[test]
fn a_late_generation_on_the_same_chain_leaves_stored_steps_alone() {
    // g1 produced `a`; g2 and g3 carry it as history. g1 is delivered
    // last although it started first.
    assert_late_arrival_appends(
        vec![
            billed(
                "g2",
                2,
                json!([user("go"), assistant("a"), user("more")]),
                "b",
            ),
            billed(
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
        ],
        billed("g1", 1, json!([user("go")]), "a"),
    );
}

#[test]
fn a_late_generation_on_its_own_branch_leaves_stored_steps_alone() {
    // A sub-agent request under the same system prompt, delivered after
    // the main line although it started first.
    assert_late_arrival_appends(
        vec![
            billed("g2", 2, json!([system("S"), user("go")]), "b"),
            billed(
                "g3",
                3,
                json!([system("S"), user("go"), assistant("b"), user("again")]),
                "c",
            ),
        ],
        billed("g1", 1, json!([system("S"), user("sub")]), "x"),
    );
}

#[test]
fn a_late_parallel_request_forks_and_settles_like_any_other() {
    // g1 and g2 answer the same prompt concurrently; g2 finishes (and
    // is sent) first, g3 continues from g2's answer, and g1 arrives
    // last. g1's answer is a fork that waits for final.
    let g1 = billed("g1", 1, json!([user("go")]), "slow");
    let g2 = billed("g2", 2, json!([user("go")]), "fast");
    let g3 = billed(
        "g3",
        3,
        json!([user("go"), assistant("fast"), user("more")]),
        "c",
    );
    let mut reader = Reader::default();
    reader.send(&session(vec![g2.clone(), g3.clone()]), false);
    let all = session(vec![g1.clone(), g2, g3]);
    let slow = derive_path(&session(vec![g1.clone()])).path.head;
    assert!(!step_ids(&reader.send(&all, false)).contains(&slow));
    assert!(step_ids(&reader.send(&all, true)).contains(&slow));
    assert!(carries_record(&reader.path(), &g1));
}

#[test]
fn a_delta_generation_fed_before_its_target_keeps_its_chain() {
    // g2 continues g1 server-side, but g1 is delivered after g2 was
    // sent: g2's user turn was sent as a root and must stay one.
    let g1 = billed("g1", 1, json!([user("go")]), "a");
    let mut g2 = billed("g2", 2, json!([user("more")]), "b");
    g2.continues = Some("g1".into());
    g2.history = serde_json::from_value(json!("delta")).unwrap();
    let mut g3 = billed("g3", 3, json!([user("again")]), "c");
    g3.continues = Some("g2".into());
    g3.history = serde_json::from_value(json!("delta")).unwrap();
    let mut reader = Reader::default();
    let first = reader.send(&session(vec![g2.clone(), g3.clone()]), false);
    assert_eq!(step_ids(&first).len(), 1, "g2's user turn");
    let all = session(vec![g1.clone(), g2, g3]);
    reader.send(&all, false);
    reader.send(&all, true);
    let back = reader.path();
    let more = &step_ids(&first)[0];
    assert!(
        back.steps
            .iter()
            .any(|s| &s.step.id == more && s.step.parents.is_empty())
    );
    assert!(carries_record(&back, &g1));
}

#[test]
fn several_late_generations_in_one_call_append_in_start_order() {
    let g2 = billed("g2", 20, json!([system("S"), user("go")]), "b");
    let g3 = billed(
        "g3",
        30,
        json!([system("S"), user("go"), assistant("b"), user("again")]),
        "c",
    );
    let la = billed("la", 5, json!([system("S"), user("sub-a")]), "xa");
    let lb = billed("lb", 1, json!([system("S"), user("sub-b")]), "xb");
    let mut reader = Reader::default();
    let first = reader.send(&session(vec![g2.clone(), g3.clone()]), false);
    let all = session(vec![la.clone(), lb.clone(), g2, g3]);
    let second = reader.send(&all, false);
    assert_eq!(reader.fed(), ["g2", "g3", "lb", "la"], "late ones by start");
    assert_eq!(step_ids(&second).len(), 2, "both sub-agent prompts");
    reader.send(&all, true);
    let back = reader.path();
    assert!(carries_record(&back, &la) && carries_record(&back, &lb));
    let full = derive_path(&in_fed_order(&all.generations, &reader.fed()));
    assert_unchanged(&first, &full, "late pair");
    assert_eq!(by_id(&back), by_id(&full));
    assert_start_order_head_and_totals(&back, &derive_path(&all), "late pair");
}

#[test]
fn a_late_generation_settles_an_unsent_parent_and_both_go_out_parents_first() {
    // `a` is withheld (never echoed). The late generation's prompt
    // echoes it, so `a` and the late user turn under it settle together.
    let g2 = billed("g2", 2, json!([user("go")]), "a");
    let late = billed(
        "late",
        1,
        json!([user("go"), assistant("a"), user("side")]),
        "y",
    );
    // A retry of g2 decides the main line without echoing `a`.
    let retry = billed("g2r", 3, json!([user("go")]), "a2");
    let mut reader = Reader::default();
    reader.send(&session(vec![g2.clone(), retry.clone()]), false);
    let a = completion_id(&session(vec![g2.clone()]));
    assert!(!reader.stored().is_empty() && !reader.stored().contains(&a));
    let next = reader.send(&session(vec![late, g2, retry]), false);
    let ids = step_ids(&next);
    assert_eq!(ids.len(), 2, "a, then the late user turn");
    assert_eq!(ids[0], a);
}

#[test]
fn a_late_turn_under_a_still_unsettled_parent_waits_for_final() {
    // `a` calls t1, which no prompt answers; the late generation echoes
    // `a` and adds a user turn under it, which waits with `a`.
    let call = json!([
        {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
    ]);
    let mut g2 = billed("g2", 2, json!([user("go")]), "");
    g2.completion.tool_calls = serde_json::from_value(call.clone()).unwrap();
    let late = billed(
        "late",
        1,
        json!([
            user("go"),
            {"role": "assistant", "content": null, "tool_calls": call},
            user("side")
        ]),
        "y",
    );
    let retry = billed("g2r", 3, json!([user("go")]), "a2");
    let mut reader = Reader::default();
    reader.send(&session(vec![g2.clone(), retry.clone()]), false);
    let all = session(vec![late, g2, retry]);
    assert!(step_ids(&reader.send(&all, false)).is_empty());
    let last = reader.send(&all, true);
    assert_eq!(step_ids(&last).len(), 4, "a, the retry, side and y");
}

#[test]
fn a_late_generation_repeating_sent_turns_sends_only_its_unplaced_step() {
    // A retry of g2's request, started first and delivered last, with
    // the same answer: every turn it carries is already stored, so only
    // its unplaced step, a dead end beside `a`, goes out.
    let g2 = billed("g2", 2, json!([user("go")]), "a");
    let g3 = billed(
        "g3",
        3,
        json!([user("go"), assistant("a"), user("more")]),
        "b",
    );
    let retry = billed("retry", 1, json!([user("go")]), "a");
    let g2_only = g2.clone();
    let mut reader = Reader::default();
    reader.send(&session(vec![g2.clone(), g3.clone()]), false);
    let all = session(vec![retry.clone(), g2, g3]);
    let remote = reader.server.remote(Hint::Exact);
    let next = reader.send(&all, false);
    let a = completion_id(&session(vec![g2_only.clone()]));
    assert_eq!(step_ids(&next), [format!("{a}~retry")]);
    assert!(carries_record(&reader.path(), &retry));
    // Replaying the same call gives the same lines, and the server takes
    // them again without change.
    let again = send(&all, &convo(), &remote, false, 0).unwrap();
    assert_eq!(value(&again.concat()), value(&next));
    let before = reader.server.steps.clone();
    for body in &again {
        reader.server.apply(body).unwrap();
    }
    assert_eq!(reader.server.steps, before);
}

#[test]
fn every_arrival_order_only_appends_and_adds_up_to_the_feed_order_path() {
    let call = json!([
        {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
    ]);
    let echo = json!({"role": "assistant", "content": null, "tool_calls": call});
    let mut g1 = billed("g1", 1, json!([system("S"), user("go")]), "");
    g1.completion.tool_calls = serde_json::from_value(call.clone()).unwrap();
    let gens = vec![
        g1,
        billed(
            "g2",
            2,
            json!([system("S"), user("go"), echo.clone(), {"role": "tool", "tool_call_id": "t1", "content": "out"}]),
            "b",
        ),
        billed("g3", 3, json!([system("S"), user("go")]), "parallel"),
        billed("g4", 4, json!([system("S"), user("sub")]), "x"),
        billed(
            "g5",
            5,
            json!([
                system("S"),
                user("go"),
                echo,
                {"role": "tool", "tool_call_id": "t1", "content": "out"},
                assistant("b"),
                user("more")
            ]),
            "c",
        ),
    ];
    let one_shot = derive_path(&session(gens.clone()));
    for (n, order) in permutations(&[0, 1, 2, 3, 4]).into_iter().enumerate() {
        // One call per arrival, and one call per two arrivals; the stored
        // hint and the body size vary with the order.
        for batch in [1, 2] {
            let (hint, max) = (HINTS[n % 3], [0, 1][n % 2]);
            let what = format!("{order:?}/{batch}/{hint:?}/{max}");
            let mut reader = Reader::new(hint, max);
            let sent = arrive(&gens, &order, batch, &mut reader);
            assert_union(&reader, &sent, &gens, &one_shot, &what);
            if order.is_sorted() {
                assert_eq!(by_id(&reader.path()), by_id(&one_shot));
                assert_eq!(reader.path().path.head, one_shot.path.head);
            }
        }
    }
}

#[test]
fn a_step_a_final_send_left_unsettled_cannot_be_amended() {
    // The final send gives the call its fallback result; a later
    // generation carries the real one, which would change a stored step.
    let call = json!([
        {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
    ]);
    let mut g1 = generation("g1", 1, json!([user("go")]), "");
    g1.completion.tool_calls = serde_json::from_value(call.clone()).unwrap();
    g1.tool_results.insert(
        "t1".into(),
        serde_json::from_value(json!({"content": "fallback", "is_error": false})).unwrap(),
    );
    let s1 = session(vec![g1.clone()]);
    let called = completion_id(&s1);
    let g2 = generation(
        "g2",
        2,
        json!([
            user("go"),
            {"role": "assistant", "content": null, "tool_calls": call},
            {"role": "tool", "tool_call_id": "t1", "content": "real"}
        ]),
        "done",
    );
    let s2 = session(vec![g1, g2]);
    for hint in HINTS {
        let mut reader = Reader::new(hint, 0);
        reader.send(&s1, true);
        let before = reader.server.clone();
        match reader.try_send(&s2, false).unwrap_err() {
            Failure::Derive(OtelError::Delta(DeltaError::Amended { steps })) => {
                assert_ne!(hint, Hint::Empty, "not told the step is stored");
                assert_eq!(steps, std::slice::from_ref(&called));
            }
            // Not told the step is stored, the call sends the change and
            // the server refuses the whole body.
            Failure::Rejected(why) => {
                assert_ne!(hint, Hint::Exact, "told the step is stored");
                assert!(why.contains("invalid_document"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            value(&reader.path()),
            value(&before.path()),
            "nothing stored"
        );
    }
}

#[test]
fn a_repeated_fed_id_counts_once() {
    let g1 = generation("g1", 1, json!([user("go")]), "a");
    let g2 = generation(
        "g2",
        2,
        json!([user("go"), assistant("a"), user("more")]),
        "b",
    );
    let g3 = generation(
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
    );
    let s = session(vec![g1, g2, g3]);
    let remote = |fed: &[&str]| Remote {
        opened: true,
        fed: fed.iter().map(|s| s.to_string()).collect(),
        ..Remote::default()
    };
    let once = send(&s, &convo(), &remote(&["g2", "g1"]), false, 0).unwrap();
    let twice = send(&s, &convo(), &remote(&["g2", "g1", "g2"]), false, 0).unwrap();
    assert_eq!(value(&twice), value(&once));
    let Some(JsonlLine::PathMeta(m)) = once[0].first() else {
        panic!("no PathMeta first");
    };
    assert_eq!(
        m.patch.extra["otel"]["generation_ids"],
        json!(["g2", "g1", "g3"])
    );
}

#[test]
fn a_fed_generation_the_session_lacks_is_an_error() {
    let s = session(vec![generation("g1", 1, json!([user("go")]), "a")]);
    let fed: Vec<String> = ["g1", "gone-a", "gone-b", "gone-c", "gone-d"]
        .map(String::from)
        .into();
    let remote = Remote {
        opened: true,
        fed,
        ..Remote::default()
    };
    for _ in 0..8 {
        let err = send(&s, &convo(), &remote, false, 0).unwrap_err();
        assert!(
            matches!(&err, OtelError::FedGenerationMissing(id) if id == "gone-a"),
            "{err:?}"
        );
    }
}

#[test]
fn a_delta_error_surfaces_as_otel_error_delta() {
    // `send` itself cannot dangle (every sent step's parent is sent
    // first), so the mapping is checked directly.
    let err: OtelError = DeltaError::DanglingParent {
        step: "b".into(),
        parent: "a".into(),
    }
    .into();
    assert!(matches!(
        &err,
        OtelError::Delta(DeltaError::DanglingParent { step, parent }) if step == "b" && parent == "a"
    ));
    assert!(err.to_string().starts_with("incremental JSONL: "));
    assert!(std::error::Error::source(&err).is_some());
}

#[test]
fn a_late_sub_agent_does_not_take_the_head() {
    // The main line g2, g3 is sent; the sub-agent request g1 started
    // first and finished last.
    let g2 = billed("g2", 2, json!([system("S"), user("go")]), "b");
    let g3 = billed(
        "g3",
        3,
        json!([system("S"), user("go"), assistant("b"), user("again")]),
        "c",
    );
    let g1 = billed("g1", 1, json!([system("S"), user("sub")]), "x");
    let all = session(vec![g1, g2.clone(), g3.clone()]);
    let one_shot = derive_path(&all);
    let again = path_step(&one_shot, &one_shot.path.head)["step"]["parents"][0]
        .as_str()
        .unwrap()
        .to_string();

    for max in [0, 1] {
        let mut reader = Reader::new(Hint::Exact, max);
        let first = reader.send(&session(vec![g2.clone(), g3.clone()]), false);
        assert_eq!(head_of(&first), Some(again.clone()));
        let late = reader.send(&all, false);
        assert_eq!(step_ids(&late).len(), 1, "the sub-agent's prompt");
        assert_eq!(
            head_of(&late),
            Some(again.clone()),
            "the main line keeps the head"
        );
        let last = reader.send(&all, true);
        assert_eq!(head_of(&last), Some(one_shot.path.head.clone()));
        assert_eq!(reader.path().path.head, one_shot.path.head);
    }
}

#[test]
fn a_claimed_step_the_fed_generations_do_not_derive_is_sent_again() {
    // g2's steps are stored under the feed order [g0, g2]. A caller that
    // passes an older feed order ([g0]) while g1, which produced the `x`
    // g2 carries as history, arrives: `x` is not trusted as stored, goes
    // out again with g1's record, and the server refuses the change.
    let g0 = billed("g0", 0, json!([user("go")]), "a");
    let g1 = billed(
        "g1",
        1,
        json!([user("go"), assistant("a"), user("more")]),
        "x",
    );
    let g2 = billed(
        "g2",
        2,
        json!([
            user("go"),
            assistant("a"),
            user("more"),
            assistant("x"),
            user("again")
        ]),
        "z",
    );
    let mut reader = Reader::default();
    reader.send(&session(vec![g0.clone()]), false);
    let stale = reader.fed();
    reader.send(&session(vec![g0.clone(), g2.clone()]), false);
    let all = session(vec![g0, g1, g2]);
    let mut remote = reader.server.remote(Hint::Exact);
    remote.fed = stale;
    let lines = send(&all, &convo(), &remote, false, 0).unwrap().concat();
    let x = completion_id(&session(vec![all.generations[1].clone()]));
    assert!(step_ids(&lines).contains(&x), "x goes out again");
    let err = reader.server.clone().apply(&lines).unwrap_err();
    assert!(err.contains("invalid_document"), "{err}");

    // The stored meta's feed order is the one that works.
    assert_eq!(fed_of(&reader.path()), ["g0", "g2"]);
    assert!(reader.try_send(&all, false).is_ok());
}

/// A session with a fallback-result call (`d1`, answered for real by
/// `d2`'s prompt), a `Delta` continuation of `d2`, a prompt-absent
/// skeleton and a sub-agent request.
fn chained_generations() -> Vec<Generation> {
    let call = json!([
        {"id": "t1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
    ]);
    let echo = json!({"role": "assistant", "content": null, "tool_calls": call});
    let mut d1 = billed("d1", 1, json!([system("S"), user("go")]), "");
    d1.completion.tool_calls = serde_json::from_value(call).unwrap();
    d1.tool_results.insert(
        "t1".into(),
        serde_json::from_value(json!({"content": "fallback", "is_error": false})).unwrap(),
    );
    let d2 = billed(
        "d2",
        2,
        json!([system("S"), user("go"), echo, {"role": "tool", "tool_call_id": "t1", "content": "real"}]),
        "b",
    );
    let mut d3 = billed("d3", 3, json!([user("more")]), "c");
    d3.continues = Some("d2".into());
    d3.history = serde_json::from_value(json!("delta")).unwrap();
    let mut d4 = billed("d4", 4, json!([]), "");
    d4.absent.prompt = true;
    d4.absent.completion = true;
    let d5 = billed("d5", 5, json!([system("S"), user("sub")]), "x");
    vec![d1, d2, d3, d4, d5]
}

#[test]
fn delta_skeleton_and_fallback_generations_only_append_in_any_order() {
    let gens = chained_generations();
    let one_shot = derive_path(&session(gens.clone()));
    for (n, order) in permutations(&[0, 1, 2, 3, 4]).into_iter().enumerate() {
        for batch in [1, 2] {
            let (hint, max) = (HINTS[(n + batch) % 3], [0, 1][n % 2]);
            let what = format!("{order:?}/{batch}/{hint:?}/{max}");
            let mut reader = Reader::new(hint, max);
            let sent = arrive(&gens, &order, batch, &mut reader);
            assert_union(&reader, &sent, &gens, &one_shot, &what);
        }
    }
}

/// A main thread that delegates to a returning sub-agent and a
/// background one, then a side request: the sub-agents' steps, the turns
/// that receive their answers and the side request wait until their marks
/// and parents are known, so in every arrival order no sent step changes.
fn delegating_generations() -> Vec<Generation> {
    let agent = |id: &str, prompt: &str| {
        json!({"id": id, "type": "function", "function": {"name": "Agent",
            "arguments": json!({"description": "d", "prompt": prompt}).to_string()}})
    };
    let calls = json!([agent("c1", "sub A"), agent("c2", "sub B")]);
    let mut m0 = billed("m0", 10, json!([system("MAIN"), user("do it")]), "");
    m0.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
    let sub = |id: &str, start, prompt: &str, out: &str| {
        let first = json!({"role": "user", "content": [
            {"type": "text", "text": "<system-reminder>ctx</system-reminder>"},
            {"type": "text", "text": prompt}]});
        billed(id, start, json!([system("SUB"), first]), out)
    };
    let mut prompt = vec![
        system("MAIN"),
        user("do it"),
        json!({"role": "assistant", "content": null, "tool_calls": calls}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "A done"}),
        json!({"role": "tool", "tool_call_id": "c2", "content": "launched"}),
    ];
    let m1 = billed("m1", 30, Value::Array(prompt.clone()), "waiting");
    prompt.push(assistant("waiting"));
    prompt.push(user("<task-notification>B done</task-notification>"));
    let m2 = billed("m2", 40, Value::Array(prompt), "all done");
    vec![
        m0,
        sub("a1", 20, "sub A", "A done"),
        sub("b1", 21, "sub B", "B done"),
        m1,
        m2,
        billed("t", 50, json!([system("TITLE"), user("name it")]), "Title"),
    ]
}

#[test]
fn sub_agents_and_side_requests_only_append_in_any_order() {
    let gens = delegating_generations();
    let one_shot = derive_path(&session(gens.clone()));
    let otel = |id: &str| {
        let st = one_shot.steps.iter().find(|s| s.step.id == id).unwrap();
        let c = st.change.values().next().unwrap();
        c.structural.as_ref().unwrap().extra["otel"].clone()
    };
    let marks: Vec<Value> = one_shot
        .steps
        .iter()
        .map(|s| otel(&s.step.id)["branch"].clone())
        .collect();
    assert!(marks.contains(&json!("subagent")) && marks.contains(&json!("side")));
    assert!(
        one_shot
            .steps
            .iter()
            .filter(|s| s.step.parents.len() == 2)
            .count()
            == 2,
        "both answers merge back"
    );
    // In true order the sub-agents and the merged main line go out before
    // `final_`; only the side request waits for it.
    let mut reader = Reader::default();
    let mut early = Vec::new();
    for k in 1..=gens.len() {
        early.extend(step_ids(&reader.send(&session(gens[..k].to_vec()), false)));
    }
    let side: Vec<&String> = one_shot
        .steps
        .iter()
        .map(|s| &s.step.id)
        .filter(|id| otel(id)["branch"] == json!("side"))
        .collect();
    assert!(
        early
            .iter()
            .any(|id| otel(id)["branch"] == json!("subagent"))
    );
    for st in one_shot.steps.iter().filter(|s| s.step.parents.len() == 2) {
        assert!(early.contains(&st.step.id), "a receiving turn goes out");
    }
    assert!(side.iter().all(|id| !early.contains(id)));
    for (n, order) in permutations(&[0, 1, 2, 3, 4, 5]).into_iter().enumerate() {
        for batch in [1, 2] {
            let (hint, max) = (HINTS[(n + batch) % 3], [0, 1][n % 2]);
            let what = format!("{order:?}/{batch}/{hint:?}/{max}");
            let mut reader = Reader::new(hint, max);
            let sent = arrive(&gens, &order, batch, &mut reader);
            assert_union(&reader, &sent, &gens, &one_shot, &what);
        }
    }
}

/// The last generation arrives after a `final_` send: the call either
/// appends without changing a stored step, or is refused (`Amended`, or
/// the server's `invalid_document` when the caller did not say the step
/// is stored) with nothing stored.
#[test]
fn a_late_arrival_after_final_appends_or_is_refused() {
    let gens = chained_generations();
    let one_shot = derive_path(&session(gens.clone()));
    let (mut appended, mut amended, mut rejected) = (0, 0, 0);
    for (n, order) in permutations(&[0, 1, 2, 3, 4]).into_iter().enumerate() {
        let hint = HINTS[n % 3];
        let what = format!("{order:?}/{hint:?}");
        let (last, early) = order.split_last().unwrap();
        let mut reader = Reader::new(hint, 0);
        let mut arrived = Vec::new();
        let mut sent = Vec::new();
        for &i in early {
            arrived.push(gens[i].clone());
            sent.extend(reader.send(&session(arrived.clone()), false));
        }
        sent.extend(reader.send(&session(arrived.clone()), true));
        arrived.push(gens[*last].clone());
        let all = session(arrived);
        let before = reader.server.clone();
        match reader.try_send(&all, true) {
            Ok(lines) => {
                appended += 1;
                sent.extend(lines);
                assert_union(&reader, &sent, &gens, &one_shot, &what);
            }
            Err(Failure::Derive(OtelError::Delta(DeltaError::Amended { steps }))) => {
                amended += 1;
                assert!(
                    !steps.is_empty() && steps.iter().all(|id| before.steps.contains_key(id)),
                    "{what}: {steps:?}"
                );
                assert_eq!(reader.server.text, before.text, "{what}");
            }
            Err(Failure::Rejected(why)) => {
                rejected += 1;
                assert!(why.contains("invalid_document"), "{what}: {why}");
                assert_eq!(reader.server.text, before.text, "{what}");
            }
            Err(e) => panic!("{what}: {e:?}"),
        }
    }
    assert!(
        appended > 0 && amended > 0 && rejected > 0,
        "{appended}/{amended}/{rejected}"
    );
}

#[test]
fn a_head_the_stream_lacks_falls_back_to_the_latest_shared_step() {
    // g2 continues g1 by Delta but is fed first, so it chains from the
    // root and the one-shot head (g2's completion under g1's) is never
    // derived: the head is the shared step the one-shot places last.
    let g1 = billed("g1", 1, json!([user("go")]), "a");
    let mut g2 = billed("g2", 2, json!([user("more")]), "b");
    g2.continues = Some("g1".into());
    g2.history = serde_json::from_value(json!("delta")).unwrap();
    let mut g3 = billed("g3", 3, json!([user("again")]), "c");
    g3.continues = Some("g2".into());
    g3.history = serde_json::from_value(json!("delta")).unwrap();
    let all = session(vec![g1.clone(), g2.clone(), g3.clone()]);
    let one_shot = derive_path(&all);
    let a = completion_id(&session(vec![g1]));
    let mut reader = Reader::default();
    reader.send(&session(vec![g2, g3]), false);
    reader.send(&all, true);
    let back = reader.path();
    assert!(back.steps.iter().all(|s| s.step.id != one_shot.path.head));
    assert_eq!(back.path.head, a);
}

#[test]
fn an_empty_stored_set_resends_every_settled_step_identically() {
    let gens = chained_generations();
    let one_shot = derive_path(&session(gens.clone()));
    let mut reader = Reader::new(Hint::Empty, 0);
    let mut arrived = Vec::new();
    for g in &gens {
        arrived.push(g.clone());
        let before = reader.server.steps.len();
        let lines = reader.send(&session(arrived.clone()), false);
        assert!(
            step_ids(&lines).len() >= before,
            "everything settled goes out"
        );
    }
    reader.send(&session(arrived), true);
    assert_same(&reader.path(), &one_shot, "empty");
}

#[test]
fn a_subset_of_the_stored_ids_sends_the_rest_again() {
    let gens = chained_generations();
    let s = session(gens.clone());
    let mut reader = Reader::default();
    reader.send(&s, false);
    let stored = reader.stored();
    let remote = reader.server.remote(Hint::Alternate);
    assert!(!remote.stored.is_empty() && remote.stored.len() < stored.len());
    let lines = send(&s, &convo(), &remote, false, 0).unwrap().concat();
    let ids: HashSet<String> = step_ids(&lines).into_iter().collect();
    let want: HashSet<String> = stored.difference(&remote.stored).cloned().collect();
    assert_eq!(ids, want, "exactly the stored steps not claimed");
    reader.server.apply(&lines).unwrap();
}

/// A frozen path's steps, then generations that arrive after the freeze:
/// a path not yet opened, continuing what the frozen one holds, gets a
/// `PathOpen` and only new steps, never a frozen id, the roots' parents
/// naming frozen steps, and a `Head` among its own steps.
#[test]
fn a_continuation_sends_only_new_steps_anchored_on_frozen_ones() {
    let g2 = billed("g2", 2, json!([system("S"), user("go")]), "b");
    let g3 = billed(
        "g3",
        3,
        json!([system("S"), user("go"), assistant("b"), user("again")]),
        "c",
    );
    let g4 = billed(
        "g4",
        4,
        json!([
            system("S"),
            user("go"),
            assistant("b"),
            user("again"),
            assistant("c"),
            user("more")
        ]),
        "d",
    );
    // A sub-agent that started first and forks off frozen history.
    let g1 = billed("g1", 1, json!([system("S"), user("sub")]), "x");
    let mut frozen = Reader::default();
    frozen.send(&session(vec![g2.clone(), g3.clone()]), true);
    let held = frozen.stored();
    let all = session(vec![g1, g2, g3, g4]);
    for max in [0, 1] {
        let remote = Remote {
            opened: false,
            fed: frozen.fed(),
            base: held.clone(),
            ..Remote::default()
        };
        let bodies = send(&all, &convo(), &remote, false, max).unwrap();
        assert!(matches!(bodies[0][0], JsonlLine::PathOpen(_)));
        let lines = bodies.concat();
        let ids = step_ids(&lines);
        assert!(!ids.is_empty());
        assert!(
            ids.iter().all(|id| !held.contains(id)),
            "a frozen id is reused"
        );
        let mut seen = held.clone();
        for line in &lines {
            match line {
                JsonlLine::Step(st) => {
                    assert!(st.0.step.parents.iter().all(|p| seen.contains(p)));
                    seen.insert(st.0.step.id.clone());
                }
                JsonlLine::Head(h) => assert!(ids.contains(&h.step_id), "Head on a frozen step"),
                _ => {}
            }
        }
        let anchored = lines.iter().any(|l| {
            matches!(l, JsonlLine::Step(st) if st.0.step.parents.iter().any(|p| held.contains(p)))
        });
        assert!(anchored, "the new steps hang off frozen ones");
        // The frozen path plus the continuation is the session in feed order.
        let mut fed = frozen.fed();
        fed.extend(["g1".to_string(), "g4".to_string()]);
        let full = derive_path(&in_fed_order(&all.generations, &fed));
        assert_unchanged(&lines, &full, "continuation");
    }
    // Nothing new settled: nothing to open.
    let remote = Remote {
        opened: false,
        fed: frozen.fed(),
        base: held.clone(),
        ..Remote::default()
    };
    let same = session(vec![
        billed("g2", 2, json!([system("S"), user("go")]), "b"),
        billed(
            "g3",
            3,
            json!([system("S"), user("go"), assistant("b"), user("again")]),
            "c",
        ),
    ]);
    assert!(send(&same, &convo(), &remote, true, 0).unwrap().is_empty());
}

#[test]
fn opened_picks_path_meta_over_path_open() {
    let s = session(vec![
        generation("g1", 1, json!([user("go")]), "a"),
        generation(
            "g2",
            2,
            json!([user("go"), assistant("a"), user("more")]),
            "b",
        ),
    ]);
    for opened in [false, true] {
        let remote = Remote {
            opened,
            ..Remote::default()
        };
        let bodies = send(&s, &convo(), &remote, true, 0).unwrap();
        let lines = &bodies[0];
        assert_eq!(matches!(lines[0], JsonlLine::PathOpen(_)), !opened);
        assert_eq!(matches!(lines[0], JsonlLine::PathMeta(_)), opened);
        let opens = lines
            .iter()
            .filter(|l| matches!(l, JsonlLine::PathOpen(_) | JsonlLine::PathMeta(_)))
            .count();
        assert_eq!(opens, 1);
        // The patch carries every meta key the open does.
        let full = derive_path(&s);
        let meta = match &lines[0] {
            JsonlLine::PathOpen(o) => value(&o.meta),
            JsonlLine::PathMeta(m) => value(&m.patch),
            _ => unreachable!(),
        };
        assert_eq!(
            meta["otel"],
            value(&full.meta.as_ref().unwrap().extra["otel"])
        );
    }
}

#[test]
fn bodies_hold_at_most_max_steps_and_the_feed_order_commits_with_the_first() {
    let gens = chained_generations();
    let s = session(gens.clone());
    for max in [1, 2, 3] {
        let bodies = send(&s, &convo(), &Remote::default(), true, max).unwrap();
        let steps: usize = bodies.iter().map(|b| step_ids(b).len()).sum();
        assert_eq!(bodies.len(), steps.div_ceil(max));
        for (i, b) in bodies.iter().enumerate() {
            assert!(step_ids(b).len() <= max);
            assert_eq!(
                matches!(b[0], JsonlLine::PathOpen(_)),
                i == 0,
                "only the first body opens"
            );
        }
        // Only the first body commits; the next call reads its feed order
        // back from the stored meta and sends the rest.
        let mut reader = Reader::new(Hint::Exact, max);
        reader.server.apply(&bodies[0]).unwrap();
        assert_eq!(reader.fed(), ["d1", "d2", "d3", "d4", "d5"]);
        reader.send(&s, true);
        assert_eq!(value(&reader.path()), value(&derive_path(&s)));
    }
}

#[test]
fn a_non_final_body_heads_the_latest_main_line_step_stored_by_then() {
    let g2 = billed("g2", 2, json!([system("S"), user("go")]), "b");
    let g3 = billed(
        "g3",
        3,
        json!([system("S"), user("go"), assistant("b"), user("again")]),
        "c",
    );
    let g1 = billed("g1", 1, json!([system("S"), user("sub")]), "x");
    let all = session(vec![g1, g2, g3]);
    let one_shot = derive_path(&all);
    let rank: HashMap<&str, usize> = one_shot
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.step.id.as_str(), i))
        .collect();
    let bodies = send(&all, &convo(), &Remote::default(), true, 1).unwrap();
    let mut have: Vec<String> = Vec::new();
    for body in &bodies {
        have.extend(step_ids(body));
        let best = have.iter().max_by_key(|id| rank[id.as_str()]).unwrap();
        assert_eq!(head_of(body).as_ref(), Some(best));
    }
    assert_eq!(head_of(bodies.last().unwrap()), Some(one_shot.path.head));
}

// ---------------------------------------------------------------------------
// The public API over the committed fixtures.
// ---------------------------------------------------------------------------

/// The session `requests` hold, keyed and ordered as `derive_jsonl` does
/// for this feed order, derived.
fn fed_order_path(requests: &[Value], fed: &[String]) -> Path {
    let (mut s, _) = read_session(requests, &classified()).unwrap();
    if s.session_id.is_none()
        && let Some(g) = s.generations.iter().find(|g| Some(&g.id) == fed.first())
    {
        s.key = derived_key(g);
    }
    derive_path(&in_feed_order(&s, fed).unwrap().0)
}

/// What a store holds after reading `requests` one delivery at a time:
/// the records in delivery order and their messages, each through JSON.
fn stored(requests: &[Value]) -> GenerationBatch {
    let mut all = GenerationBatch::default();
    for r in requests {
        let read = crate::read_generations(std::slice::from_ref(r), Default::default())
            .unwrap()
            .output;
        let text = serde_json::to_string(&read).unwrap();
        let read: GenerationBatch = serde_json::from_str(&text).unwrap();
        all.records.extend(read.records);
        all.messages.extend(read.messages);
    }
    all
}

fn files() -> Vec<&'static str> {
    CONVERSATIONS.to_vec()
}

/// One call per delivery (or pair), then a final call, through the
/// public API.
fn arrive_requests(
    all: &[Value],
    order: &[usize],
    batch: usize,
    reader: &mut Reader,
) -> Vec<JsonlLine> {
    let mut arrived = Vec::new();
    let mut sent = Vec::new();
    for chunk in order.chunks(batch) {
        arrived.extend(chunk.iter().map(|&i| all[i].clone()));
        sent.extend(reader.send_requests(&arrived, false));
    }
    sent.extend(reader.send_requests(&arrived, true));
    sent
}

/// Arrival orders: every one for a short session, else reversed and a
/// fixed shuffle.
fn arrivals(n: usize) -> Vec<Vec<usize>> {
    if n <= 5 {
        return permutations(&(0..n).collect::<Vec<_>>());
    }
    let mut shuffled: Vec<usize> = (0..n).collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    for i in (1..n).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        shuffled.swap(i, (seed >> 33) as usize % (i + 1));
    }
    vec![(0..n).collect(), (0..n).rev().collect(), shuffled]
}

#[test]
fn the_sends_read_back_to_the_one_shot_path() {
    for file in files() {
        let all = deliveries(file);
        let one_shot = crate::derive_path(&all, &classified()).unwrap().output;
        for hint in HINTS {
            for max in [0, 1] {
                let mut reader = Reader::new(hint, max);
                let order: Vec<usize> = (0..all.len()).collect();
                let sent = arrive_requests(&all, &order, 1, &mut reader);
                let what = format!("{file}/{hint:?}/{max}");
                assert_same(&reader.path(), &one_shot, &what);
                assert_unchanged(&sent, &one_shot, &what);
            }
        }
    }
}

#[test]
fn a_pending_completion_is_withheld_then_sent_after_the_next_generation() {
    for (file, _) in REAL {
        let all = deliveries(file);
        let mut reader = Reader::default();
        let sends: Vec<Vec<JsonlLine>> = (1..=all.len())
            .map(|k| reader.send_requests(&all[..k], k == all.len()))
            .collect();
        for k in 1..all.len() {
            let pending = crate::derive_path(&all[..k], &classified())
                .unwrap()
                .output
                .path
                .head;
            assert!(
                !step_ids(&sends[k - 1]).contains(&pending),
                "{file}: delivery {k}'s completion sent before its echo"
            );
            assert!(
                step_ids(&sends[k]).contains(&pending),
                "{file}: delivery {k}'s completion not sent after its echo"
            );
        }
    }
}

#[test]
fn final_sends_everything_at_once_with_the_one_shot_skips() {
    for file in files() {
        let all = deliveries(file);
        let one = crate::derive_path(&all, &classified()).unwrap();
        let read = crate::read_generations(&all, Default::default()).unwrap();
        let d = derive_jsonl(
            &read.output.records,
            |h| read.output.messages.get(h),
            &classified(),
            &Remote::default(),
            true,
            0,
        )
        .unwrap();
        assert_eq!(read.skipped, one.skipped, "{file}");
        assert_eq!(d.skipped, crate::SkipCounts::default(), "{file}");
        let lines = d.output.concat();
        let want: Vec<String> = one
            .output
            .steps
            .iter()
            .map(|st| st.step.id.clone())
            .collect();
        assert_eq!(step_ids(&lines), want, "{file}");
        let mut server = Server::default();
        for body in &d.output {
            server.apply(body).unwrap();
        }
        assert_eq!(value(&server.path()), value(&one.output), "{file}");
    }
}

#[test]
fn out_of_order_arrivals_only_append() {
    for file in files() {
        let all = deliveries(file);
        let one_shot = crate::derive_path(&all, &classified()).unwrap().output;
        for (n, order) in arrivals(all.len()).into_iter().enumerate() {
            for batch in [1, 2] {
                let (hint, max) = (HINTS[(n + batch) % 3], [0, 1][n % 2]);
                let what = format!("{file} {order:?}/{batch}/{hint:?}/{max}");
                let mut reader = Reader::new(hint, max);
                let sent = arrive_requests(&all, &order, batch, &mut reader);
                let back = reader.path();
                let full = fed_order_path(&all, &reader.fed());
                assert_eq!(fed_of(&back), fed_of(&full), "{what}: fed round-trips");
                assert_unchanged(&sent, &full, &what);
                // `path.base` is fixed at the first send, which may not yet
                // reveal the working directory. The head follows start order.
                let mut full = full;
                let Some(JsonlLine::PathOpen(open)) = sent.first() else {
                    panic!("{what}: no PathOpen first");
                };
                full.path.base = open.base.clone();
                assert_eq!(by_id(&back), by_id(&full), "{what}");
                assert_start_order_head_and_totals(&back, &one_shot, &what);
                if order.is_sorted() {
                    assert_same(&back, &one_shot, &what);
                }
            }
        }
    }
}

#[test]
fn requests_that_mix_sessions_are_an_error() {
    let mut all = deliveries("claude-code.ndjson");
    all.extend(deliveries("codex.ndjson"));
    let batch = stored(&all);
    let err = derive_jsonl(
        &batch.records,
        |h| batch.messages.get(h),
        &classified(),
        &Remote::default(),
        false,
        0,
    )
    .unwrap_err();
    assert!(matches!(err, OtelError::MixedSessions(ids) if ids.len() == 2));
}

#[test]
fn a_session_without_an_id_keeps_its_first_fed_key() {
    // pi sends no session id: the key comes from the first fed generation,
    // so the path id holds whatever arrives later.
    let all = deliveries("pi.ndjson");
    let mut reader = Reader::default();
    let rev: Vec<usize> = (0..all.len()).rev().collect();
    arrive_requests(&all, &rev, 1, &mut reader);
    let back = reader.path();
    let first = &reader.fed()[0];
    let (s, _) = read_session(&all, &classified()).unwrap();
    assert!(s.session_id.is_none());
    let g = s.generations.iter().find(|g| &g.id == first).unwrap();
    assert_eq!(
        back.meta.unwrap().extra["otel"]["session_key"],
        json!(derived_key(g))
    );
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments.to_string()}})
}

/// Each arrival order only appends, reads back to the one-shot derivation
/// of its feed order, and in start order equals the one-shot path.
fn assert_every_order_appends(gens: &[Generation]) {
    let one_shot = derive_path(&session(gens.to_vec()));
    let all: Vec<usize> = (0..gens.len()).collect();
    for (n, order) in permutations(&all).into_iter().enumerate() {
        for batch in [1, 2] {
            let (hint, max) = (HINTS[(n + batch) % 3], [0, 1][n % 2]);
            let what = format!("{order:?}/{batch}/{hint:?}/{max}");
            let mut reader = Reader::new(hint, max);
            let sent = arrive(gens, &order, batch, &mut reader);
            assert_union(&reader, &sent, gens, &one_shot, &what);
            if order.is_sorted() {
                assert_same(&reader.path(), &one_shot, &what);
            }
        }
    }
}

/// pi's core tools, then a tool pi does not have: whole-session inference
/// would say `unknown`, but the harness is the one the generations that
/// settle the first turn show, and it stays for the rest of the session.
fn pi_then_other_tools() -> Vec<Generation> {
    let t1 = json!([call("t1", "read", json!({"path": "a"}))]);
    let t2 = json!([call("t2", "bash", json!({"command": "ls"}))]);
    let t3 = json!([call("t3", "my_subtask", json!({}))]);
    let turn = |calls: &Value, id: &str, out: &str| {
        [
            json!({"role": "assistant", "content": null, "tool_calls": calls}),
            json!({"role": "tool", "tool_call_id": id, "content": out}),
        ]
    };
    let mut prompt = vec![system("S"), user("go")];
    let mut gens = Vec::new();
    for (i, (calls, id)) in [(&t1, "t1"), (&t2, "t2"), (&t3, "t3")]
        .into_iter()
        .enumerate()
    {
        let mut g = billed(
            &format!("g{i}"),
            i as u64 + 1,
            Value::Array(prompt.clone()),
            "",
        );
        g.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
        gens.push(g);
        prompt.extend(turn(calls, id, "out"));
    }
    gens.push(billed("g3", 4, Value::Array(prompt), "done"));
    gens
}

#[test]
fn the_harness_is_decided_when_the_first_turn_settles() {
    use crate::harness::{infer_harness, signals};
    let gens = pi_then_other_tools();
    let whole = session(gens.clone());
    assert_eq!(infer_harness(&signals(&whole)), SourceHarness::Unknown);
    assert_eq!(
        first_settle(&whole, true, Some(&classifier())),
        (2, SourceHarness::Pi)
    );
    let one_shot = derive_path(&whole);
    let meta = &one_shot.meta.as_ref().unwrap().extra;
    assert_eq!(meta["otel"]["harness"], "pi");
    assert_eq!(meta["producer"]["name"], "pi");
    let category = one_shot
        .steps
        .iter()
        .flat_map(|s| s.change.values())
        .filter_map(|c| c.structural.as_ref())
        .flat_map(|s| s.extra.get("tool_uses").and_then(Value::as_array))
        .flatten()
        .find(|u| u["name"] == "my_subtask")
        .map(|u| u["category"].clone());
    assert_eq!(
        category,
        Some(value(&toolpath_convo::ToolCategory::Delegation)),
        "pi's categories, not the fallback's"
    );
    // In true order every send keeps the harness the first settled send had.
    let mut reader = Reader::default();
    for k in 1..=gens.len() {
        reader.send(&session(gens[..k].to_vec()), k == gens.len());
        if !reader.stored().is_empty() {
            let meta = reader.path().meta.unwrap().extra;
            assert_eq!(meta["otel"]["harness"], "pi", "after {k}");
        }
    }
    assert_every_order_appends(&gens);
}

/// Two sub-agents started with the same prompt, under one delegating
/// turn: the first thread in feed order takes the first call.
#[test]
fn duplicate_delegation_prompts_only_append_in_any_order() {
    let agent = |id: &str| call(id, "Agent", json!({"description": "d", "prompt": "same"}));
    let calls = json!([agent("c1"), agent("c2")]);
    let mut m0 = billed("m0", 10, json!([system("MAIN"), user("do it")]), "");
    m0.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
    let sub = |id: &str, start, ctx: &str, out: &str| {
        let first = json!({"role": "user", "content": [
            {"type": "text", "text": format!("<system-reminder>{ctx}</system-reminder>")},
            {"type": "text", "text": "same"}]});
        billed(id, start, json!([system("SUB"), first]), out)
    };
    let m1 = billed(
        "m1",
        30,
        json!([
            system("MAIN"),
            user("do it"),
            {"role": "assistant", "content": null, "tool_calls": calls},
            {"role": "tool", "tool_call_id": "c1", "content": "X done"},
            {"role": "tool", "tool_call_id": "c2", "content": "Y done"}
        ]),
        "all done",
    );
    let gens = vec![
        m0,
        sub("gx", 20, "x", "X done"),
        sub("gy", 21, "y", "Y done"),
        m1,
    ];
    assert_every_order_appends(&gens);
}

/// A sub-agent answers (a1), the main line receives it (m1), the sub-agent
/// is resumed (a2, its prompt echoing the answer) and answers again, which
/// the main line gets as a notification (m2). With `reasoning`, a2 echoes
/// the answer with reasoning the completion lacked, as OpenRouter returns
/// it: the stitch records an echo on the answer.
fn answering_twice(reasoning: bool) -> Vec<Generation> {
    let c1 = json!([call(
        "c1",
        "Agent",
        json!({"description": "d", "prompt": "sub A"})
    )]);
    let anchor = json!({"role": "user", "content": [
        {"type": "text", "text": "<system-reminder>ctx</system-reminder>"},
        {"type": "text", "text": "sub A"}]});
    let mut main = vec![system("MAIN"), user("do it")];
    let mut m0 = billed("m0", 10, Value::Array(main.clone()), "");
    m0.completion.tool_calls = serde_json::from_value(c1.clone()).unwrap();
    let a1 = billed("a1", 20, json!([system("SUB"), anchor]), "first answer");
    main.push(json!({"role": "assistant", "content": null, "tool_calls": c1}));
    main.push(json!({"role": "tool", "tool_call_id": "c1", "content": "first answer"}));
    let m1 = billed("m1", 30, Value::Array(main.clone()), "noted");
    let echo = if reasoning {
        json!({"role": "assistant", "content": "first answer",
               "reasoning_details": [{"type": "reasoning.text", "text": "thought"}]})
    } else {
        assistant("first answer")
    };
    let a2 = billed(
        "a2",
        35,
        json!([system("SUB"), anchor, echo, user("continue")]),
        "second answer",
    );
    main.push(assistant("noted"));
    main.push(user("<task-notification>second answer</task-notification>"));
    let m2 = billed("m2", 40, Value::Array(main), "done");
    vec![m0, a1, m1, a2, m2]
}

/// A sub-agent answers, is resumed and answers again: it merges where its
/// first answer is received, in every arrival order.
#[test]
fn a_sub_agent_answering_twice_only_appends_in_any_order() {
    let gens = answering_twice(false);
    let one_shot = derive_path(&session(gens.clone()));
    let merged: Vec<&toolpath::v1::Step> = one_shot
        .steps
        .iter()
        .filter(|s| s.step.parents.len() == 2)
        .collect();
    assert_eq!(merged.len(), 1, "one merge");
    assert_eq!(
        first_generation(&one_shot, &merged[0].step.parents[1]),
        "a1",
        "the first answer is the merge source"
    );
    assert_every_order_appends(&gens);
}

/// The id and `extra.otel` of the step generation `gid` produced.
fn produced<'a>(
    steps: impl IntoIterator<Item = &'a toolpath::v1::Step>,
    gid: &str,
) -> Option<(String, Value)> {
    steps.into_iter().find_map(|s| {
        let o = s
            .change
            .values()
            .find_map(|c| c.structural.as_ref()?.extra.get("otel"))?;
        (o["generation_id"] == gid).then(|| (s.step.id.clone(), o.clone()))
    })
}

fn sent_steps(lines: &[JsonlLine]) -> impl Iterator<Item = &toolpath::v1::Step> {
    lines.iter().filter_map(|l| match l {
        JsonlLine::Step(st) => Some(&st.0),
        _ => None,
    })
}

/// A merged answer goes out once its merge is seen (m1). A resumed
/// sub-agent (a2) whose request echoes the answer with reasoning the
/// completion lacked leaves the sent step as it is.
#[test]
fn a_resumed_sub_agent_never_changes_a_sent_answer() {
    let gens = answering_twice(true);
    let one_shot = derive_path(&session(gens.clone()));
    let (_, extra) = produced(&one_shot.steps, "a1").unwrap();
    assert!(extra.get("echo").is_none(), "a2 starts after m1");
    for (n, hint) in HINTS.into_iter().enumerate() {
        let max = [0, 1][n % 2];
        let what = format!("{hint:?}/{max}");
        let mut reader = Reader::new(hint, max);
        let mut sent = Vec::new();
        for k in 1..=gens.len() {
            sent.extend(reader.send(&session(gens[..k].to_vec()), false));
            if k == 3 {
                assert!(
                    produced(sent_steps(&sent), "a1").is_some(),
                    "{what}: sent at m1"
                );
            }
        }
        sent.extend(reader.send(&session(gens.clone()), true));
        let (_, extra) = produced(&reader.path().steps, "a1").unwrap();
        assert!(extra.get("echo").is_none(), "{what}");
        assert_union(&reader, &sent, &gens, &one_shot, &what);
    }
    // In orders where a2 is fed before m1 the echo settles the answer first
    // and stays; every order only appends.
    assert_every_order_appends(&gens);
}

fn first_generation(p: &Path, id: &str) -> String {
    let st = p.steps.iter().find(|s| s.step.id == id).unwrap();
    let c = st.change.values().next().unwrap();
    c.structural.as_ref().unwrap().extra["otel"]["first_generation_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The final review's reproduction: pi's `edit` (a file write) settles and
/// is sent; a later generation carries a request session id (whole-session
/// inference: claude-code, where `edit` is no file write). Every send keeps
/// pi, for every `stored` hint, whether or not the caller reads
/// `meta.otel.harness` back.
#[test]
fn a_later_generation_never_changes_the_harness_of_sent_steps() {
    use crate::harness::{infer_harness, signals};
    let sid = "3f2b6c1e-8a4d-4b2e-9c1a-0d5e6f7a8b9c";
    let edit = call(
        "c1",
        "edit",
        json!({"path": "a.txt", "oldText": "x", "newText": "y"}),
    );
    let mk = |id: &str, start: u64, msgs: Value| {
        let mut g = generation(id, start, msgs, "");
        g.session_id = Some(sid.into());
        g
    };
    let mut g1 = mk("g1", 1, json!([user("go")]));
    g1.completion.tool_calls = vec![serde_json::from_value(edit.clone()).unwrap()];
    let p2 = json!([
        user("go"),
        {"role": "assistant", "content": "", "tool_calls": [edit]},
        {"role": "tool", "tool_call_id": "c1", "content": "ok"}
    ]);
    let mut g2 = mk("g2", 2, p2.clone());
    g2.completion.text = "done".into();
    let mut p3 = p2.as_array().unwrap().clone();
    p3.push(assistant("done"));
    p3.push(user("more"));
    type Flip = fn(&mut Generation);
    let late: [(&str, Flip); 1] = [("request session id", |g| {
        g.request_session_id = Some("r".into())
    })];
    let s = |v: Vec<Generation>| Session::new(sid.into(), Some(sid.into()), v);
    for (what, flip) in late {
        let mut g3 = mk("g3", 3, Value::Array(p3.clone()));
        g3.completion.text = "fin".into();
        flip(&mut g3);
        let all = vec![g1.clone(), g2.clone(), g3];
        assert_ne!(infer_harness(&signals(&s(all.clone()))), SourceHarness::Pi);
        let one_shot = derive_path(&s(all.clone()));
        assert_eq!(
            one_shot.meta.as_ref().unwrap().extra["otel"]["harness"],
            "pi"
        );
        for hint in HINTS {
            for read_back in [true, false] {
                let what = format!("{what}/{hint:?}/{read_back}");
                let mut reader = Reader::new(hint, 0);
                let first = reader.send(&s(all[..2].to_vec()), false);
                assert!(!step_ids(&first).is_empty(), "{what}");
                let mut remote = reader.server.remote(hint);
                assert_eq!(remote.harness.as_deref(), Some("pi"), "{what}");
                if !read_back {
                    remote.harness = None;
                }
                let bodies = send(&s(all.clone()), &convo(), &remote, true, 0)
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                let rest = reader
                    .apply(bodies)
                    .unwrap_or_else(|e| panic!("{what}: {e:?}"));
                let mut sent = first.clone();
                sent.extend(rest);
                assert_unchanged(&sent, &one_shot, &what);
                assert_same(&reader.path(), &one_shot, &what);
            }
        }
    }
}

/// A stored harness is used as is; one this crate does not record is an
/// error.
#[test]
fn the_stored_harness_is_used_as_is() {
    let gens = pi_then_other_tools();
    let s = session(gens.clone());
    let remote = |h: &str| Remote {
        harness: Some(h.into()),
        ..Remote::default()
    };
    let bodies = send(&s, &convo(), &remote("claude-code"), true, 0).unwrap();
    let Some(JsonlLine::PathOpen(open)) = bodies[0].first() else {
        panic!("no PathOpen first");
    };
    let meta = open.meta.as_ref().unwrap();
    assert_eq!(meta.extra["otel"]["harness"], "claude-code");
    assert_eq!(meta.extra["producer"]["name"], "claude-code");
    let err = send(&s, &convo(), &remote("gemini-cli"), true, 0).unwrap_err();
    assert!(
        matches!(&err, OtelError::UnknownHarness(h) if h == "gemini-cli"),
        "{err:?}"
    );
}

/// One call reported twice (an app-side semconv span and OpenRouter's
/// Broadcast root share its `gen-…` id): the copy received first is
/// derived, and a copy received later never replaces it, whichever
/// profile ranks better.
#[test]
fn a_later_copy_never_replaces_a_fed_generation() {
    let g1 = billed("g1", 10, json!([system("S"), user("go")]), "a");
    let g2 = billed(
        "g2",
        20,
        json!([system("S"), user("go"), assistant("a"), user("more")]),
        "b",
    );
    let g3 = billed(
        "g3",
        30,
        json!([
            system("S"),
            user("go"),
            assistant("a"),
            user("more"),
            assistant("b"),
            user("again")
        ]),
        "c",
    );
    let copy = |profile: &str, start: u64, cost: f64| {
        let mut g = g2.clone();
        g.profile = profile.into();
        g.start_ns = start;
        g.cost.total = Some(cost);
        g
    };
    let (sem, or) = (
        copy("semconv", 20, 0.5),
        copy(crate::profile::openrouter::NAME, 21, 0.7),
    );
    for (first, later) in [(&sem, &or), (&or, &sem)] {
        for (n, hint) in HINTS.into_iter().enumerate() {
            let max = [0, 1][n % 2];
            let what = format!("{} first/{hint:?}/{max}", first.profile);
            let mut store = BTreeMap::new();
            let mut records = Vec::new();
            let mut reader = Reader::new(hint, max);
            let mut sent = Vec::new();
            for g in [&g1, first, &g3, later] {
                records.push(GenerationRecord::of(g.clone(), &mut store));
                sent.extend(reader.try_send_records(&records, &store, false).unwrap());
            }
            assert_eq!(reader.last_skipped.duplicate, 1, "{what}");
            sent.extend(reader.try_send_records(&records, &store, true).unwrap());
            let (_, extra) = produced(&reader.path().steps, "g2").unwrap();
            assert_eq!(extra["cost"]["total"], json!(first.cost.total), "{what}");
            let gens = [g1.clone(), first.clone(), g3.clone()];
            let full = derive_path(&in_fed_order(&gens, &reader.fed()));
            assert_unchanged(&sent, &full, &what);
        }
    }
}

/// A continuation takes any number of sends: the frozen ids ride in
/// `base` every time, so none is sent again or named by `Head`, and the
/// path opens once. A late fork off frozen history twice (its steps rank
/// below the frozen main line), then the main line, then `final_`.
#[test]
fn a_continuation_takes_consecutive_sends() {
    let g2 = billed("g2", 20, json!([system("S"), user("go")]), "b");
    let g3 = billed(
        "g3",
        30,
        json!([system("S"), user("go"), assistant("b"), user("again")]),
        "c",
    );
    let g4 = billed(
        "g4",
        40,
        json!([
            system("S"),
            user("go"),
            assistant("b"),
            user("again"),
            assistant("c"),
            user("more")
        ]),
        "d",
    );
    let f1 = billed("f1", 10, json!([system("S"), user("sub")]), "x");
    let f2 = billed(
        "f2",
        25,
        json!([system("S"), user("sub"), assistant("x"), user("sub more")]),
        "y",
    );
    let mut frozen = Reader::default();
    frozen.send(&session(vec![g2.clone(), g3.clone()]), true);
    let base = frozen.stored();
    let all = vec![f1.clone(), g2.clone(), g3.clone(), f2.clone(), g4.clone()];
    let one_shot = derive_path(&session(all.clone()));
    let steps = |p: &Path| {
        let mut v: Vec<Value> = p.steps.iter().map(value).collect();
        v.sort_by_key(|s| s["step"]["id"].as_str().unwrap().to_string());
        v
    };
    for hint in HINTS {
        for max in [0, 1] {
            let what = format!("{hint:?}/{max}");
            let mut cont = Reader::new(hint, max);
            cont.server.base = base.clone();
            cont.server.base_fed = frozen.fed();
            let mut sent = Vec::new();
            for (k, upto) in [3, 4, 5].into_iter().enumerate() {
                let lines = cont.send(&session(all[..upto].to_vec()), false);
                assert!(!step_ids(&lines).is_empty(), "{what}: send {k}");
                let opens = lines
                    .iter()
                    .filter(|l| matches!(l, JsonlLine::PathOpen(_)))
                    .count();
                assert_eq!(opens, usize::from(k == 0), "{what}: opened once");
                let head = head_of(&lines).unwrap();
                assert!(
                    !base.contains(&head),
                    "{what}: send {k} heads a frozen step"
                );
                sent.extend(lines);
            }
            sent.extend(cont.send(&session(all.clone()), true));
            assert!(
                step_ids(&sent).iter().all(|id| !base.contains(id)),
                "{what}: a frozen id is sent again"
            );
            let full = derive_path(&in_fed_order(&all, &cont.fed()));
            assert_unchanged(&sent, &full, &what);
            let mut union = frozen.path();
            union.steps.extend(cont.path().steps);
            assert_eq!(steps(&union), steps(&full), "{what}: frozen + continuation");
            assert_eq!(cont.path().path.head, one_shot.path.head, "{what}");
        }
    }
}

/// A frozen path of `g1` sent final, then a continuation's first (final)
/// send with `g2`, which carries a request session id (whole-feed
/// inference: claude-code). The continuation's `meta.otel.harness`, with
/// the frozen path's harness passed back as `Remote::harness` or not.
fn continuation_harness(g1: Generation, g2: Generation) -> Vec<(bool, String)> {
    let sid = "3f2b6c1e-8a4d-4b2e-9c1a-0d5e6f7a8b9c";
    let s = |v: Vec<Generation>| {
        let v = v
            .into_iter()
            .map(|mut g| {
                g.session_id = Some(sid.into());
                g
            })
            .collect();
        Session::new(sid.into(), Some(sid.into()), v)
    };
    let mut frozen = Reader::default();
    frozen.send(&s(vec![g1.clone()]), true);
    let frozen_harness = frozen.server.remote(Hint::Exact).harness.unwrap();
    [true, false]
        .into_iter()
        .map(|read_back| {
            let mut cont = Reader::default();
            cont.server.base = frozen.stored();
            cont.server.base_fed = frozen.fed();
            let mut remote = cont.server.remote(Hint::Exact);
            remote.harness = read_back.then(|| frozen_harness.clone());
            let bodies = send(&s(vec![g1.clone(), g2.clone()]), &convo(), &remote, true, 0)
                .unwrap_or_else(|e| panic!("{read_back}: {e}"));
            assert!(!step_ids(&bodies.concat()).is_empty(), "{read_back}");
            cont.apply(bodies).unwrap();
            let meta = cont.path().meta.unwrap().extra;
            (
                read_back,
                meta["otel"]["harness"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The frozen path settled as pi (its only call uses a pi tool, no
/// request session id) by a final send before any turn settled; the
/// continuation, a new request, says claude-code. Its steps keep pi, so
/// the session's paths never contradict each other.
#[test]
fn a_continuation_keeps_a_known_frozen_harness() {
    let read = call("c1", "read", json!({"path": "a.txt"}));
    let mut g1 = generation("g1", 1, json!([user("go")]), "");
    g1.completion.tool_calls = vec![serde_json::from_value(read).unwrap()];
    let mut g2 = generation("g2", 2, json!([user("other")]), "done");
    g2.request_session_id = Some("r".into());
    for (read_back, harness) in continuation_harness(g1, g2) {
        assert_eq!(harness, "pi", "read back: {read_back}");
    }
}

/// The frozen path's harness was `unknown`; the continuation's request
/// session id refines it to claude-code.
#[test]
fn a_continuation_refines_an_unknown_frozen_harness() {
    let g1 = generation("g1", 1, json!([user("go")]), "a");
    let mut g2 = generation(
        "g2",
        2,
        json!([user("go"), assistant("a"), user("more")]),
        "b",
    );
    g2.request_session_id = Some("r".into());
    for (read_back, harness) in continuation_harness(g1, g2) {
        assert_eq!(harness, "claude-code", "read back: {read_back}");
    }
}

mod property;
