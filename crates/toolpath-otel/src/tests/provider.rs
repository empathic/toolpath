use super::common::*;
use crate::tests::otel::harness::{infer_harness, signals};
use crate::tests::otel::session_to_view;
use std::collections::BTreeSet;
use toolpath_convo::{DeriveConfig, Role, derive_path};

#[test]
fn views_carry_tool_results_usage_cwd_and_derived_id() {
    let exp = expected();
    for (file, key) in REAL {
        let s = session(file);
        let view = session_to_view(&s);
        let e = &exp["sessions"][key];
        assert_ne!(
            Some(view.id.as_str()),
            s.session_id.as_deref(),
            "{key}: derived id"
        );
        assert_eq!(
            view.base.as_ref().and_then(|b| b.working_dir.as_deref()),
            e["cwd"].as_str(),
            "{key}: cwd"
        );
        let uses: Vec<_> = view.turns.iter().flat_map(|t| &t.tool_uses).collect();
        let ids: Vec<String> = uses.iter().map(|u| u.id.clone()).collect();
        assert_eq!(ids, strings(&e["tool_call_ids"]), "{key}: tool ids");
        let last_calls: Vec<&str> = view
            .turns
            .last()
            .unwrap()
            .tool_uses
            .iter()
            .map(|u| u.id.as_str())
            .collect();
        for u in &uses {
            if !last_calls.contains(&u.id.as_str()) {
                assert!(u.result.is_some(), "{key}: {} has no result", u.id);
            }
        }
        let with_usage: Vec<_> = view
            .turns
            .iter()
            .filter(|t| t.token_usage.is_some())
            .collect();
        assert!(
            with_usage.iter().all(|t| t.role == Role::Assistant),
            "{key}"
        );
        assert_eq!(with_usage.len(), s.generations.len(), "{key}");
        let summed: u64 = with_usage
            .iter()
            .map(|t| t.token_usage.as_ref().unwrap().input_tokens.unwrap() as u64)
            .sum();
        let total: u64 = s
            .generations
            .iter()
            .map(|g| g.usage.input_tokens.unwrap())
            .sum();
        assert_eq!(summed, total, "{key}");
        let tu = view.total_usage.as_ref().unwrap();
        assert_eq!(tu.input_tokens.map(u64::from), Some(total), "{key}");
        let cache = |f: fn(&toolpath_convo::TokenUsage) -> Option<u32>| -> u64 {
            with_usage
                .iter()
                .map(|t| f(t.token_usage.as_ref().unwrap()).unwrap() as u64)
                .sum()
        };
        let reported = |f: fn(&crate::tests::otel::Usage) -> Option<u64>| -> u64 {
            s.generations.iter().map(|g| f(&g.usage).unwrap()).sum()
        };
        assert_eq!(
            cache(|u| u.cache_read_tokens),
            reported(|u| u.cached_input_tokens),
            "{key}"
        );
        assert_eq!(
            cache(|u| u.cache_write_tokens),
            reported(|u| u.cache_write_tokens),
            "{key}"
        );
    }
}

#[test]
fn harness_inference_matches_expected_for_each_fixture() {
    let exp = expected();
    for (file, key) in REAL {
        let s = session(file);
        assert_eq!(
            infer_harness(&signals(&s)).as_str(),
            exp["sessions"][key]["harness"].as_str().unwrap(),
            "{key}"
        );
    }
}

/// `files_changed` equals the emitted `file.write` paths, every write is
/// attributed to a tool call, and none comes from convo's fallback.
#[test]
fn files_changed_equals_emitted_file_writes() {
    for file in CONVERSATIONS {
        let view = session_to_view(&session(file));
        let path = derive_path(&view, &DeriveConfig::default());
        let mut written: Vec<String> = Vec::new();
        for (k, c) in path.steps.iter().flat_map(|s| &s.change) {
            let Some(st) = c
                .structural
                .as_ref()
                .filter(|st| st.change_type == "file.write")
            else {
                continue;
            };
            let tool_id = st.extra.get("tool_id").and_then(|v| v.as_str()).unwrap();
            assert!(
                view.turns
                    .iter()
                    .flat_map(|t| &t.file_mutations)
                    .any(|m| m.tool_id.as_deref() == Some(tool_id)),
                "{file}: {tool_id} came from the fallback"
            );
            written.push(k.clone());
        }
        let a: BTreeSet<&String> = written.iter().collect();
        let b: BTreeSet<&String> = view.files_changed.iter().collect();
        assert_eq!(a, b, "{file}");
    }
}

#[test]
fn opencode_write_calls_become_two_file_writes() {
    let view = session_to_view(&session("opencode.ndjson"));
    let writes: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .filter(|u| u.name == "write")
        .map(|u| u.id.as_str())
        .collect();
    assert_eq!(writes.len(), 2);
    let attributed: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|t| &t.file_mutations)
        .filter_map(|m| m.tool_id.as_deref())
        .collect();
    assert_eq!(attributed, writes);
    assert_eq!(view.files_changed.len(), 2, "{:?}", view.files_changed);
    let path = derive_path(&view, &DeriveConfig::default());
    let raw_writes = path
        .steps
        .iter()
        .flat_map(|s| s.change.values())
        .filter(|c| {
            c.structural
                .as_ref()
                .is_some_and(|s| s.change_type == "file.write")
        })
        .inspect(|c| assert!(c.raw.is_some()))
        .count();
    assert_eq!(raw_writes, 2);
}

/// `producer.name` uses the harness names the other derivers and Pathbase
/// use; `meta.source` stays `otel`.
#[test]
fn producer_names_the_inferred_harness() {
    for (file, name) in [
        ("claude-code.ndjson", "claude-code"),
        ("codex.ndjson", "codex"),
        ("opencode.ndjson", "opencode"),
        ("pi.ndjson", "pi"),
    ] {
        let view = session_to_view(&session(file));
        assert_eq!(view.producer.unwrap().name, name, "{file}");
        assert_eq!(view.provider_id.as_deref(), Some("otel"));
    }
    assert_eq!(
        crate::provider::producer_name(crate::harness::SourceHarness::Unknown),
        "otel"
    );
}
