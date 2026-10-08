//! The PR #315 review findings, re-checked through the record and JSONL
//! entry points: `read_generations`, `derive_path_from_records` and
//! `derive_jsonl`. Each test reproduces a finding's failure scenario
//! through records (JSON round-tripped) or incremental sends.

use super::*;
use crate::ProfileSelection;
use crate::generation::{History, Usage};
use crate::hash::canonical_json;
use crate::tests::common::fixtures_dir;
use crate::tests::otel::decode_input;
use std::path::PathBuf;

/// `gens` read by OpenRouter: a record names the profile that read it.
fn read_by_openrouter(mut gens: Vec<Generation>) -> Vec<Generation> {
    for g in &mut gens {
        g.profile = crate::profile::openrouter::NAME.into();
    }
    gens
}

/// The store's records for `gens`, in this order, through JSON.
fn records(gens: &[Generation]) -> GenerationBatch {
    let mut b = GenerationBatch::default();
    for g in read_by_openrouter(gens.to_vec()) {
        b.records.push(GenerationRecord::of(g, &mut b.messages));
    }
    through_json(&b)
}

fn through_json(b: &GenerationBatch) -> GenerationBatch {
    serde_json::from_str(&serde_json::to_string(b).unwrap()).unwrap()
}

fn cfg(profile: ProfileSelection) -> crate::DeriveConfig {
    crate::DeriveConfig {
        profile,
        ..crate::tests::otel::classified()
    }
}

fn from_records(b: &GenerationBatch, config: &crate::DeriveConfig) -> Path {
    crate::derive_path_from_records(&b.records, |h| b.messages.get(h), config)
        .unwrap()
        .output
}

/// One `Settle::Final` send to an empty store, read back.
fn final_jsonl(b: &GenerationBatch, config: &crate::DeriveConfig) -> Path {
    let d = derive_jsonl(
        &b.records,
        |h| b.messages.get(h),
        config,
        &Remote::default(),
        true,
        0,
    )
    .unwrap();
    let mut server = Server::default();
    for body in &d.output {
        server.apply(body).unwrap();
    }
    server.path()
}

/// Step changes and extras are hash maps: compare the key-sorted form.
fn bytes(p: &Path) -> String {
    canonical_json(&value(p))
}

/// `derive_path` of the session, `derive_path_from_records` and the
/// full-`Final` `derive_jsonl` read back all agree; returns the path.
fn three_ways(gens: &[Generation]) -> Path {
    let gens = &read_by_openrouter(gens.to_vec());
    let one_shot = derive_path(&session(gens.to_vec()));
    let b = records(gens);
    let rec = from_records(&b, &crate::tests::otel::classified());
    assert_eq!(bytes(&rec), bytes(&one_shot), "records");
    let sent = final_jsonl(&b, &crate::tests::otel::classified());
    assert_eq!(bytes(&sent), bytes(&one_shot), "derive_jsonl Final");
    one_shot
}

/// Like `assert_every_order_appends`, but every send goes through records
/// round-tripped through JSON and the public `derive_jsonl`.
fn assert_records_every_order_appends(gens: &[Generation]) {
    let gens = &read_by_openrouter(gens.to_vec());
    let one_shot = three_ways(gens);
    for (n, order) in arrivals(gens.len()).into_iter().enumerate() {
        for batch in [1, 2] {
            let (hint, max) = (HINTS[(n + batch) % 3], [0, 1][n % 2]);
            let what = format!("{order:?}/{batch}/{hint:?}/{max}");
            let mut reader = Reader::new(hint, max);
            let mut arrived = Vec::new();
            let mut sent = Vec::new();
            for chunk in order.chunks(batch) {
                arrived.extend(chunk.iter().map(|&i| gens[i].clone()));
                let b = records(&arrived);
                sent.extend(
                    reader
                        .try_send_records(&b.records, &b.messages, false)
                        .unwrap_or_else(|e| panic!("{what}: {e}")),
                );
            }
            let b = records(&arrived);
            sent.extend(
                reader
                    .try_send_records(&b.records, &b.messages, true)
                    .unwrap_or_else(|e| panic!("{what}: {e}")),
            );
            assert_union(&reader, &sent, gens, &one_shot, &what);
            if order.is_sorted() {
                assert_same(&reader.path(), &one_shot, &what);
            }
        }
    }
}

/// `extra.otel` of every conversation change of `p`'s steps, by step id.
fn otel_of(p: &Path) -> Vec<(String, Value)> {
    p.steps
        .iter()
        .filter_map(|s| {
            let o = s
                .change
                .values()
                .find_map(|c| c.structural.as_ref()?.extra.get("otel"))?;
            Some((s.step.id.clone(), o.clone()))
        })
        .collect()
}

/// Σ of one token class over every step's `token_usage`.
fn step_sum(p: &Path, class: &str) -> u64 {
    p.steps
        .iter()
        .flat_map(|s| s.change.values())
        .filter_map(|c| c.structural.as_ref()?.extra.get("token_usage"))
        .filter_map(|u| u.get(class)?.as_u64())
        .sum()
}

fn meta_otel(p: &Path) -> Value {
    p.meta.as_ref().unwrap().extra["otel"].clone()
}

// Finding 4187996122 (B3, an unplaced generation's tokens).
// ---------------------------------------------------------------------------

/// g2 and g3 repeat g1's answer to one prompt; g4 answers differently.
fn identical_retries() -> Vec<Generation> {
    let p = json!([system("S"), user("hi")]);
    vec![
        billed("g1", 1, p.clone(), "hello"),
        billed("g2", 2, p.clone(), "hello"),
        billed("g3", 3, p.clone(), "hello"),
        billed("g4", 4, p, "bye"),
    ]
}

#[test]
fn records_and_sends_put_every_retrys_tokens_on_a_step() {
    let gens = identical_retries();
    let want: u64 = gens.iter().map(|g| g.usage.input_tokens.unwrap()).sum();
    let p = three_ways(&gens);
    assert_eq!(step_sum(&p, "input_tokens"), want);
    let mut reader = Reader::default();
    for k in 1..=gens.len() {
        let b = records(&gens[..k]);
        reader
            .try_send_records(&b.records, &b.messages, k == gens.len())
            .unwrap();
    }
    assert_eq!(step_sum(&reader.path(), "input_tokens"), want, "streamed");
    assert_records_every_order_appends(&gens);
}

// Finding 4187996137 (B4, the main-line fallback moved marks).
// ---------------------------------------------------------------------------

/// A title request under its own system prompt starts before the main
/// line's first request.
fn title_first() -> Vec<Generation> {
    vec![
        billed("t0", 5, json!([system("TITLE"), user("name it")]), "Title"),
        billed("g0", 10, json!([system("MAIN"), user("go")]), "a0"),
        billed(
            "g1",
            20,
            json!([system("MAIN"), user("go"), assistant("a0"), user("more")]),
            "a1",
        ),
        billed(
            "g2",
            30,
            json!([
                system("MAIN"),
                user("go"),
                assistant("a0"),
                user("more"),
                assistant("a1"),
                user("again")
            ]),
            "a2",
        ),
    ]
}

#[test]
fn a_title_request_first_never_moves_a_sent_mark() {
    let gens = title_first();
    let p = three_ways(&gens);
    let (title, _) = produced(&p.steps, "t0").unwrap();
    let marks: HashMap<String, Value> = otel_of(&p).into_iter().collect();
    assert_eq!(marks[&title]["branch"], "side");
    assert_records_every_order_appends(&gens);
}

// Findings 4187996144 / 4187996156 (B5 shared sub-agent system turn, B6
// short answers merging at an unrelated turn).
// ---------------------------------------------------------------------------

fn agent(id: &str, prompt: &str) -> Value {
    call(id, "Agent", json!({"description": "d", "prompt": prompt}))
}

fn sub_prompt(p: &str) -> Value {
    json!({"role": "user", "content": [
        {"type": "text", "text": "<system-reminder>ctx</system-reminder>"},
        {"type": "text", "text": p}]})
}

/// Three parallel sub-agents under one system prompt, an unmatched thread
/// under the same system prompt, and the main line receiving the answers.
fn fan_out() -> Vec<Generation> {
    let calls = json!([
        agent("c1", "sub A"),
        agent("c2", "sub B"),
        agent("c3", "sub C")
    ]);
    let mut m0 = billed("m0", 10, json!([system("MAIN"), user("do it")]), "");
    m0.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
    let sub = |id: &str, start, p: &str, out: &str| {
        billed(id, start, json!([system("SUB"), sub_prompt(p)]), out)
    };
    let m1 = billed(
        "m1",
        30,
        json!([
            system("MAIN"),
            user("do it"),
            {"role": "assistant", "content": null, "tool_calls": calls},
            {"role": "tool", "tool_call_id": "c1", "content": "A done"},
            {"role": "tool", "tool_call_id": "c2", "content": "B done"},
            {"role": "tool", "tool_call_id": "c3", "content": "C done"}
        ]),
        "all done",
    );
    vec![
        m0,
        sub("g1", 20, "sub A", "A done"),
        sub("gu", 21, "unrelated", "U done"),
        sub("g2", 22, "sub B", "B done"),
        sub("g3", 23, "sub C", "C done"),
        m1,
    ]
}

#[test]
fn a_shared_sub_agent_system_turn_keeps_its_first_threads_mark_through_records() {
    let gens = fan_out();
    let p = three_ways(&gens);
    let marks: HashMap<String, Value> = otel_of(&p).into_iter().collect();
    let (unrelated, _) = produced(&p.steps, "gu").unwrap();
    assert_eq!(marks[&unrelated]["branch"], "side");
    let system = p
        .steps
        .iter()
        .find(|s| {
            s.step.parents.is_empty()
                && s.change.values().any(|c| {
                    c.structural
                        .as_ref()
                        .is_some_and(|st| st.extra.get("text") == Some(&json!("SUB")))
                })
        })
        .map(|s| s.step.id.clone());
    if let Some(system) = system {
        assert_ne!(marks[&system]["branch"], "side", "{:?}", marks[&system]);
    }
    assert_records_every_order_appends(&gens);
}

/// A background sub-agent answers "OK"; the main line says "OK, waiting"
/// before the notification that delivers the answer.
fn short_answer() -> Vec<Generation> {
    let calls = json!([agent("c1", "sub A")]);
    let mut m0 = billed("m0", 10, json!([system("MAIN"), user("do it")]), "");
    m0.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
    let mut main = vec![
        system("MAIN"),
        user("do it"),
        json!({"role": "assistant", "content": null, "tool_calls": calls}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "launched"}),
    ];
    let m1 = billed(
        "m1",
        20,
        Value::Array(main.clone()),
        "OK, waiting for agents",
    );
    let a = billed("a1", 15, json!([system("SUB"), sub_prompt("sub A")]), "OK");
    main.push(assistant("OK, waiting for agents"));
    main.push(user("<task-notification>OK</task-notification>"));
    let m2 = billed("m2", 30, Value::Array(main), "done");
    vec![m0, a, m1, m2]
}

#[test]
fn a_short_answer_merges_at_its_notification_through_records() {
    let gens = short_answer();
    let p = three_ways(&gens);
    let (answer, _) = produced(&p.steps, "a1").unwrap();
    let merge = p
        .steps
        .iter()
        .find(|s| s.step.parents.len() == 2 && s.step.parents[1] == answer)
        .expect("the answer merges");
    let text = value(merge).to_string();
    assert!(
        text.contains("task-notification"),
        "merged at {}",
        merge.step.id
    );
    assert_records_every_order_appends(&gens);
}

// Finding 4187996253 (B7, a delta whose continuation target is missing).
// ---------------------------------------------------------------------------

fn missing_continuation() -> Vec<Generation> {
    let delta = |id: &str, start, msgs: Value, out: &str, continues: &str| {
        let mut g = billed(id, start, msgs, out);
        g.history = History::Delta;
        g.continues = Some(continues.into());
        g
    };
    let tool = |c: &str| json!({"role": "tool", "tool_call_id": c, "content": "out"});
    let mut g0 = billed("g0", 10, json!([system("MAIN"), user("go")]), "");
    g0.completion.tool_calls =
        serde_json::from_value(json!([call("t0", "read", json!({}))])).unwrap();
    vec![
        g0,
        delta("g1", 20, json!([tool("t0")]), "a1", "g0"),
        delta("g2", 30, json!([tool("tx")]), "a2", "gX"),
        delta("g3", 40, json!([tool("ty")]), "a3", "g2"),
    ]
}

#[test]
fn a_missing_continuation_stays_on_the_main_line_through_records() {
    let gens = missing_continuation();
    let p = three_ways(&gens);
    assert!(
        otel_of(&p).iter().all(|(_, o)| o.get("branch").is_none()),
        "{:?}",
        otel_of(&p)
    );
    assert_eq!(p.path.head, produced(&p.steps, "g3").unwrap().0);
    assert_records_every_order_appends(&gens);
}

// Finding 4187996164 (B11, cwd markers of another harness).
// ---------------------------------------------------------------------------

#[test]
fn a_codex_session_reads_only_its_cwd_tag_through_records() {
    let sid = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    let opening = json!([
        {"role": "developer", "content": "dev"},
        user("# AGENTS.md\nWorking directory: /wrong"),
        user("<environment_context>\n  <cwd>/right</cwd>\n</environment_context>"),
        user("go")
    ]);
    let mut more = opening.as_array().unwrap().clone();
    more.extend([assistant("a"), user("more")]);
    let mut gens = vec![
        billed("g1", 1, opening, "a"),
        billed("g2", 2, Value::Array(more), "b"),
    ];
    for g in &mut gens {
        g.session_id = Some(sid.into());
    }
    let gens = read_by_openrouter(gens);
    let one_shot = derive_path(&Session::new(sid.into(), Some(sid.into()), gens.clone()));
    assert_eq!(meta_otel(&one_shot)["harness"], "codex");
    let b = records(&gens);
    let rec = from_records(&b, &crate::tests::otel::classified());
    let sent = final_jsonl(&b, &crate::tests::otel::classified());
    for (p, what) in [(&rec, "records"), (&sent, "derive_jsonl")] {
        let base = value(&p.path.base).to_string();
        assert!(
            base.contains("/right") && !base.contains("/wrong"),
            "{what}: {base}"
        );
        assert_eq!(bytes(p), bytes(&one_shot), "{what}");
    }
    // Streamed: the first send fixes `path.base`.
    let mut reader = Reader::default();
    for k in 1..=gens.len() {
        let b = records(&gens[..k]);
        reader
            .try_send_records(&b.records, &b.messages, k == gens.len())
            .unwrap();
    }
    assert_eq!(value(&reader.path().path.base), value(&one_shot.path.base));
}

// Findings 4187996174 / 4187996381 (NotebookEdit, MultiEdit, opencode
// delete).
// ---------------------------------------------------------------------------

/// One tool-calling generation and the one that echoes its calls.
fn tool_session(sid: &str, sys: &str, calls: Value) -> Vec<Generation> {
    let mut g1 = billed("g1", 1, json!([system(sys), user("edit")]), "");
    g1.completion.tool_calls = serde_json::from_value(calls.clone()).unwrap();
    let mut prompt = vec![
        system(sys),
        user("edit"),
        json!({"role": "assistant", "content": null, "tool_calls": calls}),
    ];
    for c in calls.as_array().unwrap() {
        prompt.push(json!({"role": "tool", "tool_call_id": c["id"], "content": "ok"}));
    }
    let g2 = billed("g2", 2, Value::Array(prompt), "done");
    let mut gens = vec![g1, g2];
    for g in &mut gens {
        g.session_id = Some(sid.into());
    }
    gens
}

fn three_ways_in(sid: &str, gens: &[Generation]) -> Path {
    let gens = &read_by_openrouter(gens.to_vec());
    let one_shot = derive_path(&Session::new(sid.into(), Some(sid.into()), gens.to_vec()));
    let b = records(gens);
    let rec = from_records(&b, &crate::tests::otel::classified());
    assert_eq!(bytes(&rec), bytes(&one_shot), "records");
    let sent = final_jsonl(&b, &crate::tests::otel::classified());
    assert_eq!(bytes(&sent), bytes(&one_shot), "derive_jsonl Final");
    one_shot
}

#[test]
fn notebook_and_multi_edit_changes_survive_records() {
    let calls = json!([
        call(
            "n",
            "NotebookEdit",
            json!({"notebook_path": "/w/a.ipynb", "cell_id": "c1", "new_source": "print(1)\n", "edit_mode": "replace"})
        ),
        call(
            "m",
            "MultiEdit",
            json!({"file_path": "/w/a.rs", "edits": [{"old_string": "x", "new_string": "y"}]})
        )
    ]);
    let sid = "cc-session";
    let p = three_ways_in(sid, &tool_session(sid, "You are Claude Code.", calls));
    assert_eq!(meta_otel(&p)["harness"], "claude-code");
    let changed: Vec<&String> = p.steps.iter().flat_map(|s| s.change.keys()).collect();
    assert!(
        changed.iter().any(|k| k.ends_with("/w/a.ipynb")),
        "{changed:?}"
    );
    let multi = p
        .steps
        .iter()
        .flat_map(|s| s.change.iter())
        .find(|(k, _)| k.ends_with("/w/a.rs"))
        .expect("MultiEdit change");
    assert!(value(multi.1).to_string().contains("\"edits\""));
}

#[test]
fn an_opencode_delete_survives_records() {
    let calls = json!([call("d", "delete", json!({"filePath": "/w/gone.txt"}))]);
    let sid = "ses_abc";
    let p = three_ways_in(sid, &tool_session(sid, "S", calls));
    assert_eq!(meta_otel(&p)["harness"], "opencode");
    let delete = p
        .steps
        .iter()
        .flat_map(|s| s.change.iter())
        .find(|(k, _)| k.ends_with("/w/gone.txt"))
        .expect("delete change");
    assert!(
        value(delete.1)
            .to_string()
            .contains(r#""operation":"delete""#),
        "{}",
        value(delete.1)
    );
}

// Finding 4187996239 (B14, a reasoning breakdown without an output count).
// ---------------------------------------------------------------------------

#[test]
fn no_reasoning_breakdown_without_an_output_count_through_records() {
    let mut g = billed("g1", 1, json!([system("S"), user("hi")]), "hello");
    g.usage = Usage {
        input_tokens: Some(10),
        reasoning_tokens: Some(5),
        ..Default::default()
    };
    let gens = vec![g];
    let p = three_ways(&gens);
    for s in &p.steps {
        for c in s.change.values() {
            if let Some(u) = c
                .structural
                .as_ref()
                .and_then(|st| st.extra.get("token_usage"))
            {
                assert!(
                    u["output_tokens"].is_u64() || u.get("breakdowns").is_none(),
                    "{u}"
                );
            }
        }
    }
}

// Finding 4187996246 (B9, the cluster key of an id-less session).
// ---------------------------------------------------------------------------

#[test]
fn same_opening_sessions_without_an_id_get_distinct_ids_through_records() {
    let pi = deliveries("pi.ndjson");
    let text = serde_json::to_string(&pi[0])
        .unwrap()
        .replace(r#""gen-"#, r#""gen-other-"#);
    let other: Value = serde_json::from_str(&text).unwrap();
    let ids = |requests: &[Value]| {
        let b = stored(requests);
        let config = crate::tests::otel::classified();
        (
            from_records(&b, &config).path.id,
            final_jsonl(&b, &config).path.id,
        )
    };
    let (a_rec, a_sent) = ids(&pi[..1]);
    let (b_rec, b_sent) = ids(std::slice::from_ref(&other));
    assert_ne!(a_rec, b_rec, "derive_path_from_records");
    assert_ne!(a_sent, b_sent, "derive_jsonl");
    assert_eq!(a_rec, a_sent);
    // A growing id-less session keeps its key.
    let whole = from_records(&stored(&pi), &crate::tests::otel::classified());
    assert_eq!(whole.path.id, a_rec);
}

// Finding 4187996276 (B8, id-less calls differing only in arguments).
// ---------------------------------------------------------------------------

#[test]
fn idless_calls_differing_in_arguments_are_two_turns_through_records() {
    let read = |path: &str| call("", "read_file", json!({"path": path}));
    let p = json!([system("S"), user("go")]);
    let mut g1 = billed("g1", 1, p.clone(), "");
    g1.completion.tool_calls = serde_json::from_value(json!([read("a")])).unwrap();
    let mut g2 = billed("g2", 2, p, "");
    g2.completion.tool_calls = serde_json::from_value(json!([read("b")])).unwrap();
    let g3 = billed(
        "g3",
        3,
        json!([
            system("S"),
            user("go"),
            {"role": "assistant", "content": null, "tool_calls": [read("b")]},
            {"role": "tool", "tool_call_id": "", "content": "B"}
        ]),
        "done",
    );
    let gens = vec![g1, g2, g3];
    let p = three_ways(&gens);
    let (a, _) = produced(&p.steps, "g1").unwrap();
    let (b, ob) = produced(&p.steps, "g2").unwrap();
    assert_ne!(a, b);
    assert!(ob.get("branch") != Some(&json!("unplaced")), "{ob}");
    assert_records_every_order_appends(&gens);
}

// Findings 4187996357 (one harness per derivation), 4187996367 (the
// walker's profile name), 4187996377 (no harness_hint).
// ---------------------------------------------------------------------------

#[test]
fn one_harness_drives_meta_producer_and_categories_through_records() {
    let gens = pi_then_other_tools();
    let p = three_ways(&gens);
    let meta = &p.meta.as_ref().unwrap().extra;
    assert_eq!(meta["otel"]["harness"], "pi");
    assert_eq!(meta["producer"]["name"], "pi");
    let mut reader = Reader::default();
    for k in 1..=gens.len() {
        let b = records(&gens[..k]);
        reader
            .try_send_records(&b.records, &b.messages, k == gens.len())
            .unwrap();
        if !reader.stored().is_empty() {
            let meta = reader.path().meta.unwrap().extra;
            assert_eq!(meta["otel"]["harness"], "pi", "after {k}");
            assert_eq!(meta["producer"]["name"], "pi", "after {k}");
        }
    }
    assert_records_every_order_appends(&gens);
}

#[test]
fn records_name_the_walkers_profile_and_no_harness_hint() {
    for file in CONVERSATIONS {
        let b = stored(&deliveries(file));
        let text = serde_json::to_string(&b).unwrap();
        assert!(!text.contains("harness_hint"), "{file}");
        for r in serde_json::to_value(&b).unwrap()["records"]
            .as_array()
            .unwrap()
        {
            let profile = r["generation"]["profile"]
                .as_str()
                .or(r["truncated"]["profile"].as_str());
            assert_eq!(profile, Some(crate::profile::openrouter::NAME), "{file}");
        }
    }
    let empty = json!({"format": 1, "generation": {"id": "g", "trace_id": "t",
        "start_ns": 1, "end_ns": 2, "session_id": null, "request_session_id": null,
        "user_id": null, "client_key": null, "completion": {"text": ""}, "usage": {},
        "cost": {}, "request_model": null, "response_model": null, "provider": null,
        "finish_reason": null, "profile": ""}});
    assert!(serde_json::from_value::<GenerationRecord>(empty).is_err());
}

// Equivalence: every fixture, three ways.
// ---------------------------------------------------------------------------

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn decode(p: &std::path::Path) -> Vec<Value> {
    decode_input(&std::fs::read(p).unwrap(), None).unwrap()
}

/// Every committed fixture input, with the profile it is read under.
fn fixture_inputs() -> Vec<(String, Vec<Value>, ProfileSelection)> {
    let mut out = Vec::new();
    for dir in [fixtures_dir(), fixture_root().join("equivalence")] {
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_str().unwrap();
                n.ends_with(".ndjson") || (n.ends_with(".json") && n != "expected.json")
            })
            .collect();
        names.sort();
        for p in names {
            out.push((p.display().to_string(), decode(&p), ProfileSelection::Auto));
        }
    }
    for name in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        for kind in ["span", "event"] {
            let dir = fixture_root().join(format!("semconv/{name}/{kind}"));
            let mut values = decode(&dir.join("traces.json"));
            if kind == "event" {
                values.extend(decode(&dir.join("logs.json")));
            }
            out.push((dir.display().to_string(), values, ProfileSelection::Auto));
        }
    }
    let cont = fixture_root().join("semconv/openai-responses/span-continuation/traces.json");
    out.push((
        cont.display().to_string(),
        decode(&cont),
        ProfileSelection::Auto,
    ));
    let dir = crate::tests::common::captures::openinference_dir();
    out.push((
        dir.display().to_string(),
        crate::tests::common::captures::traces_in(&dir),
        ProfileSelection::OpenInference,
    ));
    out
}

#[test]
fn derive_path_records_and_final_sends_agree_on_every_fixture() {
    let mut compared = 0;
    for (name, values, profile) in fixture_inputs() {
        let config = cfg(profile);
        let Ok(raw) = crate::derive_path(&values, &config) else {
            continue;
        };
        let read = crate::read_generations(&values, profile).unwrap();
        let b = through_json(&read.output);
        let rec = from_records(&b, &config);
        assert_eq!(bytes(&rec), bytes(&raw.output), "{name}: records");
        let sent = final_jsonl(&b, &config);
        assert_eq!(
            bytes(&sent),
            bytes(&raw.output),
            "{name}: derive_jsonl Final"
        );
        compared += 1;
    }
    assert!(compared >= 15, "only {compared} fixtures derived");
}
