//! End-to-end projection round-trip:
//! Cursor [`CursorSession`] → [`ConversationView`] → [`Path`]
//! (serialized) → [`ConversationView`] → [`CursorSession`] via
//! [`CursorProjector`].
//!
//! Contract: after the full chain the projected session is
//! *functionally* equivalent to the source — same bubble role
//! sequence, same text, tool invocations preserved by `toolCallId`
//! with matching tool ids/names, file content round-tripped via
//! the synthetic content blob store.

use rusqlite::Connection;
use tempfile::TempDir;
use toolpath::v1::{Graph, Path};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_cursor::project::CursorProjector;
use toolpath_cursor::reader::DbReader;
use toolpath_cursor::session_to_view;
use toolpath_cursor::types::{
    BUBBLE_TYPE_ASSISTANT, BUBBLE_TYPE_USER, CursorSession, TOOL_EDIT_FILE_V2,
    TOOL_RUN_TERMINAL_COMMAND_V2,
};

/// A real-shaped fixture: user prompt, assistant shell command, then
/// assistant `edit_file_v2` that adds `/proj/x.rs` (empty → "hello").
const FIXTURE_SQL: &str = r#"
    CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
    CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);

    INSERT INTO ItemTable (key, value) VALUES
      ('composer.composerHeaders',
       '{"allComposers":[{"type":"head","composerId":"724686cd-875e-47da-a90b-dbc3e523efb8","name":"Pickle a thing","createdAt":1000,"lastUpdatedAt":2500,"unifiedMode":"agent","workspaceIdentifier":{"id":"ws-hash","uri":{"$mid":1,"fsPath":"/proj","external":"file:///proj","path":"/proj","scheme":"file"}}}]}');

    INSERT INTO cursorDiskKV (key, value) VALUES
      ('composerData:724686cd-875e-47da-a90b-dbc3e523efb8',
       '{"_v":16,"composerId":"724686cd-875e-47da-a90b-dbc3e523efb8","name":"Pickle a thing","createdAt":1000,"lastUpdatedAt":2500,"isAgentic":true,"unifiedMode":"agent","agentBackend":"cursor-agent","modelConfig":{"modelName":"default","maxMode":false,"selectedModels":[{"modelId":"default","parameters":[]}]},"fullConversationHeadersOnly":[{"bubbleId":"11111111-1111-4111-a111-111111111111","type":1},{"bubbleId":"22222222-2222-4222-a222-222222222222","type":2},{"bubbleId":"33333333-3333-4333-a333-333333333333","type":2}]}'),
      ('bubbleId:724686cd-875e-47da-a90b-dbc3e523efb8:11111111-1111-4111-a111-111111111111',
       '{"_v":3,"type":1,"bubbleId":"11111111-1111-4111-a111-111111111111","createdAt":"2026-06-01T00:00:00.000Z","text":"make a hello in rust","conversationState":"~"}'),
      ('bubbleId:724686cd-875e-47da-a90b-dbc3e523efb8:22222222-2222-4222-a222-222222222222',
       '{"_v":3,"type":2,"bubbleId":"22222222-2222-4222-a222-222222222222","createdAt":"2026-06-01T00:00:01.000Z","text":"","capabilityType":15,"modelInfo":{"modelName":"claude-opus-4-7"},"tokenCount":{"inputTokens":10,"outputTokens":3},"toolFormerData":{"tool":15,"toolIndex":0,"modelCallId":"","toolCallId":"tool_sh","status":"completed","name":"run_terminal_command_v2","params":"{\"command\":\"ls\"}","result":"{\"output\":\"a\\nb\\n\",\"exitCode\":0,\"rejected\":false,\"notInterrupted\":true}"}}'),
      ('bubbleId:724686cd-875e-47da-a90b-dbc3e523efb8:33333333-3333-4333-a333-333333333333',
       '{"_v":3,"type":2,"bubbleId":"33333333-3333-4333-a333-333333333333","createdAt":"2026-06-01T00:00:02.000Z","text":"","capabilityType":15,"modelInfo":{"modelName":"claude-opus-4-7"},"tokenCount":{"inputTokens":50,"outputTokens":8},"toolFormerData":{"tool":38,"toolIndex":0,"modelCallId":"","toolCallId":"tool_edit","status":"completed","name":"edit_file_v2","params":"{\"relativeWorkspacePath\":\"/proj/x.rs\"}","result":"{\"beforeContentId\":\"composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\",\"afterContentId\":\"composer.content.hello-after-hash\"}"}}'),
      ('composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', ''),
      ('composer.content.hello-after-hash', 'fn main() { println!(\"hello\"); }');
"#;

fn load_source() -> CursorSession {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("state.vscdb");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(FIXTURE_SQL).unwrap();
    drop(conn);
    let reader = DbReader::open(&db_path).unwrap();
    let session = reader
        .load_session("724686cd-875e-47da-a90b-dbc3e523efb8")
        .unwrap();
    // Keep `dir` alive by leaking it for the whole test — the
    // session has already copied everything it needs into memory.
    std::mem::forget(dir);
    session
}

fn roundtrip(source: &CursorSession) -> (ConversationView, CursorSession, Path) {
    let view_forward: ConversationView = session_to_view(source);

    let path = derive_path(&view_forward, &DeriveConfig::default()).expect("derive");
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single path");

    let view_back = extract_conversation(&reparsed);
    let projector = CursorProjector::new()
        .with_composer_id(source.data.composer_id.clone())
        .with_workspace_path(source.workspace_path().unwrap_or_default());
    let rebuilt = projector.project(&view_back).expect("project");
    (view_back, rebuilt, reparsed)
}

#[test]
fn rebuilt_session_preserves_bubble_role_sequence() {
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let kinds_of = |s: &CursorSession| -> Vec<u8> { s.bubbles.iter().map(|b| b.kind).collect() };
    let want = kinds_of(&source);
    assert_eq!(kinds_of(&rebuilt), want);
    assert_eq!(
        want,
        vec![
            BUBBLE_TYPE_USER,
            BUBBLE_TYPE_ASSISTANT,
            BUBBLE_TYPE_ASSISTANT
        ]
    );
}

#[test]
fn rebuilt_session_carries_user_text() {
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    assert_eq!(rebuilt.bubbles[0].text, "make a hello in rust");
}

#[test]
fn rebuilt_session_preserves_tool_call_ids_and_names() {
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let shell = rebuilt.bubbles[1].tool_former_data.as_ref().unwrap();
    assert_eq!(shell.tool_call_id, "tool_sh");
    assert_eq!(shell.tool, TOOL_RUN_TERMINAL_COMMAND_V2);
    assert_eq!(shell.name, "run_terminal_command_v2");
    assert_eq!(shell.status, "completed");

    let edit = rebuilt.bubbles[2].tool_former_data.as_ref().unwrap();
    assert_eq!(edit.tool_call_id, "tool_edit");
    assert_eq!(edit.tool, TOOL_EDIT_FILE_V2);
    assert_eq!(edit.name, "edit_file_v2");
}

#[test]
fn rebuilt_session_round_trips_file_content_via_blob_store() {
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);

    let edit = rebuilt.bubbles[2].tool_former_data.as_ref().unwrap();
    let result = edit.parse_result().unwrap().expect("result present");
    let after_id = result["afterContentId"].as_str().unwrap();
    let hash = after_id
        .strip_prefix("composer.content.")
        .expect("composer.content.* prefix");
    let body = rebuilt
        .content_blob(hash)
        .expect("after blob registered in rebuilt session");
    // SQLite stored the SQL string verbatim including the backslashes
    // (SQL escapes single quotes via `''`, not `\"`); we round-trip
    // whatever the source had, so the comparison uses the same form.
    assert_eq!(body, r#"fn main() { println!(\"hello\"); }"#);

    // Empty before content uses Cursor's canonical SHA-256 sentinel.
    let before_id = result["beforeContentId"].as_str().unwrap();
    assert!(
        before_id.ends_with("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
}

#[test]
fn rebuilt_session_re_lifts_to_equivalent_view() {
    // Cursor → View1 → Path → View2 → Cursor → View3.
    // The double-view shape must be stable: View1 and View3 should
    // agree on roles, texts, tool ids/names, and file mutations.
    let source = load_source();
    let view_forward = session_to_view(&source);
    let (_, rebuilt, _) = roundtrip(&source);
    let view_again = session_to_view(&rebuilt);

    assert_eq!(view_forward.turns().count(), view_again.turns().count());
    for (a, b) in view_forward.turns().zip(view_again.turns()) {
        assert_eq!(a.role, b.role, "role mismatch on turn {}", a.id);
        assert_eq!(a.text, b.text, "text mismatch on turn {}", a.id);
        assert_eq!(
            a.tool_uses.len(),
            b.tool_uses.len(),
            "tool count mismatch on turn {}",
            a.id
        );
        for (ta, tb) in a.tool_uses.iter().zip(b.tool_uses.iter()) {
            assert_eq!(ta.id, tb.id);
            assert_eq!(ta.name, tb.name);
            assert_eq!(ta.category, tb.category);
        }
        assert_eq!(a.file_mutations.len(), b.file_mutations.len());
        for (fa, fb) in a.file_mutations.iter().zip(b.file_mutations.iter()) {
            assert_eq!(fa.path, fb.path);
            assert_eq!(fa.before, fb.before);
            assert_eq!(fa.after, fb.after);
        }
    }
}

#[test]
fn rebuilt_session_workspace_path_preserved() {
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    assert_eq!(rebuilt.workspace_path().unwrap().to_string_lossy(), "/proj");
}

#[test]
fn projector_serializes_to_disk_readable_shape() {
    // Sanity: the projected types serialize back to JSON that the
    // reader's deserializers accept.
    let source = load_source();
    let (_, rebuilt, _) = roundtrip(&source);
    let data_json = serde_json::to_string(&rebuilt.data).unwrap();
    let parsed: toolpath_cursor::types::ComposerData = serde_json::from_str(&data_json).unwrap();
    assert_eq!(parsed.composer_id, rebuilt.data.composer_id);

    for src_b in &rebuilt.bubbles {
        let raw = serde_json::to_string(src_b).unwrap();
        let parsed: toolpath_cursor::types::Bubble = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.bubble_id, src_b.bubble_id);
        assert_eq!(parsed.kind, src_b.kind);
    }
}

/// Demonstrate that a `CursorProjector` can be used as a generic
/// projector for *any* `ConversationView` — including views produced
/// from foreign providers — and that the resulting bubble store
/// still parses back through the reader's types.
#[test]
fn projector_accepts_foreign_view_shape() {
    use serde_json::json;
    use toolpath_convo::{
        EnvironmentSnapshot, FileMutation, Item, ProducerInfo, Role, SessionBase, TokenUsage,
        ToolCategory, ToolInvocation, ToolResult, Turn,
    };

    let view = ConversationView {
        id: "imported-claude-sess".into(),
        provider_id: Some("claude-code".into()),
        producer: Some(ProducerInfo {
            name: "claude-code".into(),
            version: Some("2.1.90".into()),
        }),
        base: Some(SessionBase {
            working_dir: Some("/foreign".into()),
            ..Default::default()
        }),
        items: vec![
            Item::Turn(Turn {
                id: "uA".into(),
                parent_id: None,
                group_id: None,
                role: Role::User,
                timestamp: "2026-06-01T00:00:00Z".into(),
                text: "rename main".into(),
                thinking: None,
                tool_uses: vec![],
                model: None,
                stop_reason: None,
                token_usage: None,
                attributed_token_usage: None,
                environment: Some(EnvironmentSnapshot {
                    working_dir: Some("/foreign".into()),
                    ..Default::default()
                }),
                delegations: vec![],
                file_mutations: vec![],
            }),
            Item::Turn(Turn {
                id: "aA".into(),
                parent_id: Some("uA".into()),
                group_id: None,
                role: Role::Assistant,
                timestamp: "2026-06-01T00:00:01Z".into(),
                text: "done".into(),
                thinking: Some("planning...".into()),
                tool_uses: vec![ToolInvocation {
                    id: "claude_edit".into(),
                    name: "Edit".into(),
                    input: json!({"file_path": "/foreign/main.rs", "old_string": "old", "new_string": "new"}),
                    result: Some(ToolResult {
                        content: "ok".into(),
                        is_error: false,
                    }),
                    category: Some(ToolCategory::FileWrite),
                }],
                model: Some("claude-opus-4-7".into()),
                stop_reason: Some("end_turn".into()),
                token_usage: Some(TokenUsage {
                    input_tokens: Some(20),
                    output_tokens: Some(5),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    ..Default::default()
                }),
                attributed_token_usage: None,
                environment: Some(EnvironmentSnapshot {
                    working_dir: Some("/foreign".into()),
                    ..Default::default()
                }),
                delegations: vec![],
                file_mutations: vec![FileMutation {
                    path: "/foreign/main.rs".into(),
                    tool_id: Some("claude_edit".into()),
                    operation: Some("update".into()),
                    raw_diff: None,
                    before: Some("old\n".into()),
                    after: Some("new\n".into()),
                    rename_to: None,
                }],
            }),
        ],
        ..Default::default()
    };

    let rebuilt = CursorProjector::new().project(&view).unwrap();
    // Foreign tool name should have been remapped onto Cursor's native vocabulary.
    let tf = rebuilt.bubbles[1].tool_former_data.as_ref().unwrap();
    assert_eq!(tf.tool, TOOL_EDIT_FILE_V2);
    assert_eq!(tf.name, "edit_file_v2");
    assert_eq!(tf.tool_call_id, "claude_edit");
    // Both content blobs registered.
    assert_eq!(rebuilt.content_blobs.len(), 2);
}
