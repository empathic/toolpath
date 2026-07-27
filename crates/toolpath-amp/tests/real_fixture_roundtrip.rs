//! Fidelity tests against a REAL captured Amp thread.
//!
//! The fixture (`tests/fixtures/real-session.json`, byte-identical to
//! `test-fixtures/amp/convo.json`) is the sanitized `amp threads export` of
//! a feature-elicit run (docs/agents/feature-elicit.md) captured live on
//! 2026-07-27 at Amp version `0.0.1785170481-ga5b614` (the thread's own
//! `env.initial.platform.clientVersion` anchor). It exercises shell
//! commands, `apply_patch` create + edit, a deliberately failing read, a
//! real `Task` sub-agent dispatch, a `skill` load, thinking blocks (5
//! non-empty of 12), and per-message token usage on all 12 assistant
//! messages.

use toolpath::v1::{Graph, PATH_KIND_AGENT_CODING_SESSION, query};
use toolpath_amp::derive::{DeriveConfig, derive_path};
use toolpath_amp::{ExportReader, Session, to_view};
use toolpath_convo::{Role, ToolCategory};

const REAL: &str = include_str!("fixtures/real-session.json");
const REAL_ID: &str = "T-019fa4db-29cf-70c9-8d9b-81524df70e52";

fn real_session() -> Session {
    // Strict: the real capture must parse without tolerance — any Unknown
    // block or malformed message here means schema drift.
    Session::from_export(ExportReader::parse_export_with(REAL, true).expect("parse real fixture"))
}

// ── Forward: source → view invariants ────────────────────────────────

#[test]
fn forward_view_matches_source_counts() {
    let view = to_view(&real_session());
    assert_eq!(view.id, REAL_ID);

    // 24 messages = 1 human prompt + 12 assistant + 11 tool-result-only.
    assert_eq!(view.turns.len(), 13);
    let users: Vec<_> = view.turns.iter().filter(|t| t.role == Role::User).collect();
    assert_eq!(users.len(), 1, "one elicit user prompt");
    assert_eq!(
        view.turns
            .iter()
            .filter(|t| t.role == Role::Assistant)
            .count(),
        12
    );

    // 11 tool calls, all with results, categorized per the native
    // vocabulary (no read/glob/grep tools exist — Codex-shaped surface).
    let tools: Vec<_> = view.turns.iter().flat_map(|t| &t.tool_uses).collect();
    assert_eq!(tools.len(), 11);
    assert!(tools.iter().all(|t| t.result.is_some()));
    let by_cat = |c: ToolCategory| tools.iter().filter(|t| t.category == Some(c)).count();
    assert_eq!(by_cat(ToolCategory::Shell), 6); // shell_command ×6
    assert_eq!(by_cat(ToolCategory::FileWrite), 3); // apply_patch ×3
    assert_eq!(by_cat(ToolCategory::Delegation), 1); // Task
    assert_eq!(
        tools.iter().filter(|t| t.category.is_none()).count(),
        1,
        "skill stays uncategorized"
    );

    // Exactly one errored result — the intentionally-missing file, whose
    // failure shows only as exitCode 1 (run.status was "done").
    let errored = tools
        .iter()
        .filter(|t| t.result.as_ref().is_some_and(|r| r.is_error))
        .count();
    assert_eq!(errored, 1);

    // The sub-agent: paired to the Task call by TU id, result backfilled,
    // no child turns (none exist anywhere).
    let delegations: Vec<_> = view.turns.iter().flat_map(|t| &t.delegations).collect();
    assert_eq!(delegations.len(), 1);
    let task = tools.iter().find(|t| t.name == "Task").unwrap();
    assert_eq!(delegations[0].agent_id, task.id);
    assert_eq!(
        delegations[0].result.as_deref(),
        Some("`notes.md` contains **11 words**.")
    );
    assert!(delegations[0].turns.is_empty());

    // File mutations carry the wire's REAL unified diffs.
    let muts: Vec<_> = view.turns.iter().flat_map(|t| &t.file_mutations).collect();
    assert_eq!(muts.len(), 3, "notes.md add + update, count.sh add");
    assert!(
        muts.iter()
            .all(|m| m.raw_diff.is_some() && m.tool_id.is_some())
    );
    assert_eq!(view.files_changed, vec!["notes.md", "count.sh"]);

    // Thinking: 5 of 12 summaries were non-empty; sealed-empty ones must
    // not surface as Some("").
    assert_eq!(
        view.turns.iter().filter(|t| t.thinking.is_some()).count(),
        5
    );

    // Token accounting: clean per-message → Σ = session total.
    let total = view.total_usage.as_ref().unwrap();
    assert_eq!(total.output_tokens, Some(1537));
    assert_eq!(total.input_tokens, Some(0));
    assert_eq!(total.cache_read_tokens, Some(210_694));
    assert_eq!(total.cache_write_tokens, Some(20_758));
    for t in &view.turns {
        assert!(t.group_id.is_none());
        assert!(t.attributed_token_usage.is_none());
    }

    // Base and producer.
    assert_eq!(
        view.base.as_ref().and_then(|b| b.working_dir.as_deref()),
        Some("/tmp/amp-elicit")
    );
    assert!(view.base.as_ref().unwrap().vcs_revision.is_none());
    assert_eq!(
        view.producer.as_ref().and_then(|p| p.version.as_deref()),
        Some("0.0.1785170481-ga5b614")
    );
}

// ── Forward: derived document validity ──────────────────────────────

#[test]
fn forward_derives_valid_single_path_graph() {
    let path = derive_path(&real_session(), &DeriveConfig::default());
    assert_eq!(
        path.meta.as_ref().unwrap().kind.as_deref(),
        Some(PATH_KIND_AGENT_CODING_SESSION)
    );
    assert_eq!(
        path.meta.as_ref().unwrap().title.as_deref(),
        Some("Amp session: T-019fa4")
    );
    let doc = Graph::from_path(path);
    let json = doc.to_json().unwrap();
    let parsed = Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    let anc = query::ancestors(&p.steps, &p.path.head);
    assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
}

// ── Wire: parse → serialize is value-identity-preserving ────────────

#[test]
fn wire_serde_roundtrip_is_lossless_per_message() {
    let orig: serde_json::Value = serde_json::from_str(REAL).unwrap();
    let typed = real_session();
    let back = serde_json::to_value(&typed.export).unwrap();

    // Whole document first (catches envelope drift)…
    assert_eq!(orig, back, "export document not value-identical");

    // …then per message, so a failure names the culprit line.
    let orig_msgs = orig["messages"].as_array().unwrap();
    let back_msgs = back["messages"].as_array().unwrap();
    assert_eq!(orig_msgs.len(), back_msgs.len());
    for (i, (a, b)) in orig_msgs.iter().zip(back_msgs.iter()).enumerate() {
        assert_eq!(a, b, "message {} not value-identical after serde", i + 1);
    }
}
