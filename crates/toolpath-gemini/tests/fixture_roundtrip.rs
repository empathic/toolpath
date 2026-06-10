//! Integration test exercising the full parse → view → derive pipeline
//! against a real Gemini CLI chat file.

use toolpath_convo::{ConversationProvider, Role, ToolCategory};
use toolpath_gemini::{GeminiConvo, PathResolver, derive};

const FIXTURE: &str = include_str!("fixtures/sample_subagent.json");

fn write_session(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let gemini = dir.join(".gemini");
    let session_dir = gemini.join("tmp/myrepo/chats/session-uuid");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        gemini.join("projects.json"),
        r#"{"projects":{"/Users/ben/empathic/oss/toolpath":"myrepo"}}"#,
    )
    .unwrap();
    std::fs::write(session_dir.join("sample_subagent.json"), FIXTURE).unwrap();
    (gemini, session_dir)
}

#[test]
fn fixture_parses_successfully() {
    let chat: toolpath_gemini::ChatFile = serde_json::from_str(FIXTURE).unwrap();
    // Basic shape checks from the observed file
    assert_eq!(chat.session_id, "qclszz");
    assert!(!chat.project_hash.is_empty());
    assert_eq!(chat.kind.as_deref(), Some("subagent"));
    assert!(!chat.messages.is_empty());
}

#[test]
fn fixture_load_via_provider() {
    let temp = tempfile::tempdir().unwrap();
    let (gemini, _sd) = write_session(temp.path());

    let mgr = GeminiConvo::with_resolver(PathResolver::new().with_gemini_dir(&gemini));
    let view = ConversationProvider::load_conversation(
        &mgr,
        "/Users/ben/empathic/oss/toolpath",
        "session-uuid",
    )
    .unwrap();

    // Provider id set
    assert_eq!(view.provider_id.as_deref(), Some("gemini-cli"));
    // User and assistant turns both present
    let user_turns = view.turns().filter(|t| t.role == Role::User).count();
    let assistant_turns = view
        .turns()
        .filter(|t| t.role == Role::Assistant)
        .count();
    assert!(user_turns >= 1);
    assert!(assistant_turns >= 1);

    // At least one FileRead-categorised tool (get_internal_docs)
    let has_file_read = view.turns().any(|t| {
        t.tool_uses
            .iter()
            .any(|tu| tu.category == Some(ToolCategory::FileRead))
    });
    assert!(has_file_read);
}

#[test]
fn fixture_derives_to_valid_path() {
    let temp = tempfile::tempdir().unwrap();
    let (gemini, _sd) = write_session(temp.path());

    let mgr = GeminiConvo::with_resolver(PathResolver::new().with_gemini_dir(&gemini));
    let convo = mgr
        .read_conversation("/Users/ben/empathic/oss/toolpath", "session-uuid")
        .unwrap();

    let path = derive::derive_path(
        &convo,
        &derive::DeriveConfig {
            project_path: Some("/Users/ben/empathic/oss/toolpath".into()),
            include_thinking: false,
        },
    );
    let doc = toolpath::v1::Graph::from_path(path);
    let json = doc.to_json().unwrap();
    // Roundtrip verifies serde is well-formed
    let parsed = toolpath::v1::Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    assert!(p.path.id.starts_with("path-gemini-"));
    assert!(!p.steps.is_empty());
    // Ancestors of head must cover all steps in a linear session
    let ancestors = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
    assert_eq!(ancestors.len(), p.steps.len());
}
