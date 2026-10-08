//! The send property over every fixture session and many feed schedules:
//! whatever the arrival order, repeats and duplicate records, the union of
//! the sends is the feed-order derivation, no sent step ever changes, the
//! head follows start order and the token totals are the session's.

use super::*;

/// One event of a stream: a record arrives, or the caller sends.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ev {
    Arrive(usize),
    Send,
}

/// Schedules over `n` items, by name. Every one ends with a final send.
fn schedules(n: usize) -> Vec<(String, Vec<Ev>)> {
    let each = |order: &[usize], batch: usize, repeat: bool| {
        let mut ev = Vec::new();
        for chunk in order.chunks(batch) {
            ev.extend(chunk.iter().map(|&i| Ev::Arrive(i)));
            ev.push(Ev::Send);
            if repeat {
                ev.push(Ev::Send);
            }
        }
        ev
    };
    let ordered: Vec<usize> = (0..n).collect();
    let reversed: Vec<usize> = (0..n).rev().collect();
    let interleaved: Vec<usize> = (0..n)
        .map(|i| if i % 2 == 0 { i / 2 } else { n - 1 - i / 2 })
        .collect();
    let evens_odds: Vec<usize> = (0..n).step_by(2).chain((1..n).step_by(2)).collect();
    let mut out = vec![
        ("ordered/1".to_string(), each(&ordered, 1, false)),
        ("ordered/2".into(), each(&ordered, 2, false)),
        ("ordered/3".into(), each(&ordered, 3, false)),
        ("ordered/repeat".into(), each(&ordered, 1, true)),
        ("reversed".into(), each(&reversed, 1, false)),
        ("interleaved".into(), each(&interleaved, 1, false)),
        ("evens-odds/2".into(), each(&evens_odds, 2, false)),
    ];
    // Every record twice in a row, and the first again at the end.
    let twice: Vec<usize> = (0..n).flat_map(|i| [i, i]).chain([0]).collect();
    out.push(("duplicates".into(), each(&twice, 1, false)));
    // One record first (a delta before its target), one record last.
    for i in 0..n {
        let mut first = vec![i];
        first.extend((0..n).filter(|&j| j != i));
        out.push((format!("{i}-first"), each(&first, 1, false)));
        let mut last: Vec<usize> = (0..n).filter(|&j| j != i).collect();
        last.push(i);
        out.push((format!("{i}-last"), each(&last, 1, false)));
    }
    out
}

/// Σ of every `token_usage` class over the steps' conversation changes.
fn token_totals(p: &Path) -> BTreeMap<String, u64> {
    let mut sum = BTreeMap::new();
    for st in &p.steps {
        for c in st.change.values() {
            let Some(s) = &c.structural else { continue };
            let v = value(s);
            let Some(Value::Object(u)) = v.get("token_usage") else {
                continue;
            };
            for (k, n) in u {
                if let Some(n) = n.as_u64() {
                    *sum.entry(k.clone()).or_default() += n;
                }
            }
        }
    }
    sum
}

/// Σ of every generation's own `token_usage`.
fn session_totals(s: &Session) -> BTreeMap<String, u64> {
    let mut sum = BTreeMap::new();
    for g in &s.generations {
        let Some(u) = crate::provider::token_usage(&g.usage) else {
            continue;
        };
        if let Value::Object(u) = value(&u) {
            for (k, n) in u {
                if let Some(n) = n.as_u64() {
                    *sum.entry(k).or_default() += n;
                }
            }
        }
    }
    sum
}

/// Remembers every step payload sent and reports a second, different one.
#[derive(Default)]
struct Sent(HashMap<String, Value>);

impl Sent {
    fn record(&mut self, lines: &[JsonlLine], what: &str) {
        for line in lines {
            if let JsonlLine::Step(st) = line {
                let v = value(&st.0);
                match self.0.get(&st.0.step.id) {
                    Some(old) => assert_eq!(
                        *old, v,
                        "{what}: {} sent twice with different content",
                        st.0.step.id
                    ),
                    None => {
                        self.0.insert(st.0.step.id.clone(), v);
                    }
                }
            }
        }
    }
}

fn records_session(
    records: &[GenerationRecord],
    messages: &BTreeMap<MessageHash, StoredMessage>,
    fed: &[String],
) -> Session {
    let (mut s, _) = crate::session_from_records(
        records,
        |h| messages.get(h),
        &crate::tests::otel::classified(),
        crate::record::Pick::Arrival,
    )
    .unwrap();
    if s.session_id.is_none()
        && let Some(g) = s.generations.iter().find(|g| Some(&g.id) == fed.first())
    {
        s.key = derived_key(g);
    }
    in_feed_order(&s, fed).unwrap().0.into_owned()
}

/// The checks every finished stream must pass. `fed_full` is the
/// feed-order session, `start` the one-shot path in start order.
fn assert_stream(
    reader: &Reader,
    sent: &[JsonlLine],
    fed_full: &Session,
    one_shot: &Path,
    in_start_order: bool,
    what: &str,
) {
    let back = reader.path();
    let mut full = derive_path(fed_full);
    assert_eq!(fed_of(&back), fed_of(&full), "{what}: fed round-trips");
    assert_unchanged(sent, &full, what);
    if let Some(JsonlLine::PathOpen(open)) = sent.first() {
        full.path.base = open.base.clone();
    }
    assert_eq!(by_id(&back), by_id(&full), "{what}: union");
    for g in &fed_full.generations {
        assert_eq!(
            carries_record(&back, g),
            carries_record(one_shot, g),
            "{what}: record {}",
            g.id
        );
    }
    assert_start_order_head_and_totals(&back, one_shot, what);
    assert_eq!(
        token_totals(&back),
        session_totals(fed_full),
        "{what}: Σ token_usage"
    );
    assert_eq!(
        token_totals(&back),
        token_totals(one_shot),
        "{what}: Σ token_usage, one-shot"
    );
    if in_start_order {
        assert_same(&back, one_shot, &format!("{what}: start order"));
    }
}

fn fixture_files() -> Vec<String> {
    let mut files: Vec<String> = CONVERSATIONS.iter().map(|f| f.to_string()).collect();
    files.extend(CONVERSATIONS.iter().map(|f| format!("../equivalence/{f}")));
    files
}

/// Records (one per generation, in delivery order) of every fixture
/// session, fed through the public API under every schedule.
#[test]
fn every_fixture_schedule_sends_the_feed_order_path_once() {
    for file in fixture_files() {
        let all = deliveries(&file);
        let batch = stored(&all);
        let one_shot = crate::derive_path(&all, &crate::tests::otel::classified())
            .unwrap()
            .output;
        let n = batch.records.len();
        for (s, (name, events)) in schedules(n).into_iter().enumerate() {
            for (hint, max) in [(HINTS[s % 3], 0), (HINTS[(s + 1) % 3], 2)] {
                let what = format!("{file} {name}/{hint:?}/{max}");
                let mut reader = Reader::new(hint, max);
                let mut arrived: Vec<GenerationRecord> = Vec::new();
                let mut order: Vec<usize> = Vec::new();
                let mut sent = Vec::new();
                let mut seen = Sent::default();
                let finish = [Ev::Send];
                let last = events.len();
                for (k, ev) in events.iter().chain(&finish).enumerate() {
                    match *ev {
                        Ev::Arrive(i) => {
                            arrived.push(batch.records[i].clone());
                            order.push(i);
                        }
                        Ev::Send => {
                            let final_ = k == last;
                            let lines =
                                match reader.try_send_records(&arrived, &batch.messages, final_) {
                                    Ok(l) => l,
                                    Err(OtelError::NoGenerations { .. }) => Vec::new(),
                                    Err(e) => panic!("{what}: send {k}: {e}"),
                                };
                            seen.record(&lines, &what);
                            sent.extend(lines);
                        }
                    }
                }
                let fed = reader.fed();
                let fed_full = records_session(&arrived, &batch.messages, &fed);
                let mut firsts: Vec<usize> = Vec::new();
                for i in order {
                    if !firsts.contains(&i) {
                        firsts.push(i);
                    }
                }
                let in_start_order = firsts.is_sorted()
                    && fed_full
                        .generations
                        .is_sorted_by_key(|g| (g.start_ns, g.id.clone()));
                assert_stream(&reader, &sent, &fed_full, &one_shot, in_start_order, &what);
                let again = reader
                    .try_send_records(&arrived, &batch.messages, true)
                    .unwrap_or_else(|e| panic!("{what}: final again: {e}"));
                assert!(
                    hint != Hint::Exact || step_ids(&again).is_empty(),
                    "{what}: final again sent steps"
                );
            }
        }
    }
}

fn synthetic() -> Vec<(&'static str, Vec<Generation>)> {
    vec![
        ("chained", chained_generations()),
        ("delegating", delegating_generations()),
        ("answering-twice", answering_twice(false)),
        ("answering-twice-reasoning", answering_twice(true)),
        ("pi-then-other-tools", pi_then_other_tools()),
        ("mid-session", mid_session()),
        ("shared-sub-system", shared_sub_system()),
        ("mid-session-races-a-main-line", mid_session_racing()),
    ]
}

fn delta(id: &str, start: u64, text: &str, out: &str, continues: &str) -> Generation {
    let mut g = billed(id, start, json!([user(text)]), out);
    g.continues = Some(continues.into());
    g.history = serde_json::from_value(json!("delta")).unwrap();
    g
}

/// A capture that starts mid-session (its first delta continues a missing
/// target) with title requests between its turns.
fn mid_session() -> Vec<Generation> {
    vec![
        delta("g0", 10, "u0", "a0", "gX"),
        billed("t1", 15, json!([system("TITLE"), user("name it")]), "T1"),
        delta("g2", 20, "u1", "a1", "g0"),
        billed("t3", 25, json!([system("TITLE"), user("classify")]), "T2"),
        delta("g4", 30, "u2", "a2", "g2"),
    ]
}

/// A mid-session tree and a full-history conversation under `MAIN`, each
/// able to take the main line first depending on the feed order.
fn mid_session_racing() -> Vec<Generation> {
    vec![
        delta("g0", 10, "u0", "a0", "gX"),
        billed("m1", 12, json!([system("MAIN"), user("go")]), "b"),
        delta("g2", 20, "u1", "a1", "g0"),
        billed(
            "m3",
            22,
            json!([system("MAIN"), user("go"), assistant("b"), user("more")]),
            "c",
        ),
        billed("t4", 25, json!([system("TITLE"), user("name it")]), "T"),
    ]
}

/// A sub-agent and an unmatched request under one sub-agent system
/// prompt, either of which can be its first child.
fn shared_sub_system() -> Vec<Generation> {
    let c1 = json!([call(
        "c1",
        "Agent",
        json!({"description": "d", "prompt": "sub A"})
    )]);
    let mut m0 = billed("m0", 10, json!([system("MAIN"), user("do it")]), "");
    m0.completion.tool_calls = serde_json::from_value(c1.clone()).unwrap();
    let x1 = billed("x1", 15, json!([system("SUB"), user("unrelated")]), "x");
    let a1 = billed("a1", 20, json!([system("SUB"), user("sub A")]), "A done");
    let m1 = billed(
        "m1",
        30,
        json!([
            system("MAIN"),
            user("do it"),
            {"role": "assistant", "content": null, "tool_calls": c1},
            {"role": "tool", "tool_call_id": "c1", "content": "A done"}
        ]),
        "ok",
    );
    vec![m0, x1, a1, m1]
}

/// The synthetic sessions, generation by generation, under every schedule.
#[test]
fn every_synthetic_schedule_sends_the_feed_order_path_once() {
    for (label, gens) in synthetic() {
        let one_shot = derive_path(&session(gens.clone()));
        let mut all = schedules(gens.len());
        let idx: Vec<usize> = (0..gens.len()).collect();
        for order in permutations(&idx) {
            let mut ev: Vec<Ev> = order
                .iter()
                .flat_map(|&i| [Ev::Arrive(i), Ev::Send])
                .collect();
            ev.pop();
            all.push((format!("{order:?}"), ev));
        }
        for (s, (name, events)) in all.into_iter().enumerate() {
            for (hint, max) in [(HINTS[s % 3], 0), (HINTS[(s + 1) % 3], 2)] {
                let what = format!("{label} {name}/{hint:?}/{max}");
                let mut reader = Reader::new(hint, max);
                let mut arrived: Vec<Generation> = Vec::new();
                let mut sent = Vec::new();
                let mut seen = Sent::default();
                let finish = [Ev::Send];
                let last = events.len();
                for (k, ev) in events.iter().chain(&finish).enumerate() {
                    match *ev {
                        // A generation that arrives again is the same call:
                        // the session holds it once.
                        Ev::Arrive(i) => {
                            if !arrived.iter().any(|g| g.id == gens[i].id) {
                                arrived.push(gens[i].clone());
                            }
                        }
                        Ev::Send => {
                            if arrived.is_empty() {
                                continue;
                            }
                            let lines = reader
                                .try_send(&session(arrived.clone()), k == last)
                                .unwrap_or_else(|e| panic!("{what}: send {k}: {e:?}"));
                            seen.record(&lines, &what);
                            sent.extend(lines);
                        }
                    }
                }
                let fed_full = in_fed_order(&gens, &reader.fed());
                let in_start_order = arrived.is_sorted_by_key(|g| g.start_ns);
                assert_stream(&reader, &sent, &fed_full, &one_shot, in_start_order, &what);
                let again = reader
                    .try_send(&session(arrived.clone()), true)
                    .unwrap_or_else(|e| panic!("{what}: final again: {e:?}"));
                assert!(
                    hint != Hint::Exact || step_ids(&again).is_empty(),
                    "{what}: final again sent steps"
                );
            }
        }
    }
}
