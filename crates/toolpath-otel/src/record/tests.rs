use super::*;
use crate::hash::canonical_json;
use crate::profile::openrouter::NAME as OR;
use crate::tests::common::{deliveries, fixtures_dir};
use crate::tests::otel::classified;
use crate::tests::otel::decode_input;
use crate::{DeriveConfig, derive_path, derive_path_from_records};
use serde_json::json;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel")
}

fn file(rel: &str) -> Vec<Value> {
    decode_input(&std::fs::read(fixtures().join(rel)).unwrap(), None).unwrap()
}

fn files_in(dir: &std::path::Path) -> Vec<(String, Vec<Value>)> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            n.ends_with(".ndjson") || (n.ends_with(".json") && n != "expected.json")
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|p| {
            let values = decode_input(&std::fs::read(&p).unwrap(), None).unwrap();
            (p.display().to_string(), values)
        })
        .collect()
}

/// Every committed fixture input, with the profile it is read under.
fn inputs() -> Vec<(String, Vec<Value>, ProfileSelection)> {
    let mut out = Vec::new();
    for dir in [fixtures_dir(), fixtures().join("equivalence")] {
        for (name, values) in files_in(&dir) {
            out.push((name, values, ProfileSelection::Auto));
        }
    }
    for name in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        for kind in ["span", "event"] {
            let rel = format!("semconv/{name}/{kind}");
            let mut values = file(&format!("{rel}/traces.json"));
            if kind == "event" {
                values.extend(file(&format!("{rel}/logs.json")));
            }
            out.push((rel, values, ProfileSelection::Auto));
        }
    }
    out.push((
        "semconv/openai-responses/span-continuation".into(),
        file("semconv/openai-responses/span-continuation/traces.json"),
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

fn through_json(b: &GenerationBatch) -> GenerationBatch {
    serde_json::from_str(&serde_json::to_string(b).unwrap()).unwrap()
}

fn read(values: &[Value], profile: ProfileSelection) -> Derived<GenerationBatch> {
    read_generations(values, profile).unwrap()
}

/// What a store holds after reading `values` one delivery at a time.
fn per_delivery(values: &[Value], profile: ProfileSelection) -> (GenerationBatch, SkipCounts) {
    let mut all = GenerationBatch::default();
    let mut skipped = SkipCounts::default();
    for v in values {
        let d = read(std::slice::from_ref(v), profile);
        skipped.add(&d.skipped);
        let b = through_json(&d.output);
        all.records.extend(b.records);
        for (h, m) in b.messages {
            all.messages.entry(h).or_insert(m);
        }
    }
    (all, skipped)
}

fn from_records(
    b: &GenerationBatch,
    config: &DeriveConfig,
) -> crate::Result<Derived<toolpath::v1::Path>> {
    derive_path_from_records(&b.records, |h| b.messages.get(h), config)
}

/// Step changes and extras are hash maps: compare the key-sorted form.
fn bytes(p: &toolpath::v1::Path) -> String {
    canonical_json(&serde_json::to_value(p).unwrap())
}

#[test]
fn records_derive_what_the_bodies_do_on_every_fixture() {
    let mut compared = 0;
    for (name, values, profile) in inputs() {
        let config = DeriveConfig {
            profile,
            ..classified()
        };
        let raw = derive_path(&values, &config);
        let whole = read(&values, profile);
        let got = from_records(&through_json(&whole.output), &config);
        match (&raw, &got) {
            (Ok(raw), Ok(got)) => {
                assert_eq!(bytes(&got.output), bytes(&raw.output), "{name}");
                assert_eq!(whole.skipped, raw.skipped, "{name}");
                assert_eq!(got.skipped, SkipCounts::default(), "{name}");
                compared += 1;
            }
            (Err(a), Err(b)) => assert_eq!(
                std::mem::discriminant(a),
                std::mem::discriminant(b),
                "{name}"
            ),
            _ => panic!("{name}: {raw:?} vs {got:?}"),
        }
    }
    assert!(compared >= 15, "only {compared} fixtures derived");
}

#[test]
fn per_delivery_records_derive_what_the_bodies_do() {
    // Span fixtures send each call's trace whole in one delivery.
    for (name, values, profile) in inputs()
        .into_iter()
        .filter(|(n, ..)| !n.ends_with("/event"))
    {
        let config = DeriveConfig {
            profile,
            ..classified()
        };
        let (batch, skipped) = per_delivery(&values, profile);
        match (derive_path(&values, &config), from_records(&batch, &config)) {
            (Ok(raw), Ok(got)) => {
                assert_eq!(bytes(&got.output), bytes(&raw.output), "{name}");
                let mut sum = skipped;
                sum.add(&got.skipped);
                assert_eq!(sum.total(), raw.skipped.total(), "{name}");
            }
            (Err(a), Err(b)) => assert_eq!(
                std::mem::discriminant(&a),
                std::mem::discriminant(&b),
                "{name}"
            ),
            (raw, got) => panic!("{name}: {raw:?} vs {got:?}"),
        }
    }
}

#[test]
fn a_redelivered_call_is_one_generation_and_counted_once() {
    let mut values = deliveries("claude-code.ndjson");
    let config = classified();
    let one = derive_path(&values, &config).unwrap().output;
    values.push(values[1].clone());
    values.insert(0, values[2].clone());
    let (batch, _) = per_delivery(&values, ProfileSelection::Auto);
    let got = from_records(&batch, &config).unwrap();
    assert_eq!(bytes(&got.output), bytes(&one));
    assert_eq!(got.skipped.duplicate, 2);
}

fn gen_record(id: &str, profile: &str, text: &str) -> (GenerationRecord, StoredMessage) {
    let m = StoredMessage {
        parent: None,
        message: Message {
            role: "user".into(),
            content: json!(text),
            ..Default::default()
        },
    };
    let g = Generation {
        id: id.into(),
        trace_id: format!("t-{id}"),
        session_id: Some("s".into()),
        profile: profile.into(),
        ..Default::default()
    };
    let r = GenerationRecord {
        format: GenerationRecord::FORMAT,
        generation: Some(g),
        prompt: Some(m.hash()),
        truncated: None,
    };
    (r, m)
}

fn marker(id: &str, profile: &str) -> GenerationRecord {
    GenerationRecord {
        format: GenerationRecord::FORMAT,
        generation: None,
        prompt: None,
        truncated: Some(Truncated {
            generation_id: Some(id.into()),
            session_id: Some("s".into()),
            profile: profile.into(),
        }),
    }
}

fn session_for(
    records: &[GenerationRecord],
    msgs: &[StoredMessage],
    pick: Pick,
) -> (Session, SkipCounts) {
    let map: HashMap<MessageHash, &StoredMessage> = msgs.iter().map(|m| (m.hash(), m)).collect();
    let entries = rebuild(records, |h| map.get(h).copied()).unwrap();
    session_of(entries, ProfileSelection::Auto, pick, SkipCounts::default()).unwrap()
}

#[test]
fn by_rank_the_better_ranked_profile_wins_whatever_the_order() {
    let (sem, m1) = gen_record("g", "semconv", "from semconv");
    let (or, m2) = gen_record("g", OR, "from the gateway");
    for records in [[sem.clone(), or.clone()], [or.clone(), sem.clone()]] {
        let (s, skipped) = session_for(&records, &[m1.clone(), m2.clone()], Pick::Rank);
        assert_eq!(s.generations.len(), 1);
        assert_eq!(s.generations[0].profile, OR);
        assert_eq!(skipped.duplicate, 1);
    }
}

#[test]
fn in_arrival_order_the_first_copy_wins() {
    let (sem, m1) = gen_record("g", "semconv", "from semconv");
    let (or, m2) = gen_record("g", OR, "from the gateway");
    for (records, kept) in [
        ([sem.clone(), or.clone()], "semconv"),
        ([or.clone(), sem.clone()], OR),
    ] {
        let (s, skipped) = session_for(&records, &[m1.clone(), m2.clone()], Pick::Arrival);
        assert_eq!(s.generations.len(), 1);
        assert_eq!(s.generations[0].profile, kept);
        assert_eq!(skipped.duplicate, 1);
    }
}

#[test]
fn a_truncated_call_reads_as_a_marker_that_round_trips() {
    let good = deliveries("claude-code.ndjson");
    let mut cut = good[0].clone();
    let spans = cut["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array_mut()
        .unwrap();
    for a in spans[0]["attributes"].as_array_mut().unwrap() {
        // The prompt attribute: the one holding the request's messages.
        let prompt = a["value"]["stringValue"]
            .as_str()
            .is_some_and(|v| v.starts_with("{\"messages\""));
        if prompt {
            a["value"]["stringValue"] = json!("{\"messages\":[{\"role\"");
        }
    }
    // Without the call's good copy the marker marks the session; with it,
    // the marker is a duplicate.
    let config = classified();
    for (values, truncated) in [
        ([vec![cut.clone()], good[1..].to_vec()].concat(), true),
        ([vec![cut], good].concat(), false),
    ] {
        let raw = derive_path(&values, &config).unwrap();
        assert_eq!(raw.skipped.truncated, usize::from(truncated));
        // Read alone, the cut delivery has no kept copy to duplicate.
        let (batch, skipped) = per_delivery(&values, ProfileSelection::Auto);
        assert_eq!(skipped.truncated, 1);
        let marker = &batch.records[0];
        assert!(marker.is_truncated());
        assert_eq!(marker.start_ns(), None);
        assert!(marker.prompt().is_none());
        assert_eq!(marker.session_id(), batch.records[1].session_id());
        let delivered = batch.records[1..]
            .iter()
            .any(|r| r.generation_id() == marker.generation_id());
        assert_eq!(delivered, !truncated);
        let got = from_records(&batch, &config).unwrap();
        assert_eq!(bytes(&got.output), bytes(&raw.output));
        assert_eq!(
            got.output.meta.unwrap().extra["otel"]["truncated"],
            truncated
        );
    }
}

/// A truncated call without a session id still reads as a marker and
/// marks the session, as `derive_path` of the bodies does.
#[test]
fn a_truncated_call_without_a_session_id_marks_the_session() {
    let mut values = deliveries("claude-code.ndjson");
    crate::tests::common::without_session_ids(&mut values);
    let spans = values[0]["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array_mut()
        .unwrap();
    for a in spans[0]["attributes"].as_array_mut().unwrap() {
        let prompt = a["value"]["stringValue"]
            .as_str()
            .is_some_and(|v| v.starts_with("{\"messages\""));
        if prompt {
            a["value"]["stringValue"] = json!("{\"messages\":[{\"role\"");
        }
    }
    let config = classified();
    let raw = derive_path(&values, &config).unwrap();
    assert_eq!(raw.skipped.truncated, 1);
    assert_eq!(
        raw.output.meta.as_ref().unwrap().extra["otel"]["session_id"],
        Value::Null
    );
    let (batch, _) = per_delivery(&values, ProfileSelection::Auto);
    let marker = batch.records.iter().find(|r| r.is_truncated());
    assert!(
        marker.is_some_and(|m| m.session_id().is_none()),
        "an id-less marker"
    );
    let got = from_records(&batch, &config).unwrap();
    assert_eq!(
        got.output.meta.as_ref().unwrap().extra["otel"]["truncated"],
        true
    );
    assert_eq!(bytes(&got.output), bytes(&raw.output));
}

/// The prompt `r` names, first message first.
fn prompt_of<'b>(b: &'b GenerationBatch, r: &GenerationRecord) -> Vec<&'b StoredMessage> {
    let mut out = Vec::new();
    let mut next = r.prompt();
    while let Some(h) = next {
        let m = &b.messages[h];
        assert_eq!(m.hash(), *h);
        out.push(m);
        next = m.parent();
    }
    out.reverse();
    out
}

#[test]
fn a_record_names_its_prompt_by_its_last_message() {
    let values = deliveries("claude-code.ndjson");
    let b = read(&values, ProfileSelection::Auto).output;
    let whole = crate::walk::read_deliveries(&values, ProfileSelection::Auto)
        .unwrap()
        .generations;
    let r = &b.records[b.records.len() - 1];
    assert_eq!(r.format(), 1);
    assert!(!r.is_truncated());
    assert!(r.start_ns().is_some());
    assert!(r.generation_id().unwrap().starts_with("gen-"));
    let g = whole
        .iter()
        .find(|g| Some(g.id.as_str()) == r.generation_id())
        .unwrap();
    let prompt: Vec<&Message> = prompt_of(&b, r).into_iter().map(|m| &m.message).collect();
    assert_eq!(prompt, g.messages.iter().collect::<Vec<_>>());
    assert_eq!(prompt_of(&b, r)[0].parent(), None);
    let v = serde_json::to_value(r).unwrap();
    assert!(v["generation"].get("messages").is_none());
    assert_eq!(v["prompt"], json!(r.prompt().unwrap().as_str()));
    // Each call repeats the prompt before it: every prefix is stored once.
    let named: usize = b.records.iter().map(|r| prompt_of(&b, r).len()).sum();
    let longest = b
        .records
        .iter()
        .map(|r| prompt_of(&b, r).len())
        .max()
        .unwrap();
    assert!(b.messages.len() < named);
    assert!(
        b.messages.len() <= longest + b.records.len() * 4,
        "{}",
        b.messages.len()
    );
}

#[test]
fn the_message_hash_chains_jcs() {
    let first: StoredMessage = serde_json::from_value(json!({"message":
        {"role": "user", "content": [{"type": "text", "text": "hi"}]}}))
    .unwrap();
    // python3: sha256(b"toolpath-otel/message\0" + b"\0" +
    //   b'{"content":[{"text":"hi","type":"text"}],"role":"user"}').hexdigest()
    let h1 = "d63c9ba05cda35eb3901228c8a4fa1c6e98a2b4674e468e6efc3e965db981931";
    assert_eq!(first.hash().as_str(), h1);
    let second: StoredMessage = serde_json::from_value(json!({"parent": h1,
        "message": {"role": "assistant", "content": "ok"}}))
    .unwrap();
    // python3: sha256(b"toolpath-otel/message\0" + h1 + b"\0" +
    //   b'{"content":"ok","role":"assistant"}').hexdigest()
    assert_eq!(
        second.hash().as_str(),
        "b7ebd7754026ca8765f43c510ad7e68ede580a20e8bd7c2c289fa50b5da6cbed"
    );
    assert_eq!(second.parent(), Some(&first.hash()));
    assert!(
        serde_json::to_value(&first)
            .unwrap()
            .get("parent")
            .is_none()
    );
    assert_eq!(first.hash().to_string(), h1);
    let back: MessageHash = h1.parse().unwrap();
    assert_eq!(String::from(back), h1);

    // Where RFC 8785 differs from sorted compact JSON: UTF-16 key order,
    // ECMAScript number spellings, and escapes. Known answer from an
    // independent JCS serializer (python).
    let h2 = second.hash();
    let raw = r#"{"role":"user","content":[{"type":"text","text":"caf\u00e9 \u001f \ud83d\ude00","n":1.0,"m":1e2,"z":-0.0,"\u00e9":1,"a":0.000001,"b":1e21,"\ud83d\ude00":2,"\uff61":3}]}"#;
    let third = StoredMessage {
        parent: Some(h2),
        message: serde_json::from_str(raw).unwrap(),
    };
    assert_eq!(
        third.hash().as_str(),
        "df291fffbc32972a6778d3741b078d4077760ce019ef50905f9468a502391074"
    );
}

/// JCS spells a number by its value, so `1` and `1.0` (and `0` and `-0.0`)
/// give one message hash. A store keyed by hash keeps the first spelling
/// it saw; deriving from it can then differ from the raw bodies in that
/// spelling only.
#[test]
fn number_spellings_of_one_value_share_a_message_hash() {
    let msg = |content: &str| -> Message {
        serde_json::from_str(&format!(r#"{{"role":"user","content":{content}}}"#)).unwrap()
    };
    let mut store = BTreeMap::new();
    for (first, other) in [("[1]", "[1.0]"), ("[0]", "[-0.0]")] {
        let a = store_prompt(&[msg(first)], &mut store).unwrap();
        let b = store_prompt(&[msg(other)], &mut store).unwrap();
        assert_eq!(a, b, "{first} and {other}");
        assert_ne!(msg(first), msg(other), "the messages themselves differ");
        assert_eq!(store[&a].message, msg(first), "the first spelling stays");
    }
}

#[test]
fn a_chain_that_never_ends_is_an_error() {
    let (r, m) = gen_record("g", OR, "hi");
    let mut a = m.clone();
    a.message.content = json!("a");
    let mut b = m.clone();
    b.message.content = json!("b");
    let (ha, hb) = (MessageHash("a".repeat(64)), MessageHash("b".repeat(64)));
    a.parent = Some(hb.clone());
    b.parent = Some(ha.clone());
    let mut tail = m.clone();
    tail.parent = Some(ha.clone());
    let store: HashMap<MessageHash, StoredMessage> =
        [(m.hash(), tail), (ha, a), (hb, b)].into_iter().collect();
    let err = rebuild(std::slice::from_ref(&r), |h| store.get(h))
        .err()
        .unwrap();
    assert!(matches!(err, OtelError::MessageMissing(_)), "{err}");
}

#[test]
fn a_shared_prompt_reads_and_prints_as_its_prefix() {
    let all: Arc<[Message]> = (0..3)
        .map(|i| Message {
            role: "user".into(),
            content: json!(i),
            ..Default::default()
        })
        .collect::<Vec<_>>()
        .into();
    let mut p = Prompt::shared(all.clone(), 2);
    assert_eq!(p.len(), 2);
    assert_eq!(format!("{p:?}"), format!("{:?}", &all[..2]));
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        serde_json::to_value(&all[..2]).unwrap()
    );
    assert_eq!(p.pop().map(|m| m.content), Some(json!(1)));
    assert_eq!(p, Prompt::from(vec![all[0].clone()]));
    assert_eq!(p.into_iter().count(), 1);
}

#[test]
fn a_record_without_a_prompt_rebuilds_with_no_messages() {
    let (mut r, _) = gen_record("g", OR, "hi");
    r.prompt = None;
    let back: GenerationRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back.prompt(), None);
    let (s, _) = session_for(&[back], &[], Pick::Rank);
    assert!(s.generations[0].messages.is_empty());
}

#[test]
fn a_looping_message_store_is_an_error() {
    let (r, m) = gen_record("g", OR, "hi");
    let mut looped = m.clone();
    looped.parent = Some(m.hash());
    let err = rebuild(std::slice::from_ref(&r), |_| Some(&looped))
        .err()
        .unwrap();
    assert!(matches!(err, OtelError::MessageMissing(_)), "{err}");
}

#[test]
fn malformed_records_and_hashes_do_not_deserialize() {
    let (r, _) = gen_record("g", OR, "hi");
    let mut v = serde_json::to_value(&r).unwrap();
    let ok: GenerationRecord = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(ok, r);
    v["format"] = json!(2);
    assert!(serde_json::from_value::<GenerationRecord>(v.clone()).is_err());
    v["format"] = json!(0);
    assert!(serde_json::from_value::<GenerationRecord>(v.clone()).is_err());
    v["format"] = json!(1);
    v["generation"]["messages"] = json!([{"role": "user", "content": "inline"}]);
    v["prompt"] = Value::Null;
    let err = serde_json::from_value::<GenerationRecord>(v).unwrap_err();
    assert!(err.to_string().contains("by hash"), "{err}");
    let both = json!({"format": 1, "generation": {"id": "g", "trace_id": "t", "start_ns": 0,
        "end_ns": 0, "session_id": null, "request_session_id": null, "user_id": null,
        "client_key": null, "completion": {"text": "", "reasoning": null, "tool_calls": []},
        "usage": {}, "cost": {}, "request_model": null, "response_model": null,
        "provider": null, "finish_reason": null},
        "truncated": {"generation_id": "g", "session_id": "s", "profile": OR}});
    assert!(serde_json::from_value::<GenerationRecord>(both).is_err());
    assert!(serde_json::from_value::<GenerationRecord>(json!({"format": 1})).is_err());
    let mut v = serde_json::to_value(&r).unwrap();
    v["generation"]["profile"] = json!("");
    let err = serde_json::from_value::<GenerationRecord>(v).unwrap_err();
    assert!(err.to_string().contains("profile"), "{err}");
    let mut v = serde_json::to_value(marker("g", OR)).unwrap();
    v["prompt"] = json!(r.prompt().unwrap());
    let err = serde_json::from_value::<GenerationRecord>(v.clone()).unwrap_err();
    assert!(err.to_string().contains("marker"), "{err}");
    v.as_object_mut().unwrap().remove("prompt");
    v["truncated"]["profile"] = json!("");
    assert!(serde_json::from_value::<GenerationRecord>(v).is_err());
    for bad in ["ABC", "", &"g".repeat(64), &"A".repeat(64)] {
        assert!(bad.parse::<MessageHash>().is_err(), "{bad}");
        assert!(serde_json::from_value::<MessageHash>(json!(bad)).is_err());
    }
}

#[test]
fn a_missing_message_is_an_error() {
    let b = read(&deliveries("codex.ndjson"), ProfileSelection::Auto).output;
    let err = derive_path_from_records(&b.records, |_| None, &classified()).unwrap_err();
    assert!(
        matches!(&err, OtelError::MessageMissing(h) if h.len() == 64),
        "{err}"
    );
}

#[test]
fn reading_bodies_without_a_call_gives_no_records() {
    let d = read(&deliveries("connection-test.json"), ProfileSelection::Auto);
    assert!(d.output.records.is_empty());
    assert!(d.output.messages.is_empty());
    assert_eq!(d.skipped.connection_test, 1);
    let err = derive_path_from_records(&[], |_| None, &classified()).unwrap_err();
    assert!(matches!(err, OtelError::NoGenerations { .. }));
    assert!(matches!(
        read_generations(&[json!([1])], ProfileSelection::Auto),
        Err(OtelError::NotOtlp)
    ));
}

#[test]
fn records_of_two_sessions_read_together_and_mix_only_when_derived_together() {
    let mut values = deliveries("claude-code.ndjson");
    values.extend(deliveries("codex.ndjson"));
    let b = read(&values, ProfileSelection::Auto).output;
    let ids: BTreeSet<Option<&str>> = b.records.iter().map(|r| r.session_id()).collect();
    assert_eq!(ids.len(), 2);
    let err = from_records(&b, &classified()).unwrap_err();
    assert!(matches!(err, OtelError::MixedSessions(ids) if ids.len() == 2));
}

/// Every pointer into `v`: the root, each member, and each element.
fn pointers(v: &Value, at: String, out: &mut Vec<String>) {
    out.push(at.clone());
    match v {
        Value::Object(m) => m
            .iter()
            .for_each(|(k, x)| pointers(x, format!("{at}/{k}"), out)),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, x)| pointers(x, format!("{at}/{i}"), out)),
        _ => {}
    }
}

/// `v` with the value at `ptr` removed (`None`) or replaced.
fn mutated(v: &Value, ptr: &str, with: &Option<Value>) -> Option<Value> {
    let Some((parent, key)) = ptr.rsplit_once('/') else {
        return with.clone();
    };
    let mut v = v.clone();
    match (v.pointer_mut(parent)?, with) {
        (Value::Object(o), None) => {
            o.remove(key);
        }
        (Value::Object(o), Some(x)) => {
            o.insert(key.into(), x.clone());
        }
        (Value::Array(a), None) => {
            a.remove(key.parse().ok()?);
        }
        (Value::Array(a), Some(x)) => a[key.parse::<usize>().ok()?] = x.clone(),
        _ => return None,
    }
    Some(v)
}

/// Every field of a record and of a stored message, removed or replaced
/// by a value of another type, either fails to deserialize or derives (or
/// errors) without a panic.
#[test]
fn malformed_fields_are_rejected_or_derived_without_a_panic() {
    let batch = read(&deliveries("claude-code.ndjson"), ProfileSelection::Auto).output;
    let config = classified();
    let withs = [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!(-1)),
        Some(json!(1.5)),
        Some(json!(u64::MAX)),
        Some(json!(true)),
        Some(json!([])),
        Some(json!({})),
        Some(json!("a".repeat(64))),
    ];
    let mut cases = 0;
    let mut check = |v: &Value, derive: &dyn Fn(Value)| {
        let mut ptrs = Vec::new();
        pointers(v, String::new(), &mut ptrs);
        for ptr in &ptrs {
            for with in &withs {
                let Some(m) = mutated(v, ptr, with) else {
                    continue;
                };
                let run = std::panic::AssertUnwindSafe(|| derive(m));
                assert!(std::panic::catch_unwind(run).is_ok(), "{ptr} = {with:?}");
                cases += 1;
            }
        }
    };
    let last = batch.records.len() - 1;
    let record = serde_json::to_value(&batch.records[last]).unwrap();
    check(&record, &|m| {
        if let Ok(r) = serde_json::from_value::<GenerationRecord>(m) {
            let mut records = batch.records.clone();
            records[last] = r;
            let _ = from_records(
                &GenerationBatch {
                    records,
                    messages: batch.messages.clone(),
                },
                &config,
            );
        }
    });
    let (hash, message) = batch.messages.iter().next().unwrap();
    check(&serde_json::to_value(message).unwrap(), &|m| {
        if let Ok(stored) = serde_json::from_value::<StoredMessage>(m) {
            let mut messages = batch.messages.clone();
            messages.insert(hash.clone(), stored);
            let _ = derive_path_from_records(&batch.records, |h| messages.get(h), &config);
        }
    });
    assert!(cases > 500, "{cases}");
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// A session of prompts drawn from a few messages (system, id-less and
/// id'd calls, their results, an error result), so prompts share prefixes
/// at every depth; some generations are deltas, prompt-absent skeletons,
/// or redelivered copies, in no particular start order.
fn random_generations(rng: &mut Rng) -> Vec<Generation> {
    use crate::generation::{Completion, FunctionCall, History, ToolCall};
    let pool = [
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "u"}),
        json!({"role": "user", "content": "v"}),
        json!({"role": "assistant", "content": "a0"}),
        json!({"role": "assistant", "content": "a1",
            "tool_calls": [{"id": "", "function": {"name": "Bash", "arguments": "{}"}}]}),
        json!({"role": "assistant", "content": "a2",
            "tool_calls": [{"id": "c1", "function": {"name": "Bash", "arguments": "{}"}}]}),
        json!({"role": "tool", "content": "r"}),
        json!({"role": "tool", "content": "r", "tool_call_id": "c1"}),
        json!({"role": "tool", "content": "e", "tool_call_id": "c1", "is_error": true}),
        json!({"role": "developer", "content": "dev"}),
    ];
    let n = 1 + rng.below(10);
    let mut gens: Vec<Generation> = (0..n)
        .map(|i| {
            let messages: Vec<Message> = (0..rng.below(7))
                .map(|_| serde_json::from_value(pool[rng.below(pool.len())].clone()).unwrap())
                .collect();
            let tool_calls = (0..rng.below(3))
                .map(|k| ToolCall {
                    id: match rng.below(2) {
                        0 => String::new(),
                        _ => format!("c{}", rng.below(2) + k),
                    },
                    function: FunctionCall {
                        name: "Bash".into(),
                        arguments: json!("{}"),
                    },
                })
                .collect();
            let mut g = Generation {
                id: format!("g{i:02}"),
                trace_id: format!("t{}", rng.below(2)),
                start_ns: rng.below(5) as u64,
                end_ns: 10,
                profile: OR.into(),
                messages: messages.into(),
                completion: Completion {
                    text: format!("a{}", rng.below(3)),
                    tool_calls,
                    ..Default::default()
                },
                ..Default::default()
            };
            if i > 0 && rng.below(4) == 0 {
                g.continues = Some(format!("g{:02}", rng.below(n + 1)));
                g.history = History::Delta;
            }
            if rng.below(10) == 0 {
                g.absent.prompt = true;
                g.messages = Default::default();
            }
            g
        })
        .collect();
    let copies: Vec<Generation> = gens.iter().filter(|_| rng.below(5) == 0).cloned().collect();
    gens.extend(copies);
    gens
}

/// Records rebuilt from a message store, in any order, stitch by message
/// hash to what the generations themselves stitch to by content.
#[test]
fn random_sessions_derive_alike_from_records_and_from_generations() {
    let config = classified();
    let mut derived = 0;
    for seed in 1..300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
        let gens = random_generations(&mut rng);
        let entries = gens
            .iter()
            .map(|g| Entry::Generation(Box::new(g.clone()), None))
            .collect();
        let (session, _) = session_of(
            entries,
            ProfileSelection::Auto,
            Pick::Rank,
            SkipCounts::default(),
        )
        .unwrap();
        let want = bytes(&crate::derive::derive_session(
            &session,
            &config.convo,
            config.tool_category.as_ref(),
        ));
        let mut batch = GenerationBatch::default();
        batch.records = gens
            .into_iter()
            .map(|g| GenerationRecord::of(g, &mut batch.messages))
            .collect();
        for i in (1..batch.records.len()).rev() {
            batch.records.swap(i, rng.below(i + 1));
        }
        let got = from_records(&through_json(&batch), &config).unwrap();
        assert!(bytes(&got.output) == want, "seed {seed}");
        derived += 1;
    }
    assert_eq!(derived, 299);
}

/// Every order of `items`.
fn orders<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut tail in orders(&rest) {
            tail.insert(0, first.clone());
            out.push(tail);
        }
    }
    out
}

/// A truncated copy of a call some kept copy delivers is a duplicate in
/// every record order and whichever profile ranks better; only a marker
/// for a call no copy delivers marks the session.
#[test]
fn a_marker_for_a_kept_call_never_marks_the_session_in_any_order() {
    let (or, m1) = gen_record("g", OR, "from the gateway");
    let (sem, m2) = gen_record("g", "semconv", "from semconv");
    let msgs = [m1, m2];
    for pick in [Pick::Rank, Pick::Arrival] {
        for marker_profile in [OR, "semconv"] {
            let records = [or.clone(), sem.clone(), marker("g", marker_profile)];
            for order in orders(&records) {
                let (s, _) = session_for(&order, &msgs, pick);
                assert!(!s.truncated, "{pick:?} {marker_profile} {order:?}");
            }
            let records = [or.clone(), marker("other", marker_profile)];
            for order in orders(&records) {
                let (s, _) = session_for(&order, &msgs, pick);
                assert!(s.truncated, "{pick:?} {marker_profile} {order:?}");
            }
        }
    }
}

/// Pins the store contract (otel.md "Generation records"): a store keeps
/// every message of every batch read, so a prompt missing even for a copy
/// derivation would discard is a broken store and fails loudly.
#[test]
fn store_contract_a_discarded_copys_missing_prompt_is_an_error() {
    let (or, m1) = gen_record("g", OR, "from the gateway");
    let (sem, _) = gen_record("g", "semconv", "from semconv");
    let store: HashMap<MessageHash, StoredMessage> = [(m1.hash(), m1)].into();
    for records in [[or.clone(), sem.clone()], [sem, or]] {
        let err = derive_path_from_records(&records, |h| store.get(h), &classified()).unwrap_err();
        assert!(matches!(err, OtelError::MessageMissing(_)), "{err}");
    }
}
