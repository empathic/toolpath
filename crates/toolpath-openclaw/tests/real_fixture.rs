//! Tests against REAL OpenClaw sessions captured from the official Docker
//! image (v2026.6.11) via `scripts/openclaw-docker.sh` — not synthesized
//! fixtures. Fixtures live at `test-fixtures/openclaw/` (workspace root):
//!
//! - `convo.jsonl`      — a feature-elicit main session (shell/read/write/edit
//!   tools, one errored result, a `sessions_spawn` delegation, custom entries)
//! - `telegram-dm.jsonl` — a telegram-keyed DM session
//! - `subagent.jsonl`   — the spawned child session
//! - `sessions.json`    — the real routing index tying them together

use std::path::{Path, PathBuf};

use toolpath_convo::{Role, ToolCategory};
use toolpath_openclaw::reader::read_session_from_file;
use toolpath_openclaw::{DeriveConfig, derive_path, session_to_view};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/openclaw")
}

fn read_with_key(name: &str) -> toolpath_openclaw::OpenClawSession {
    let mut s = read_session_from_file(&fixtures().join(name)).expect("fixture parses");
    s.attach_routing_key();
    s
}

#[test]
fn real_main_session_parses_and_classifies() {
    let s = read_with_key("convo.jsonl");
    assert_eq!(s.header.version, 3);
    let v = session_to_view(&s);
    assert_eq!(v.provider_id.as_deref(), Some("openclaw"));

    // Roles as observed: one user prompt, many assistant/toolResult rounds.
    assert!(v.turns.iter().any(|t| t.role == Role::User));
    let n_assistant = v.turns.iter().filter(|t| t.role == Role::Assistant).count();
    assert!(n_assistant >= 5, "elicit run has many assistant turns");

    // Real tool vocabulary classifies: exec (shell), read, write/edit, spawn.
    let cats: Vec<(String, Option<ToolCategory>)> = v
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .map(|tu| (tu.name.clone(), tu.category))
        .collect();
    let has = |name: &str, cat: ToolCategory| {
        cats.iter()
            .any(|(n, c)| n == name && *c == Some(cat))
    };
    assert!(has("exec", ToolCategory::Shell), "exec classifies as Shell: {cats:?}");
    assert!(has("read", ToolCategory::FileRead));
    assert!(has("write", ToolCategory::FileWrite));
    assert!(has("edit", ToolCategory::FileWrite));
    assert!(
        has("sessions_spawn", ToolCategory::Delegation),
        "sessions_spawn classifies as Delegation"
    );

    // The delegation surfaced as DelegatedWork.
    assert!(
        v.turns.iter().any(|t| !t.delegations.is_empty()),
        "sessions_spawn produces a DelegatedWork"
    );

    // The elicit prompt's deliberate missing-file read produced an error.
    assert!(
        v.turns
            .iter()
            .flat_map(|t| &t.tool_uses)
            .any(|tu| tu.result.as_ref().is_some_and(|r| r.is_error)),
        "one errored tool result present"
    );

    // File changes recovered from write/edit tool inputs.
    assert!(!v.files_changed.is_empty());

    // Real per-message usage sums into a session total.
    let total = v.total_usage.expect("usage recorded");
    assert!(total.output_tokens.unwrap_or(0) > 0);
}

#[test]
fn real_telegram_session_gets_channel_actor() {
    let s = read_with_key("telegram-dm.jsonl");
    assert_eq!(
        s.session_key.as_deref(),
        Some("agent:main:telegram:direct:15555550123")
    );
    let path = derive_path(&s, &DeriveConfig::default());
    assert!(
        path.steps
            .iter()
            .any(|st| st.step.actor == "human:telegram/15555550123"),
        "channel-aware human actor derived from a real telegram-keyed session"
    );
    let meta = path.meta.as_ref().unwrap();
    assert_eq!(meta.extra["openclaw"]["channel"], "telegram");
    assert_eq!(meta.extra["openclaw"]["sessionKind"], "direct");
}

#[test]
fn real_subagent_session_is_spawn_child_without_channel_actor() {
    let s = read_with_key("subagent.jsonl");
    let key = s.session_key.as_deref().expect("routing key found");
    assert!(key.starts_with("agent:main:subagent:"), "got {key}");
    let path = derive_path(&s, &DeriveConfig::default());
    let meta = path.meta.as_ref().unwrap();
    assert_eq!(meta.extra["openclaw"]["sessionKind"], "spawn-child");
    // The sub-agent's user turns are the orchestrator's prompt, not a human
    // channel peer — no channel actor may be fabricated.
    assert!(
        !path
            .steps
            .iter()
            .any(|st| st.step.actor.starts_with("human:subagent")),
        "no fake 'subagent' channel actor"
    );
}

#[test]
fn real_sessions_roundtrip_through_projector() {
    use toolpath_convo::ConversationProjector;
    for name in ["convo.jsonl", "telegram-dm.jsonl", "subagent.jsonl"] {
        let s = read_with_key(name);
        let view = session_to_view(&s);
        let projected = toolpath_openclaw::project::OpenClawProjector::default()
            .project(&view)
            .expect("project");
        assert_eq!(projected.header.version, 3);
        for entry in &projected.entries {
            let line = serde_json::to_string(entry).expect("serialize");
            let _: toolpath_openclaw::Entry =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
