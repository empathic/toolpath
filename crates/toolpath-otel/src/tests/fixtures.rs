use super::common::*;
use crate::tests::otel::SkipReason;

#[test]
fn reader_yields_every_expected_generation_in_each_fixture() {
    let exp = expected();
    for (file, key) in REAL {
        let values = deliveries(file);
        let out = read_deliveries(&values).unwrap();
        let ids: Vec<String> = out.generations.iter().map(|g| g.id.clone()).collect();
        assert_eq!(
            ids,
            strings(&exp["sessions"][key]["generation_ids"]),
            "{file}"
        );
        assert!(out.skipped.is_empty(), "{file}: {:?}", out.skipped);
    }
}

#[test]
fn reader_drops_error_spans_and_connection_tests() {
    let values = [
        deliveries("codex-error-span.json"),
        deliveries("connection-test.json"),
    ]
    .concat();
    let out = read_deliveries(&values).unwrap();
    assert!(out.generations.is_empty());
    let reasons: Vec<SkipReason> = out.skipped.iter().map(|s| s.reason).collect();
    assert_eq!(
        reasons,
        vec![SkipReason::ErrorStatus, SkipReason::ConnectionTest]
    );
}

#[test]
fn reader_dedupes_a_repeated_delivery() {
    let mut values = deliveries("opencode.ndjson");
    values.push(values[0].clone());
    let out = read_deliveries(&values).unwrap();
    assert_eq!(out.generations.len(), 9);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].reason, SkipReason::Duplicate);
}

#[test]
fn reader_reads_usage_cost_models_and_source_meta() {
    let out = read_deliveries(&deliveries("claude-code.ndjson")).unwrap();
    let g = &out.generations[0];
    assert_eq!(
        g.session_id.as_deref(),
        Some("177b923f-8cf6-42fc-9f30-9a7b86236265")
    );
    assert_eq!(g.request_session_id.as_deref(), g.session_id.as_deref());
    // 48894 inclusive − 47889 cache writes; input_cost 0.06086625 =
    // 1005 × $1/M + 47889 × $1.25/M confirms the source was inclusive.
    assert_eq!(g.usage.input_tokens, Some(1005));
    assert_eq!(g.usage.cache_write_tokens, Some(47889));
    assert_eq!(g.usage.reasoning_tokens, Some(518));
    assert_eq!(g.cost.total, Some(0.06507625));
    assert_eq!(
        g.response_model.as_deref(),
        Some("anthropic/claude-haiku-4.5")
    );
    assert_eq!(g.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(g.client_key.as_deref(), Some("fixture key"));
    assert_eq!(g.source_meta["provider_name"], "Amazon Bedrock");
    assert_eq!(g.source_meta["upstream_finish_reason"], "tool_use");
    assert!(g.source_meta["provider_responses"].is_array());
    assert!(g.start_ns < g.end_ns);
    assert_eq!(
        g.completion.tool_calls[0].id,
        "toolu_bdrk_012ruqQFviGtwFzzYTxLJrcg"
    );
    assert_eq!(g.source_meta["first_token_ms"], 4413.0);
    assert_eq!(g.source_meta["router_latency_ms"], 167.0);
    assert!(g.source_meta["inter_token_latency_ms"].is_f64());
    assert!(!g.source_meta.contains_key("latency_ms"));
    let params = g.source_meta["request_params"].as_object().unwrap();
    assert_eq!(params["session_id"], "177b923f-8cf6-42fc-9f30-9a7b86236265");
    for k in ["messages", "tools", "input"] {
        assert!(!params.contains_key(k), "request_params carries {k}");
    }
}

/// Oracle: per `gen_ai.response.id`, the tools its request carried (or `None`), read directly from the fixture JSON.
fn requested_tools(file: &str) -> std::collections::BTreeMap<String, Option<serde_json::Value>> {
    let mut out = std::collections::BTreeMap::new();
    for d in deliveries(file) {
        let spans = d["resourceSpans"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|rs| rs["scopeSpans"].as_array().into_iter().flatten())
            .flat_map(|ss| ss["spans"].as_array().into_iter().flatten());
        for span in spans.filter(|s| s["name"] == "LLM Generation") {
            let attr = |k: &str| {
                span["attributes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|kv| kv["key"] == k)
                    .and_then(|kv| kv["value"]["stringValue"].as_str())
                    .map(str::to_string)
            };
            let id = attr("gen_ai.response.id").expect("real fixtures carry ids");
            let completion: serde_json::Value = attr("gen_ai.completion")
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default();
            let tools = [&completion["tools"], &completion["rawRequest"]["tools"]]
                .into_iter()
                .find(|t| !t.is_null())
                .cloned();
            out.insert(id, tools);
        }
    }
    out
}

#[test]
fn fixture_tools_digests_match_their_requests() {
    use crate::tests::otel::hash::{canonical_json, sha256_hex};
    let mut digests = 0;
    for (file, key) in REAL {
        let oracle = requested_tools(file);
        let out = read_deliveries(&deliveries(file)).unwrap();
        assert!(!out.generations.is_empty(), "{key}");
        assert_eq!(out.generations.len(), oracle.len(), "{key}");
        for g in &out.generations {
            let got = g
                .source_meta
                .get("tools_digest")
                .map(|v| v.as_str().unwrap());
            let want = oracle[&g.id]
                .as_ref()
                .map(|t| sha256_hex(&[canonical_json(t).as_bytes()]));
            assert_eq!(got, want.as_deref(), "{key} {}", g.id);
            if let Some(d) = got {
                assert_eq!(d.len(), 64, "{key} {}", g.id);
                assert!(
                    d.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                    "{key} {}",
                    g.id
                );
                digests += 1;
            }
        }
    }
    assert!(digests > 0, "no REAL fixture carries tools");
}

use crate::tests::otel::group_sessions;

#[test]
fn all_fixtures_group_into_five_sessions() {
    let files = [
        "claude-code.ndjson",
        "codex.ndjson",
        "opencode.ndjson",
        "pi.ndjson",
        "synthetic-fork.ndjson",
    ];
    let values: Vec<_> = files.iter().flat_map(|f| deliveries(f)).collect();
    let sessions = group_sessions(read_deliveries(&values).unwrap().generations);
    assert_eq!(sessions.len(), 5);
    let exp = expected();
    for (_, key) in REAL {
        let ids = strings(&exp["sessions"][key]["generation_ids"]);
        let s = sessions
            .iter()
            .find(|s| s.generations[0].id == ids[0])
            .unwrap_or_else(|| panic!("{key}: no session"));
        assert_eq!(
            s.generations
                .iter()
                .map(|g| g.id.clone())
                .collect::<Vec<_>>(),
            ids,
            "{key}"
        );
        assert_eq!(
            s.session_id.as_deref(),
            exp["sessions"][key]["session_id"].as_str(),
            "{key}"
        );
    }
    // This pi capture (an older release) sends no session id, so its whole
    // run clusters into exactly one session under one cluster key.
    let sessionless: Vec<_> = sessions.iter().filter(|s| s.session_id.is_none()).collect();
    assert_eq!(sessionless.len(), 1);
    let pi = sessionless[0];
    assert!(pi.key.starts_with("otel-cluster:"));
    assert_eq!(
        pi.generations
            .iter()
            .map(|g| g.id.clone())
            .collect::<Vec<_>>(),
        strings(&exp["sessions"]["pi"]["generation_ids"])
    );
    let fork = &exp["synthetic_fork"];
    let s = sessions
        .iter()
        .find(|s| s.session_id.as_deref() == fork["session_id"].as_str())
        .expect("synthetic fork session");
    assert_eq!(s.key, fork["session_id"].as_str().unwrap());
    assert_eq!(
        s.generations.len() as u64,
        fork["generations"].as_u64().unwrap()
    );
}

use crate::tests::otel::stitch;

#[test]
fn real_sessions_are_strict_extensions_with_no_forks() {
    let exp = expected();
    for (file, key) in REAL {
        let s = session(file);
        let g = stitch(&s);
        assert!(
            fork_points(&g).is_empty(),
            "{key}: forks {:?}",
            fork_points(&g)
        );
        let completions: Vec<&str> = g.links.iter().map(|l| l.completion.as_str()).collect();
        // Each generation's completion is on the next generation's prompt chain.
        for w in g.links.windows(2) {
            let node = g.nodes.iter().find(|n| n.id == w[1].completion).unwrap();
            let mut ancestor = node.parent.clone();
            let mut found = false;
            while let Some(a) = ancestor {
                if a == w[0].completion {
                    found = true;
                    break;
                }
                ancestor = g
                    .nodes
                    .iter()
                    .find(|n| n.id == a)
                    .and_then(|n| n.parent.clone());
            }
            assert!(found, "{key}: {completions:?}");
        }
        assert_eq!(
            g.links.len() - 1,
            exp["sessions"][key]["consecutive_extensions"]
                .as_u64()
                .unwrap() as usize,
            "{key}"
        );
        assert_eq!(
            g.nodes.last().unwrap().id,
            g.links.last().unwrap().completion,
            "{key}: head"
        );
    }
}

#[test]
fn claude_history_rewrites_are_kept_as_echo_arguments() {
    let exp = expected();
    let g = stitch(&session("claude-code.ndjson"));
    let mut rewritten: Vec<String> = g
        .nodes
        .iter()
        .filter_map(|n| n.echo.as_ref())
        .flat_map(|e| e.arguments.keys().cloned())
        .collect();
    rewritten.sort();
    let mut want = strings(&exp["sessions"]["claude-code"]["history_rewritten_tool_calls"]);
    want.sort();
    assert_eq!(rewritten, want);
}

#[test]
fn synthetic_fixture_forks_at_the_retry_and_the_compaction() {
    let exp = expected();
    let s = session("synthetic-fork.ndjson");
    let g = stitch(&s);
    let forks = fork_points(&g);
    assert_eq!(
        forks.len(),
        exp["synthetic_fork"]["forks"].as_array().unwrap().len()
    );
    let gen_index = |id: &str| s.generations.iter().position(|g| g.id == id).unwrap();
    let completion = |id: &str| g.links[gen_index(id)].completion.clone();
    let parent_of = |id: &str| {
        g.nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap()
            .parent
            .clone()
            .unwrap()
    };
    // Retry: both generation-2 completions hang off generation 1's completion.
    assert_eq!(
        parent_of(&completion("gen-fixture-fork-2")),
        completion("gen-fixture-fork-1")
    );
    assert_eq!(
        parent_of(&completion("gen-fixture-fork-2-retry")),
        completion("gen-fixture-fork-1")
    );
    assert!(forks.contains(&completion("gen-fixture-fork-1")));
    // Compaction: shares only the leading system message.
    let system = g.nodes[0].id.clone();
    assert_eq!(g.nodes[0].message.role, "system");
    assert!(forks.contains(&system));
    assert_eq!(
        g.nodes.last().unwrap().id,
        completion("gen-fixture-fork-compacted")
    );
}

#[test]
fn trailing_system_blocks_are_dropped_and_remembered() {
    let g = stitch(&session("claude-code.ndjson"));
    assert!(g.links.iter().all(|l| !l.dropped.is_empty()));
    for l in &g.links {
        for d in &l.dropped {
            assert!(g.dropped_content.contains_key(&d.content_hash));
        }
    }
    assert!(g.nodes.iter().skip(1).all(|n| n.message.role != "system"));
}

#[test]
fn openrouter_generations_carry_the_neutral_defaults() {
    use crate::tests::otel::{CacheBasis, History};
    for (file, _) in REAL {
        let out = read_deliveries(&deliveries(file)).unwrap();
        assert!(!out.generations.is_empty(), "{file}");
        for g in &out.generations {
            assert_eq!(g.profile, "openrouter", "{file} {}", g.id);
            assert_eq!(
                g.usage.cache_basis,
                Some(CacheBasis::Inclusive),
                "{file} {}",
                g.id
            );
            assert_eq!(g.history, History::Full);
            assert_eq!(g.continues, None);
            assert!(!g.absent.any());
            assert!(g.tool_results.is_empty());
            assert!(!g.compacted);
            assert_eq!(g.harness_hint, None);
            assert!(g.completion.reasoning_details.is_empty());
        }
    }
}
