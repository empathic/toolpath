//! Compaction-event roundtrip: an opencode session that includes a
//! `compaction` part type in the middle of the conversation should
//! still preserve the surrounding user/assistant content through the
//! projection round-trip.
//!
//! Synthetic fixture is justified per project policy: real compaction
//! events fire when the model context window fills, which can't
//! reliably be triggered by a 5-minute capture prompt.
//!
//! What this test asserts (and why):
//!
//!   - A compacted session loads via the SQLite reader without crashing.
//!   - `to_view` surfaces the compaction part as a `part.compaction`
//!     `ConversationEvent` at its position in the item stream, parented
//!     on the preceding turn (this is the documented contract).
//!   - The event, and the user/assistant content surrounding it, survive
//!     the IR derive/extract round-trip: `derive_path` emits the event
//!     as a `conversation.event` step and `extract_conversation`
//!     restores it, including its `parent_id` (from the `source_parent`
//!     key stamped at derive time).
//!   - The projector emits a functionally equivalent `Session`.

use std::fs;

use rusqlite::Connection;
use tempfile::TempDir;
use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, Item, derive_path, extract_conversation,
};
use toolpath_opencode::project::OpencodeProjector;
use toolpath_opencode::types::{MessageData, PartData};
use toolpath_opencode::{OpencodeConvo, PathResolver, Session, to_view};

/// Mid-session compaction. Schema mirrors `tests/projection_roundtrip.rs`
/// but adds a `compaction` part in the middle of the assistant flow.
const COMPACTION_SQL: &str = r#"
    CREATE TABLE project (
      id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
      icon_url text, icon_color text,
      time_created integer NOT NULL, time_updated integer NOT NULL,
      time_initialized integer, sandboxes text NOT NULL, commands text
    );
    CREATE TABLE session (
      id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
      slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
      version text NOT NULL, share_url text,
      summary_additions integer, summary_deletions integer,
      summary_files integer, summary_diffs text, revert text, permission text,
      time_created integer NOT NULL, time_updated integer NOT NULL,
      time_compacting integer, time_archived integer, workspace_id text
    );
    CREATE TABLE message (
      id text PRIMARY KEY, session_id text NOT NULL,
      time_created integer NOT NULL, time_updated integer NOT NULL,
      data text NOT NULL
    );
    CREATE TABLE part (
      id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
      time_created integer NOT NULL, time_updated integer NOT NULL,
      data text NOT NULL
    );
    INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
      VALUES ('proj-id', '/tmp/proj', 1000, 9000, '[]');
    INSERT INTO session (id, project_id, slug, directory, title, version,
                         time_created, time_updated, time_compacting)
      VALUES ('ses_compact', 'proj-id', 'compaction-demo', '/tmp/proj',
              'Compaction demo', '1.3.10', 1000, 9000, 1500);
    INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
      ('msg_u1','ses_compact',1001,1001,
       '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big"}}'),
      ('msg_a1','ses_compact',1002,1500,
       '{"parentID":"msg_u1","role":"assistant","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":100,"output":20,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1002,"completed":1500},"finish":"stop"}'),
      ('msg_u2','ses_compact',1600,1600,
       '{"role":"user","time":{"created":1600},"agent":"build","model":{"providerID":"opencode","modelID":"big"}}'),
      ('msg_a2','ses_compact',1700,2000,
       '{"parentID":"msg_u2","role":"assistant","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":50,"output":10,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1700,"completed":2000},"finish":"stop"}');
    INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
      ('prt_u1_1','msg_u1','ses_compact',1001,1001,'{"type":"text","text":"refactor the auth module"}'),
      ('prt_a1_1','msg_a1','ses_compact',1002,1002,'{"type":"step-start"}'),
      ('prt_a1_2','msg_a1','ses_compact',1100,1100,'{"type":"text","text":"reading the current auth code"}'),
      ('prt_a1_3','msg_a1','ses_compact',1500,1500,'{"type":"compaction","auto":true,"overflow":true,"tailStartId":"prt_a1_3"}'),
      ('prt_u2_1','msg_u2','ses_compact',1600,1600,'{"type":"text","text":"now add session validation"}'),
      ('prt_a2_1','msg_a2','ses_compact',1700,1700,'{"type":"step-start"}'),
      ('prt_a2_2','msg_a2','ses_compact',1900,1900,'{"type":"text","text":"added session validation to login()"}'),
      ('prt_a2_3','msg_a2','ses_compact',2000,2000,'{"type":"step-finish","reason":"stop","tokens":{"input":50,"output":10,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.01}');
"#;

fn setup_session() -> (TempDir, Session) {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&data).unwrap();
    let conn = Connection::open(data.join("opencode.db")).unwrap();
    conn.execute_batch(COMPACTION_SQL).unwrap();
    drop(conn);
    let resolver = PathResolver::new()
        .with_home(temp.path())
        .with_data_dir(&data);
    let mgr = OpencodeConvo::with_resolver(resolver);
    let session = mgr.read_session("ses_compact").unwrap();
    (temp, session)
}

fn ir_roundtrip(view: &ConversationView) -> ConversationView {
    let path = derive_path(view, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let path = back.into_single_path().expect("single path");
    extract_conversation(&path)
}

#[test]
fn fixture_loads_with_compaction_part() {
    let (_temp, session) = setup_session();
    // Source-level sanity: the compaction part is present in the
    // SQLite-derived Session before any IR conversion.
    let has_compaction = session.messages.iter().any(|m| {
        m.parts
            .iter()
            .any(|p| matches!(p.data, PartData::Compaction(_)))
    });
    assert!(
        has_compaction,
        "fixture should have a Compaction part on the source side"
    );
}

#[test]
fn to_view_surfaces_compaction_as_event() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let event = view.events().find(|e| e.event_type == "part.compaction");
    assert!(
        event.is_some(),
        "expected a `part.compaction` ConversationEvent in view.events(); got: {:?}",
        view.events().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    // In-stream position and parent: the boundary rides on msg_a1's part
    // list, so it lands before msg_a1's turn and parents on msg_u1 (the
    // last turn pushed when the part was reached).
    assert_eq!(event.unwrap().parent_id.as_deref(), Some("msg_u1"));
    let pos = view
        .items
        .iter()
        .position(|i| matches!(i, Item::Event(e) if e.event_type == "part.compaction"))
        .unwrap();
    match &view.items[pos - 1] {
        Item::Turn(t) => assert_eq!(t.id, "msg_u1"),
        other => panic!("expected msg_u1 turn before the boundary, got {other:?}"),
    }
    match &view.items[pos + 1] {
        Item::Turn(t) => assert_eq!(t.id, "msg_a1"),
        other => panic!("expected msg_a1 turn after the boundary, got {other:?}"),
    }
}

#[test]
fn compaction_event_survives_ir_roundtrip() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);

    let pos = after
        .items
        .iter()
        .position(|i| matches!(i, Item::Event(e) if e.event_type == "part.compaction"))
        .expect("part.compaction event dropped by derive → extract roundtrip");

    // Stream position: still between the first user turn and the first
    // assistant turn.
    let before_turn = after.items[..pos]
        .iter()
        .rev()
        .find_map(Item::as_turn)
        .expect("no turn before the boundary after roundtrip");
    assert!(before_turn.text.contains("refactor the auth module"));
    let after_turn = after.items[pos..]
        .iter()
        .find_map(Item::as_turn)
        .expect("no turn after the boundary after roundtrip");
    assert!(after_turn.text.contains("reading the current auth code"));

    // Parent restored from the `source_parent` key stamped at derive time:
    // the boundary still parents on the turn preceding it in the stream.
    let event = after.items[pos].as_event().unwrap();
    assert_eq!(event.parent_id.as_deref(), Some(before_turn.id.as_str()));
}

#[test]
fn pre_compact_user_turn_survives_roundtrip() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);

    let needle = "refactor the auth module";
    assert!(
        view.turns().any(|t| t.text.contains(needle)),
        "pre-compact prompt missing from initial view"
    );
    assert!(
        after.turns().any(|t| t.text.contains(needle)),
        "pre-compact prompt dropped after roundtrip"
    );
}

#[test]
fn post_compact_user_and_assistant_turns_survive_roundtrip() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);

    for needle in [
        "now add session validation",
        "added session validation to login()",
    ] {
        assert!(
            view.turns().any(|t| t.text.contains(needle)),
            "post-compact text {needle:?} missing from initial view"
        );
        assert!(
            after.turns().any(|t| t.text.contains(needle)),
            "post-compact text {needle:?} dropped after roundtrip"
        );
    }
}

#[test]
fn projector_emits_session_with_pre_and_post_compact_messages() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);
    let projector = OpencodeProjector::new()
        .with_directory(session.directory.clone())
        .with_project_id(session.project_id.clone())
        .with_version(session.version.clone());
    let projected: Session = projector.project(&after).expect("project");

    // The projected session must carry both surrounding user prompts and
    // both assistant responses (modulo whatever the compaction part
    // itself becomes — see module-level note).
    let user_count = projected
        .messages
        .iter()
        .filter(|m| matches!(m.data, MessageData::User(_)))
        .count();
    let assistant_count = projected
        .messages
        .iter()
        .filter(|m| matches!(m.data, MessageData::Assistant(_)))
        .count();
    assert!(
        user_count >= 2,
        "expected at least 2 user messages in projected session, got {user_count}"
    );
    assert!(
        assistant_count >= 2,
        "expected at least 2 assistant messages in projected session, got {assistant_count}"
    );

    let projected_text: String = projected
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| {
            if let PartData::Text(t) = &p.data {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "refactor the auth module",
        "now add session validation",
        "added session validation to login()",
    ] {
        assert!(
            projected_text.contains(needle),
            "projected session text missing {needle:?}; got: {projected_text:?}"
        );
    }
}

#[test]
fn projected_session_serdes_symmetrically() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);
    let projector = OpencodeProjector::new()
        .with_directory(session.directory.clone())
        .with_project_id(session.project_id.clone())
        .with_version(session.version.clone());
    let projected: Session = projector.project(&after).expect("project");

    let json = serde_json::to_string(&projected).expect("serialize");
    let _: Session = serde_json::from_str(&json).expect("re-parse");
}
