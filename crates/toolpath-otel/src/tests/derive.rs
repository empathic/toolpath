use super::common::retention::{assert_retains, conv_extra, meta, otel_extra, rebuild};
use super::common::*;
use crate::tests::otel::derive::canonical_step_json;
use crate::tests::otel::hash::{canonical_json, sha256_hex};
use crate::tests::otel::{
    DeriveConfig, ProfileSelection, Session, derive_path, derive_paths, group_sessions,
    session_to_view, stitch,
};
use serde_json::{Value, json};

fn prefix(s: &Session, k: usize) -> Session {
    Session::new(
        s.key.clone(),
        s.session_id.clone(),
        s.generations[..k].to_vec(),
    )
}

#[test]
fn shared_steps_are_identical_when_the_session_grows() {
    let cfg = DeriveConfig::default();
    for file in CONVERSATIONS {
        let full = session(file);
        let whole = derive_path(&full, &cfg);
        for k in 1..full.generations.len() {
            let head = prefix(&full, k);
            let part = derive_path(&head, &cfg);
            // Payload stability: a produced turn is fixed once a later
            // generation echoes it (bringing its echo and results).
            let pending: Vec<String> = stitch(&head)
                .nodes
                .iter()
                .filter(|n| n.producer.is_some() && n.echoed_by.is_none())
                .map(|n| n.id.clone())
                .collect();
            for step in &part.steps {
                let later = whole
                    .steps
                    .iter()
                    .find(|s| s.step.id == step.step.id)
                    .unwrap_or_else(|| panic!("{file} k={k}: step {} vanished", step.step.id));
                if !pending.contains(&step.step.id) {
                    assert_eq!(
                        canonical_step_json(step),
                        canonical_step_json(later),
                        "{file} k={k} step {}",
                        step.step.id
                    );
                }
            }
        }
    }
}

#[test]
fn meta_records_join_keys_cost_harness_and_profile() {
    let exp = expected();
    for (file, key) in REAL {
        let s = session(file);
        let p = derive_path(&s, &DeriveConfig::default());
        assert_eq!(p.meta.as_ref().unwrap().source.as_deref(), Some("otel"));
        assert!(p.path.id.starts_with("path-otel-"), "{}", p.path.id);
        let m = meta(&p);
        let e = &exp["sessions"][key];
        assert_eq!(m["profile"], "openrouter", "{key}");
        assert_eq!(m["harness"], e["harness"], "{key}");
        assert_eq!(m["session_id"], e["session_id"], "{key}");
        assert_eq!(m["session_key"], json!(s.key), "{key}");
        assert_eq!(
            m["derived_session_id"],
            json!(crate::tests::otel::derived_session_id(&s.key))
        );
        assert_eq!(
            strings(&m["generation_ids"]),
            strings(&e["generation_ids"]),
            "{key}"
        );
        let total = m["cost_usd"]["total"].as_f64().unwrap();
        assert!(
            (total - e["total_cost_usd"].as_f64().unwrap()).abs() < 1e-9,
            "{key}: {total}"
        );
        let models: Vec<String> = m["cost_usd"]["by_model"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(models, strings(&e["models"]), "{key}");
        assert_eq!(m["client_key"], "fixture key");
        assert_eq!(m["truncated"], false);
        assert!(m.get("unplaced_generations").is_none(), "{key}");
        // Session-level profile data comes from the raw span attributes.
        let raw = &deliveries(file)[0]["resourceSpans"][0]["scopeSpans"][0]["spans"];
        let root = raw
            .as_array()
            .unwrap()
            .iter()
            .find(|sp| sp["name"] == "LLM Generation")
            .unwrap();
        let attr = |k: &str| {
            root["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["key"] == k)
                .unwrap()["value"]["stringValue"]
                .clone()
        };
        assert_eq!(
            m["openrouter"]["entity_id"],
            attr("trace.metadata.openrouter.entity_id"),
            "{key}"
        );
        assert_eq!(
            m["openrouter"]["creator_user_id"],
            attr("trace.metadata.openrouter.creator_user_id"),
            "{key}"
        );
    }
}

#[test]
fn every_turn_step_carries_retention_extras() {
    for file in CONVERSATIONS {
        let p = derive_path(&session(file), &DeriveConfig::default());
        let mut producers = 0;
        for st in &p.steps {
            let x = otel_extra(&p, st)
                .unwrap_or_else(|| panic!("{file}: {} has no extras", st.step.id));
            assert_eq!(x["content_hash"].as_str().unwrap().len(), 64);
            assert!(x["first_generation_id"].is_string());
            assert!(x["message_role"].is_string());
            if x.get("generation_id").is_some() {
                producers += 1;
                for k in [
                    "trace_id",
                    "prompt_tip",
                    "request_model",
                    "usage",
                    "cost",
                    "dropped",
                ] {
                    assert!(x.get(k).is_some(), "{file}: missing {k}");
                }
                // tools_digest is pinned per real fixture elsewhere; the
                // synthetic fixture carries no tool definitions.
                assert!(x["openrouter"]["request_params"].is_object(), "{file}");
            }
        }
        assert_eq!(producers, session(file).generations.len(), "{file}");
    }
}

#[test]
fn each_generation_rebuilds_from_the_path_alone() {
    for file in CONVERSATIONS {
        let one = group_sessions(read_deliveries(&deliveries(file)).unwrap().generations).len();
        assert_eq!(one, 1, "{file}");
        let checked = assert_retains(&deliveries(file), ProfileSelection::Auto);
        assert_eq!(
            checked.generations,
            session(file).generations.len(),
            "{file}"
        );
        assert_eq!(checked.continued, 0, "{file}: M0 has no Delta generation");
        assert_eq!(checked.skeletons, 0, "{file}: M0 has no skeleton");
    }
}

#[test]
fn claude_cross_check_against_the_harness_import() {
    let exp = expected();
    let h = &exp["sessions"]["claude-code"]["harness_import"];
    let s = session("claude-code.ndjson");
    let p = derive_path(&s, &DeriveConfig::default());
    let appends = |actor_prefix: &str| {
        p.steps
            .iter()
            .filter(|st| st.step.actor.starts_with(actor_prefix))
            .filter(|st| {
                st.change.values().any(|c| {
                    c.structural
                        .as_ref()
                        .is_some_and(|x| x.change_type == "conversation.append")
                })
            })
            .count()
    };
    assert_eq!(
        appends("human:") as u64,
        h["change_counts"]["human:conversation.append"]
            .as_u64()
            .unwrap()
    );
    // The harness writes each message as thinking + tool_use lines (10 =
    // 5 × 2); the trace has one assistant turn per generation.
    assert_eq!(appends("agent:"), s.generations.len());
    let writes = p
        .steps
        .iter()
        .flat_map(|st| st.change.values())
        .filter(|c| {
            c.structural
                .as_ref()
                .is_some_and(|x| x.change_type == "file.write")
        })
        .count() as u64;
    assert_eq!(
        writes,
        h["change_counts"]["agent:file.write"].as_u64().unwrap()
    );
    let ids: Vec<String> = session_to_view(&s)
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .map(|u| u.id.clone())
        .collect();
    assert_eq!(ids, strings(&h["tool_call_ids"]));
}

#[test]
fn a_capture_ending_mid_tool_call_still_derives() {
    let p = derive_path(&session("codex.ndjson"), &DeriveConfig::default());
    let head = p.steps.iter().find(|st| st.step.id == p.path.head).unwrap();
    let uses = conv_extra(&p, head).unwrap()["tool_uses"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        uses.iter()
            .any(|u| u.get("result").is_none_or(Value::is_null))
    );
}

fn one_request(id: &str, start: u64, session: &str, prompt: &str, completion: &str) -> Value {
    let attr = |k: &str, v: &str| json!({"key": k, "value": {"stringValue": v}});
    json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": format!("t-{id}"), "spanId": "r", "name": "LLM Generation",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 1).to_string(),
        "attributes": [
            attr("gen_ai.response.id", id), attr("session.id", session),
            attr("gen_ai.prompt", prompt), attr("gen_ai.completion", completion),
            {"key": "gen_ai.usage.total_cost", "value": {"doubleValue": 0.5}}
        ]
    }]}]}]})
}

const HI: &str = r#"{"messages":[{"role":"user","content":"hi"}]}"#;

#[test]
fn an_identical_retry_is_recorded_as_unplaced_with_its_extras() {
    let batch = [
        one_request("g1", 10, "s", HI, r#"{"completion":"hello"}"#),
        one_request("g2", 20, "s", HI, r#"{"completion":"hello"}"#),
    ];
    let (paths, _) =
        derive_paths(&batch, ProfileSelection::Auto, &DeriveConfig::default()).unwrap();
    let m = meta(&paths[0]);
    let unplaced = m["unplaced_generations"].as_array().unwrap();
    assert_eq!(unplaced.len(), 1);
    assert_eq!(unplaced[0]["generation_id"], "g2");
    assert_eq!(unplaced[0]["trace_id"], "t-g2");
    assert!(unplaced[0]["prompt_tip"].is_string() && unplaced[0]["usage"].is_object());
    let completion = unplaced[0]["completion"].as_str().unwrap();
    assert!(paths[0].steps.iter().any(|s| s.step.id == completion));
    let rebuilt = rebuild(&paths[0], "g2").prompt;
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(m["cost_usd"]["total"], 1.0);
}

#[test]
fn a_truncated_span_marks_its_session() {
    let batch = [
        one_request("g1", 10, "s", HI, r#"{"completion":"ok"}"#),
        one_request(
            "g2",
            20,
            "s",
            r#"{"messages":[{"role""#,
            r#"{"completion":"ok"}"#,
        ),
        one_request("g3", 30, "other", HI, r#"{"completion":"ok"}"#),
    ];
    let (paths, outcome) =
        derive_paths(&batch, ProfileSelection::Auto, &DeriveConfig::default()).unwrap();
    assert!(outcome.generations.is_empty(), "moved into sessions");
    let truncated: Vec<(Value, Value)> = paths
        .iter()
        .map(|p| (meta(p)["session_id"].clone(), meta(p)["truncated"].clone()))
        .collect();
    assert_eq!(
        truncated,
        vec![(json!("s"), json!(true)), (json!("other"), json!(false))]
    );
}

#[test]
fn derived_paths_are_pinned() {
    let mut rec = serde_json::Map::new();
    for file in CONVERSATIONS {
        let p = derive_path(&session(file), &DeriveConfig::default());
        let canon = canonical_json(&serde_json::to_value(&p).unwrap());
        if let Ok(dir) = std::env::var("TOOLPATH_OTEL_DUMP") {
            std::fs::write(format!("{dir}/{file}.path.json"), &canon).unwrap();
        }
        rec.insert(
            file.to_string(),
            json!({"head": p.path.head, "steps": p.steps.len(), "sha256": sha256_hex(&[canon.as_bytes()])}),
        );
    }
    check_snapshot_with_hint(
        "derive-paths",
        &Value::Object(rec),
        "\nRe-run with TOOLPATH_OTEL_DUMP=<dir> to write each derived path as \
         <dir>/<fixture>.path.json and diff it against the base branch's dump.",
    );
}
