//! Fidelity tests against a REAL captured Copilot CLI session.
//!
//! The fixture (`tests/fixtures/real-session.jsonl`) is a feature-elicit run
//! (docs/agents/feature-elicit.md) captured live at `copilotVersion` 1.0.68 —
//! it exercises shell, file create/edit/read, glob + grep search, an errored
//! read, a real sub-agent (`task`) dispatch, per-message reasoning + output
//! tokens, and a real `session.shutdown`. Paths are sanitized to
//! `/tmp/elicit-scratch`.

use std::collections::BTreeSet;
use toolpath::v1::{Graph, PATH_KIND_AGENT_CODING_SESSION, query};
use toolpath_convo::{ConversationProjector, Role, ToolCategory};
use toolpath_copilot::{CopilotProjector, EventReader, Session, to_view};

fn real_session() -> Session {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/real-session.jsonl"
    );
    let lines = EventReader::read_lines(path).expect("parse real fixture");
    Session {
        id: "7a80f0ee-real".to_string(),
        dir_path: path.into(),
        lines,
        workspace: None,
    }
}

// ── Forward: source → view invariants ────────────────────────────────

#[test]
fn forward_view_matches_source_counts() {
    let session = real_session();
    let view = to_view(&session);

    // 1 user prompt (the elicit task list); assistant turns collapse per
    // turn_start/turn_end (11 in the source).
    let users: Vec<_> = view.turns().filter(|t| t.role == Role::User).collect();
    assert_eq!(users.len(), 1, "one elicit user prompt");
    let assistants = view.turns().filter(|t| t.role == Role::Assistant).count();
    assert_eq!(assistants, 11, "one turn per turn_start/turn_end pair");

    // 11 tool calls, all with results, categories per the native vocabulary.
    let tools: Vec<_> = view.turns().flat_map(|t| &t.tool_uses).collect();
    assert_eq!(tools.len(), 11);
    assert!(
        tools.iter().all(|t| t.result.is_some()),
        "every call has its result"
    );
    let by_cat = |c: ToolCategory| tools.iter().filter(|t| t.category == Some(c)).count();
    assert_eq!(by_cat(ToolCategory::FileRead), 4); // view ×4
    assert_eq!(by_cat(ToolCategory::FileWrite), 3); // create ×2 + edit ×1
    assert_eq!(by_cat(ToolCategory::FileSearch), 2); // glob + grep
    assert_eq!(by_cat(ToolCategory::Shell), 1); // bash
    assert_eq!(by_cat(ToolCategory::Delegation), 1); // task

    // Exactly one errored result (the intentionally-missing file).
    let errored = tools
        .iter()
        .filter(|t| t.result.as_ref().is_some_and(|r| r.is_error))
        .count();
    assert_eq!(errored, 1);

    // The sub-agent marker paired with the task tool call by toolCallId.
    let delegations: Vec<_> = view.turns().flat_map(|t| &t.delegations).collect();
    assert_eq!(delegations.len(), 1);
    let task = tools.iter().find(|t| t.name == "task").unwrap();
    assert_eq!(
        delegations[0].agent_id, task.id,
        "delegation correlates to the task tool call via toolCallId"
    );

    // File mutations carry the NATIVE file-state diff (upgraded from the
    // complete's result.detailedContent — Codex-grade fidelity), attributed
    // to their tool call.
    let muts: Vec<_> = view.turns().flat_map(|t| &t.file_mutations).collect();
    assert_eq!(muts.len(), 3, "create ×2 + edit ×1");
    assert!(muts.iter().all(|m| m.tool_id.is_some()));
    let edit_mut = muts
        .iter()
        .find(|m| m.operation.as_deref() == Some("update"))
        .expect("the edit mutation");
    let raw = edit_mut.raw_diff.as_deref().expect("raw diff");
    assert!(raw.contains("@@"), "hunked");
    assert!(
        raw.contains("-") && raw.contains("fixture"),
        "native file-state diff (scratch→fixture edit), got: {raw:.120}"
    );

    // Reasoning captured on every assistant turn that had reasoningText.
    let with_thinking = view.turns().filter(|t| t.thinking.is_some()).count();
    assert!(
        with_thinking >= 10,
        "reasoningText → thinking (got {with_thinking})"
    );

    // Token accounting: Σ per-turn output == session output (3141), and the
    // session total also carries the input/cache totals from the shutdown's
    // tokenDetails (which per-message usage doesn't report) — without the
    // merge these ~222k tokens would be dropped.
    let sum: u32 = view
        .turns()
        .filter_map(|t| t.token_usage.as_ref())
        .filter_map(|u| u.output_tokens)
        .sum();
    assert_eq!(sum, 3141);
    let total = view.total_usage.as_ref().unwrap();
    assert_eq!(total.output_tokens, Some(3141), "output = Σ per-message");
    assert_eq!(total.input_tokens, Some(54), "input from shutdown");
    assert_eq!(
        total.cache_read_tokens,
        Some(198_884),
        "cache_read from shutdown"
    );
    assert_eq!(
        total.cache_write_tokens,
        Some(22_998),
        "cache_write from shutdown"
    );

    // Git-less scratch dir: base still carries the cwd from session.start.
    assert_eq!(
        view.base.as_ref().and_then(|b| b.working_dir.as_deref()),
        Some("/tmp/elicit-scratch")
    );
}

#[test]
fn forward_derives_valid_path() {
    let session = real_session();
    let path = toolpath_copilot::derive::derive_path(&session, &Default::default());
    assert_eq!(
        path.meta.as_ref().unwrap().kind.as_deref(),
        Some(PATH_KIND_AGENT_CODING_SESSION)
    );
    let doc = Graph::from_path(path);
    let json = doc.to_json().unwrap();
    let parsed = Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    let anc = query::ancestors(&p.steps, &p.path.head);
    assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
}

// ── Reverse: view → project → view is a fixed point ─────────────────

#[test]
fn projection_roundtrip_preserves_fidelity() {
    let session = real_session();
    let v1 = to_view(&session);
    let projected = CopilotProjector::new().project(&v1).expect("project");
    // Wire round-trip: serialize each projected line and reparse through the
    // real reader (what `copilot --resume` would read).
    let jsonl: String = projected
        .lines
        .iter()
        .map(|l| serde_json::to_string(l).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("events.jsonl"), &jsonl).unwrap();
    let reparsed = EventReader::read_session_dir(dir.path()).expect("reparse projection");
    let v2 = to_view(&reparsed);

    // Turn structure.
    assert_eq!(v1.turns().count(), v2.turns().count(), "turn count");
    for (a, b) in v1.turns().zip(v2.turns()) {
        assert_eq!(a.role, b.role);
        let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(norm(&a.text), norm(&b.text), "turn text");
        assert_eq!(a.thinking, b.thinking, "thinking");
        // Tool calls: ids, results, error flags survive.
        assert_eq!(a.tool_uses.len(), b.tool_uses.len());
        for (ta, tb) in a.tool_uses.iter().zip(b.tool_uses.iter()) {
            assert_eq!(ta.id, tb.id, "tool call id");
            assert_eq!(ta.category, tb.category, "tool category");
            match (&ta.result, &tb.result) {
                (Some(ra), Some(rb)) => {
                    assert_eq!(ra.is_error, rb.is_error, "error flag for {}", ta.id)
                }
                (a, b) => assert_eq!(a.is_some(), b.is_some(), "result presence"),
            }
        }
        // Per-turn token usage survives exactly.
        assert_eq!(a.token_usage, b.token_usage, "per-turn tokens");
        assert_eq!(a.delegations.len(), b.delegations.len(), "delegations");
    }

    // Session totals survive.
    assert_eq!(
        v1.total_usage.as_ref().unwrap().output_tokens,
        v2.total_usage.as_ref().unwrap().output_tokens
    );
    // Delegation ids stay findable (as delegation or tool call).
    let ids = |v: &toolpath_convo::ConversationView| -> BTreeSet<String> {
        v.turns()
            .flat_map(|t| t.delegations.iter().map(|d| d.agent_id.clone()))
            .collect()
    };
    let tool_ids = |v: &toolpath_convo::ConversationView| -> BTreeSet<String> {
        v.turns()
            .flat_map(|t| t.tool_uses.iter().map(|tu| tu.id.clone()))
            .collect()
    };
    for id in ids(&v1) {
        assert!(
            ids(&v2).contains(&id) || tool_ids(&v2).contains(&id),
            "delegation {id} lost"
        );
    }
}

// ── Wire: reader → serialize is line-fidelity-preserving ────────────

#[test]
fn wire_serde_roundtrip_is_lossless() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/real-session.jsonl"
    );
    let raw = std::fs::read_to_string(path).unwrap();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        let typed: toolpath_copilot::EventLine = serde_json::from_str(line).unwrap();
        let back = serde_json::to_value(&typed).unwrap();
        assert_eq!(orig, back, "line {} not value-identical after serde", i + 1);
    }
}
