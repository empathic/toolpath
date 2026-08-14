//! Integration: parse the synthetic `events.jsonl` fixture, build a
//! `ConversationView`, derive a `Path`, and assert the end-to-end shape.
//!
//! ⚠️ The fixture is *synthetic*, built from the reverse-engineered schema in
//! `docs/agents/formats/copilot-cli/` — there is no first-hand Copilot session
//! to capture in this environment. When one becomes available, replace this
//! fixture with the real capture (see the feature-elicit flow) and re-grade.

use std::fs;
use tempfile::TempDir;
use toolpath::v1::{Graph, PATH_KIND_AGENT_CODING_SESSION, query};
use toolpath_convo::{Role, ToolCategory};
use toolpath_copilot::{CopilotConvo, PathResolver, derive};

/// Lay the fixture out as `~/.copilot/session-state/<id>/events.jsonl` in a
/// temp dir and return a manager pointed at it.
fn setup() -> (TempDir, CopilotConvo, String) {
    let temp = TempDir::new().unwrap();
    let copilot = temp.path().join(".copilot");
    let id = "demo-019dabc6-session";
    let dir = copilot.join("session-state").join(id);
    fs::create_dir_all(&dir).unwrap();
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sample-session.jsonl"
    );
    fs::copy(fixture, dir.join("events.jsonl")).unwrap();
    let resolver = PathResolver::new(temp.path()).with_copilot_dir(&copilot);
    (temp, CopilotConvo::with_resolver(resolver), id.to_string())
}

#[test]
fn lists_and_reads_the_session() {
    let (_t, convo, id) = setup();
    let sessions = convo.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);
    assert_eq!(sessions[0].cwd.as_deref(), Some("/tmp/demo"));
    assert_eq!(
        sessions[0].first_user_message.as_deref(),
        Some("Set up a tiny Rust project and check it for TODOs.")
    );

    let session = convo.read_session("demo").unwrap(); // prefix resolution
    assert_eq!(session.id, id);
}

#[test]
fn view_has_expected_turns_and_tools() {
    let (_t, convo, id) = setup();
    let session = convo.read_session(&id).unwrap();
    let view = toolpath_copilot::to_view(&session);

    // One user turn, one collapsed assistant turn.
    assert_eq!(view.turns.len(), 2);
    assert_eq!(view.turns[0].role, Role::User);
    assert_eq!(view.turns[1].role, Role::Assistant);

    let tools = &view.turns[1].tool_uses;
    assert_eq!(
        tools.len(),
        5,
        "shell, create_file, 2x read_file, grep_search"
    );

    // Categories classified.
    assert_eq!(
        tools.iter().find(|t| t.name == "bash").unwrap().category,
        Some(ToolCategory::Shell)
    );
    assert_eq!(
        tools
            .iter()
            .find(|t| t.name == "create_file")
            .unwrap()
            .category,
        Some(ToolCategory::FileWrite)
    );
    assert_eq!(
        tools
            .iter()
            .find(|t| t.name == "grep_search")
            .unwrap()
            .category,
        Some(ToolCategory::FileSearch)
    );

    // The errored read is flagged.
    let errored = tools
        .iter()
        .filter(|t| t.name == "read_file")
        .find(|t| t.result.as_ref().map(|r| r.is_error).unwrap_or(false))
        .expect("an errored read_file");
    assert!(
        errored
            .result
            .as_ref()
            .unwrap()
            .content
            .contains("no such file")
    );
}

#[test]
fn view_has_delegation_skill_and_usage() {
    let (_t, convo, id) = setup();
    let session = convo.read_session(&id).unwrap();
    let view = toolpath_copilot::to_view(&session);

    // Sub-agent → delegation with result back-filled.
    let d = &view.turns[1].delegations[0];
    assert_eq!(d.agent_id, "sub-1");
    assert_eq!(d.result.as_deref(), Some("looks good, no issues"));

    // Skill + session.task_complete land as non-turn events.
    assert!(view.events.iter().any(|e| e.event_type == "skill.invoked"));
    assert!(
        view.events
            .iter()
            .any(|e| e.event_type == "session.task_complete")
    );

    // Session output total = sum of per-message outputTokens (120+90+60),
    // no per-message input in an open session (no session.shutdown).
    let u = view.total_usage.as_ref().unwrap();
    assert_eq!(u.output_tokens, Some(270));
    assert_eq!(u.input_tokens, None);

    // File change tracked.
    assert_eq!(view.files_changed, vec!["src/main.rs".to_string()]);
}

#[test]
fn derives_a_valid_single_path_graph() {
    let (_t, convo, id) = setup();
    let session = convo.read_session(&id).unwrap();
    let path = derive::derive_path(&session, &derive::DeriveConfig::default());

    assert!(path.path.id.starts_with("path-copilot-"));
    assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///tmp/demo");
    assert_eq!(
        path.meta.as_ref().unwrap().kind.as_deref(),
        Some(PATH_KIND_AGENT_CODING_SESSION)
    );

    // The file write projects to a sibling artifact with a raw diff.
    let file_step = path
        .steps
        .iter()
        .find(|s| s.change.contains_key("src/main.rs"))
        .expect("a step carries the file artifact");
    let change = &file_step.change["src/main.rs"];
    assert!(change.raw.as_ref().unwrap().contains("+fn main() {"));
    assert_eq!(
        change.structural.as_ref().unwrap().change_type,
        "file.write"
    );

    // Round-trips through JSON as a single-path graph with every step on the
    // head's ancestry.
    let doc = Graph::from_path(path);
    let json = doc.to_json().unwrap();
    let parsed = Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    let anc = query::ancestors(&p.steps, &p.path.head);
    assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
}
