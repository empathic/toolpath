//! Derived-Path keys (docs/agents/formats/otel.md: Retention, Path meta and
//! Per step).

use crate::tests::otel::{
    Absent, DeriveConfig, Generation, History, Message, Session, derive_path,
};
use serde_json::{Value, json};

fn user(text: &str) -> Message {
    serde_json::from_value(json!({"role": "user", "content": text})).unwrap()
}

fn system(text: &str) -> Message {
    serde_json::from_value(json!({"role": "system", "content": text})).unwrap()
}

fn gen_(id: &str, start: u64, profile: &str, messages: Vec<Message>, text: &str) -> Generation {
    let mut g = Generation {
        id: id.into(),
        start_ns: start,
        end_ns: start + 1,
        profile: profile.into(),
        messages,
        response_model: Some("m".into()),
        ..Default::default()
    };
    g.completion.text = text.into();
    g
}

fn doc(s: &Session) -> Value {
    serde_json::to_value(derive_path(s, &DeriveConfig::default())).unwrap()
}

fn otel_extras(d: &Value) -> Vec<Value> {
    d["steps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["change"].as_object().unwrap().values())
        .filter(|c| c["structural"]["type"] == "conversation.append")
        .map(|c| c["structural"]["otel"].clone())
        .collect()
}

fn producer<'a>(ex: &'a [Value], gid: &str) -> &'a Value {
    ex.iter()
        .find(|e| e["generation_id"] == gid)
        .unwrap_or_else(|| panic!("no producer step for {gid}"))
}

#[test]
fn mixed_profiles_are_listed() {
    // source_meta must be non-empty for the namespace key to appear.
    let mut g1 = gen_("g1", 1, "semconv", vec![user("hi")], "a");
    g1.source_meta.insert("k".into(), json!(1));
    let mut g2 = gen_(
        "g2",
        2,
        "openrouter",
        vec![
            user("hi"),
            serde_json::from_value(json!({"role": "assistant", "content": "a"})).unwrap(),
            user("more"),
        ],
        "b",
    );
    g2.source_meta.insert("k".into(), json!(2));
    let d = doc(&Session::new("s".into(), None, vec![g1, g2]));
    assert_eq!(d["meta"]["otel"]["profile"], "mixed");
    assert_eq!(
        d["meta"]["otel"]["profiles"],
        json!(["openrouter", "semconv"])
    );
    let ex = otel_extras(&d);
    assert_eq!(
        producer(&ex, "g1")["semconv"],
        json!({"k": 1}),
        "source_meta under the generation's own profile"
    );
    assert_eq!(producer(&ex, "g2")["openrouter"], json!({"k": 2}));
}

#[test]
fn skeleton_turns_carry_absent_and_producer_keys() {
    let mut g1 = gen_("g1", 1, "semconv", vec![user("hi")], "a");
    g1.compacted = true;
    g1.completion.reasoning_details = vec![json!({"type": "reasoning", "content": "r"})];
    let mut g2 = gen_("g2", 2, "semconv", vec![], "");
    g2.absent = Absent {
        prompt: true,
        completion: false,
    };
    g2.continues = Some("g1".into());
    let d = doc(&Session::new("s".into(), None, vec![g1, g2]));
    let ex = otel_extras(&d);
    let producer_1 = producer(&ex, "g1");
    assert_eq!(producer_1["compacted"], true);
    assert_eq!(
        producer_1["reasoning_details"],
        json!([{"type": "reasoning", "content": "r"}])
    );
    assert!(producer_1.get("absent").is_none());
    assert!(producer_1.get("continues").is_none());
    let producer_2 = producer(&ex, "g2");
    assert_eq!(producer_2["absent"], json!({"prompt": true}));
    assert_eq!(producer_2["continues"], "g1");
    assert!(producer_2.get("compacted").is_none());
    assert!(producer_2.get("reasoning_details").is_none());
    assert!(d["meta"]["otel"].get("missing_continuations").is_none());
}

#[test]
fn missing_continuations_reach_meta() {
    let mut g = gen_("g1", 1, "semconv", vec![user("more")], "done");
    g.history = History::Delta;
    g.continues = Some("gone".into());
    let d = doc(&Session::new("s".into(), None, vec![g]));
    assert_eq!(d["meta"]["otel"]["missing_continuations"], json!(["gone"]));
    assert_eq!(d["meta"]["otel"]["harness"], "unknown");
}

#[test]
fn a_delta_records_dropped_by_its_own_message_index() {
    // `Dropped.index` is recorded as the position in the generation's own
    // `messages` (not the effective index), the coordinate a rebuild
    // re-inserts at. The text is stored once, on the Delta's producer step
    // (g1 keeps its index-0 system message, so g1 drops nothing).
    let g1 = gen_("g1", 1, "semconv", vec![system("I"), user("hi")], "hello");
    let mut g2 = gen_("g2", 2, "semconv", vec![system("I"), user("more")], "done");
    g2.history = History::Delta;
    g2.continues = Some("g1".into());
    let d = doc(&Session::new("s".into(), None, vec![g1, g2]));
    let ex = otel_extras(&d);
    assert_eq!(producer(&ex, "g1")["dropped"], json!([]));
    let p2 = producer(&ex, "g2");
    // Known answer, the content hash of the canonical system message:
    // python3: sha256(b'{"role":"system","text":"I"}').hexdigest()
    let h = "5864d6afe8b6ec0d2c5b83c2ea57036adf799378910af0f9cc3d5d09e1b11df0";
    assert_eq!(
        p2["dropped"],
        json!([{"index": 0, "role": "system", "content_hash": h}])
    );
    assert_eq!(p2["dropped_content"], json!({ h: "I" }));
    assert_eq!(p2["continues"], "g1");
    assert!(d["meta"]["otel"].get("missing_continuations").is_none());
}

#[test]
fn unplaced_generations_carry_their_per_request_keys() {
    // An identical retry maps onto g1's turn, so g2 is unplaced.
    let g1 = gen_("g1", 1, "semconv", vec![user("hi")], "a");
    let mut g2 = gen_("g2", 2, "semconv", vec![user("hi")], "a");
    g2.compacted = true;
    g2.completion.reasoning_details = vec![json!({"type": "reasoning", "content": "r"})];
    let d = doc(&Session::new("s".into(), None, vec![g1, g2]));
    let ex = otel_extras(&d);
    let unplaced = producer(&ex, "g2");
    assert_eq!(unplaced["branch"], "unplaced");
    assert_eq!(unplaced["compacted"], true);
    assert_eq!(
        unplaced["reasoning_details"],
        json!([{"type": "reasoning", "content": "r"}])
    );
    assert!(
        producer(&ex, "g1").get("compacted").is_none(),
        "g1's producer step keeps only g1's keys"
    );
}
