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
                .filter(|n| n.producer.is_some() && !n.echoed)
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
        assert!(
            p.steps
                .iter()
                .all(|st| otel_extra(&p, st).is_some_and(|x| x.get("completion").is_none())),
            "{key}: every generation is placed"
        );
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
fn one_inferred_harness_drives_meta_producer_and_tool_categories() {
    for (file, key) in REAL {
        let s = session(file);
        let harness =
            crate::tests::otel::harness::infer_harness(&crate::tests::otel::harness::signals(&s));
        let p = derive_path(&s, &DeriveConfig::default());
        assert_eq!(meta(&p)["harness"], harness.as_str(), "{key}");
        let producer = &p.meta.as_ref().unwrap().extra["producer"]["name"];
        assert_eq!(producer, crate::provider::producer_name(harness), "{key}");
        let view = session_to_view(&s);
        assert_eq!(
            view.producer.as_ref().map(|p| p.name.as_str()),
            producer.as_str(),
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

/// [`one_request`] in session `s` reporting `input`/`output` tokens.
fn used_request(id: &str, start: u64, completion: &str, input: u64, output: u64) -> Value {
    let mut r = one_request(id, start, "s", HI, completion);
    let attrs = r["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
        .as_array_mut()
        .unwrap();
    for (k, v) in [
        ("gen_ai.usage.input_tokens", input),
        ("gen_ai.usage.output_tokens", output),
    ] {
        attrs.push(json!({"key": k, "value": {"intValue": v.to_string()}}));
    }
    r
}

fn step_by_generation<'p>(p: &'p toolpath::v1::Path, gid: &str) -> &'p toolpath::v1::Step {
    p.steps
        .iter()
        .find(|s| otel_extra(p, s).is_some_and(|x| x["generation_id"] == gid))
        .unwrap_or_else(|| panic!("no step for {gid}"))
}

/// Σ of one token class over every step's `token_usage`.
fn step_sum(p: &toolpath::v1::Path, class: &str) -> u64 {
    p.steps
        .iter()
        .filter_map(|s| conv_extra(p, s)?.get("token_usage")?.get(class)?.as_u64())
        .sum()
}

#[test]
fn an_identical_retry_is_recorded_as_unplaced_with_its_extras() {
    let batch = [
        one_request("g1", 10, "s", HI, r#"{"completion":"hello"}"#),
        one_request("g2", 20, "s", HI, r#"{"completion":"hello"}"#),
    ];
    let (paths, _) =
        derive_paths(&batch, ProfileSelection::Auto, &DeriveConfig::default()).unwrap();
    let p = &paths[0];
    let m = meta(p);
    assert!(m.get("unplaced_generations").is_none(), "carried by a step");
    let completion = step_by_generation(p, "g1");
    let retry = step_by_generation(p, "g2");
    let x = otel_extra(p, retry).unwrap();
    assert_eq!(x["trace_id"], "t-g2");
    assert!(x["prompt_tip"].is_string() && x["usage"].is_object());
    assert_eq!(x["completion"], json!(completion.step.id));
    assert_eq!(x["branch"], "unplaced");
    assert_eq!(retry.step.id, format!("{}~g2", completion.step.id));
    assert_eq!(retry.step.parents, completion.step.parents, "a sibling");
    assert_eq!(conv_extra(p, retry).unwrap()["text"], "");
    assert_eq!(p.path.head, completion.step.id);
    let dead: Vec<&str> = toolpath::v1::query::dead_ends(&p.steps, &p.path.head)
        .into_iter()
        .map(|s| s.step.id.as_str())
        .collect();
    assert_eq!(dead, [retry.step.id.as_str()]);
    let rebuilt = rebuild(p, "g2").prompt;
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(m["cost_usd"]["total"], 1.0);
}

#[test]
fn every_generation_s_tokens_are_on_a_step_with_identical_retries() {
    // g2 and g3 repeat g1's answer; g4 answers differently.
    let gens = [
        ("g1", "hello", 10, 3),
        ("g2", "hello", 11, 4),
        ("g3", "hello", 12, 5),
        ("g4", "bye", 13, 6),
    ];
    let request = |i: usize| {
        let (id, text, input, output) = gens[i];
        let completion = format!(r#"{{"completion":"{text}"}}"#);
        used_request(id, 10 * (i as u64 + 1), &completion, input, output)
    };
    let cfg = DeriveConfig::default();
    for retries in [1, 2] {
        let used: Vec<usize> = (0..=retries).chain([3]).collect();
        let fed: Vec<Value> = used.iter().map(|&i| request(i)).collect();
        let (paths, _) = derive_paths(&fed, ProfileSelection::Auto, &cfg).unwrap();
        let p = &paths[0];
        let input: u64 = used.iter().map(|&i| gens[i].2).sum();
        let output: u64 = used.iter().map(|&i| gens[i].3).sum();
        assert_eq!(step_sum(p, "input_tokens"), input, "{retries} retries");
        assert_eq!(step_sum(p, "output_tokens"), output, "{retries} retries");
        let unplaced = p
            .steps
            .iter()
            .filter(|s| otel_extra(p, s).is_some_and(|x| x["branch"] == "unplaced"))
            .count();
        assert_eq!(unplaced, retries);
        assert_eq!(p.path.head, step_by_generation(p, "g4").step.id);
    }
}

#[test]
fn unplaced_steps_are_identical_when_the_session_grows() {
    let hello = r#"{"completion":"hello"}"#;
    let batch: Vec<Value> = (0..4u64)
        .map(|i| used_request(&format!("g{i}"), 10 * (i + 1), hello, 5, i + 1))
        .collect();
    let cfg = DeriveConfig::default();
    let (whole, _) = derive_paths(&batch, ProfileSelection::Auto, &cfg).unwrap();
    let whole = &whole[0];
    for k in 1..batch.len() {
        let (part, _) = derive_paths(&batch[..k], ProfileSelection::Auto, &cfg).unwrap();
        let part = &part[0];
        let unplaced = part
            .steps
            .iter()
            .filter(|s| otel_extra(part, s).is_some_and(|x| x["branch"] == "unplaced"));
        assert_eq!(unplaced.count(), k - 1, "k={k}");
        // g0's produced turn is still pending (no later prompt echoes it).
        let produced = step_by_generation(part, "g0").step.id.clone();
        for step in part.steps.iter().filter(|s| s.step.id != produced) {
            let later = whole
                .steps
                .iter()
                .find(|s| s.step.id == step.step.id)
                .unwrap_or_else(|| panic!("k={k}: step {} vanished", step.step.id));
            assert_eq!(
                canonical_step_json(step),
                canonical_step_json(later),
                "k={k} step {}",
                step.step.id
            );
        }
    }
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
