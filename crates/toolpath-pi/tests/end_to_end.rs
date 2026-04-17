//! End-to-end tests: build a realistic Pi session fixture on disk,
//! read it through PiConvo, and derive a Toolpath Path.

use std::fs;
use std::path::Path;
use tempfile::TempDir;
use toolpath_convo::ConversationProvider;
use toolpath_pi::{DeriveConfig, PathResolver, PiConvo};

const PROJECT_CWD: &str = "/Users/alex/demoproject";

/// Write a fixture JSONL for a small, realistic Pi session.
fn write_fixture(sessions_dir: &Path) {
    let encoded = "--Users-alex-demoproject--";
    let proj = sessions_dir.join(encoded);
    fs::create_dir_all(&proj).unwrap();

    let lines = [
        r#"{"type":"session","version":3,"id":"demo-session-1","timestamp":"2026-04-16T10:00:00Z","cwd":"/Users/alex/demoproject"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-04-16T10:00:01Z","message":{"role":"user","content":"Write hello.rs","timestamp":1744797601000}}"#,
        r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-04-16T10:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll write the file."},{"type":"toolCall","id":"tc-1","name":"write","arguments":{"path":"hello.rs","content":"fn main(){}"}}],"api":"anthropic","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":1744797602000}}"#,
        r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-04-16T10:00:03Z","message":{"role":"toolResult","toolCallId":"tc-1","toolName":"write","content":[{"type":"text","text":"file written"}],"isError":false,"timestamp":1744797603000}}"#,
        r#"{"type":"message","id":"m4","parentId":"m3","timestamp":"2026-04-16T10:00:04Z","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"api":"anthropic","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":120,"output":10,"cacheRead":100,"cacheWrite":0,"totalTokens":130,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":1744797604000}}"#,
    ];

    fs::write(
        proj.join("2026-04-16_demo-session-1.jsonl"),
        lines.join("\n"),
    )
    .unwrap();
}

fn manager_for(temp: &TempDir) -> PiConvo {
    let sessions_dir = temp.path().join(".pi/agent/sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let resolver = PathResolver::new().with_sessions_dir(&sessions_dir);
    PiConvo::with_resolver(resolver)
}

#[test]
fn test_list_projects_finds_fixture() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let projects = manager.list_projects().unwrap();
    assert_eq!(projects, vec![PROJECT_CWD.to_string()]);
}

#[test]
fn test_list_sessions_finds_fixture() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let sessions = manager.list_sessions(PROJECT_CWD).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "demo-session-1");
}

#[test]
fn test_read_session_parses_fixture() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let session = manager.read_session(PROJECT_CWD, "demo-session-1").unwrap();
    assert_eq!(session.header.id, "demo-session-1");
    // 1 header + 4 messages
    assert_eq!(session.entries.len(), 5);
}

#[test]
fn test_to_view_produces_expected_turns() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let session = manager.read_session(PROJECT_CWD, "demo-session-1").unwrap();
    let view = manager.to_view(&session);

    // Turn count: user + assistant + toolResult + assistant = 4
    assert_eq!(view.turns.len(), 4);
    assert_eq!(view.provider_id.as_deref(), Some("pi"));
    // files_changed should include "hello.rs"
    assert!(view.files_changed.iter().any(|f| f == "hello.rs"));
    // total_usage should aggregate both assistant turns: 100+120=220 input
    let total = view.total_usage.expect("expected total_usage");
    assert_eq!(total.input_tokens, Some(220));
}

#[test]
fn test_derive_path_from_fixture() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let session = manager.read_session(PROJECT_CWD, "demo-session-1").unwrap();

    let path = toolpath_pi::derive_path(&session, &DeriveConfig::default());

    // Path has 4 steps (one per turn).
    assert_eq!(path.steps.len(), 4);
    // Path ID format.
    assert!(path.path.id.starts_with("path-pi-"));
    // Head points at the last step.
    assert_eq!(path.path.head, "step-0004");
    // Base URI derived from cwd.
    assert!(
        path.path
            .base
            .as_ref()
            .map(|b| b.uri.as_str())
            .unwrap_or("")
            .contains("/Users/alex/demoproject")
    );
    // files_changed makes it into meta.extra.
    let meta = path.meta.as_ref().expect("meta");
    let fc = meta.extra.get("files_changed").expect("files_changed");
    assert!(fc.to_string().contains("hello.rs"));
}

#[test]
fn test_derive_roundtrip_serde() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let session = manager.read_session(PROJECT_CWD, "demo-session-1").unwrap();

    let path = toolpath_pi::derive_path(&session, &DeriveConfig::default());
    let doc = toolpath::v1::Document::Path(path);
    let json = doc.to_json_pretty().unwrap();
    let parsed = toolpath::v1::Document::from_json(&json).unwrap();
    // Compare as structured JSON values — HashMap-based `extra`/`actors` have
    // non-deterministic key order when re-serialized, so a string compare is flaky.
    let a: serde_json::Value = serde_json::from_str(&json).unwrap();
    let b: serde_json::Value = serde_json::from_str(&parsed.to_json_pretty().unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn test_conversation_provider_impl() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());

    let ids = manager.list_conversations(PROJECT_CWD).unwrap();
    assert_eq!(ids, vec!["demo-session-1".to_string()]);

    let view = manager
        .load_conversation(PROJECT_CWD, "demo-session-1")
        .unwrap();
    assert_eq!(view.id, "demo-session-1");

    let meta = manager
        .load_metadata(PROJECT_CWD, "demo-session-1")
        .unwrap();
    assert_eq!(meta.id, "demo-session-1");

    let all = manager.list_metadata(PROJECT_CWD).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn test_most_recent_session() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    write_fixture(manager.resolver().sessions_dir());
    let session = manager.most_recent_session(PROJECT_CWD).unwrap().unwrap();
    assert_eq!(session.header.id, "demo-session-1");
}

#[test]
fn test_most_recent_session_none_for_empty_project() {
    let temp = TempDir::new().unwrap();
    let manager = manager_for(&temp);
    let sessions_dir = manager.resolver().sessions_dir();
    fs::create_dir_all(sessions_dir.join("--empty--")).unwrap();
    let result = manager.most_recent_session("/empty").unwrap();
    assert!(result.is_none());
}
