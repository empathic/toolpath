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
//!   - `to_view` surfaces the compaction part as an `Item::Compaction`
//!     at its position in `view.items` (this is the documented contract),
//!     not a generic `ConversationEvent`.
//!   - The compaction boundary survives the IR derive/extract round-trip
//!     as a `conversation.compact` step, and the user/assistant content
//!     surrounding it survives too, with the projector emitting a
//!     functionally equivalent `Session`.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;
use toolpath::v1::Graph;
use toolpath_convo::{
    CompactionTrigger, ConversationProjector, ConversationView, DeriveConfig, Item, derive_path,
    extract_conversation,
};
use toolpath_opencode::project::OpencodeProjector;
use toolpath_opencode::types::{Message, MessageData, Part, PartData, Session};
use toolpath_opencode::{OpencodeConvo, PathResolver, to_view};

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
fn to_view_surfaces_compaction_as_compaction_item() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);

    assert!(
        !view.events().any(|e| e.event_type == "part.compaction"),
        "compaction should no longer surface as a generic event"
    );

    let compactions: Vec<_> = view.compactions().collect();
    assert_eq!(
        compactions.len(),
        1,
        "expected exactly one Item::Compaction; got {}",
        compactions.len()
    );
    let c = compactions[0];
    // The synthetic SQL fixture's compaction part has `auto: true`.
    assert_eq!(c.trigger, Some(CompactionTrigger::Auto));
    assert!(
        c.parent_id.is_some(),
        "compaction should parent on the prior turn"
    );
    // `tailStartId` is present in the SQL fixture, so a kept range is set.
    assert!(!c.kept.is_empty(), "tailStartId present ⇒ kept range");
}

#[test]
fn compaction_item_survives_derive_extract() {
    let (_temp, session) = setup_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);

    let before_count = view.compactions().count();
    let after_count = after.compactions().count();
    assert_eq!(
        before_count, after_count,
        "compaction count changed across round-trip: {before_count} → {after_count}"
    );
    assert_eq!(after_count, 1, "the compaction boundary should survive");
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

// ── Real-fixture assertions ────────────────────────────────────────────
//
// `test-fixtures/opencode/convo-compacted.json` is a captured opencode
// session with a real manual `/compact` boundary (a synthetic
// compaction-bearing user message, `auto: false`, no `tailStartId`).
// It exercises the user-message compaction path that the synthetic SQL
// fixture above (an assistant-message compaction) doesn't.

fn compacted_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("opencode")
        .join("convo-compacted.json")
}

/// Translate opencode's `path export` wrapper (camelCase + nested `info`)
/// into the flat snake-case `Session` shape `to_view` expects. Mirrors the
/// helper in `tests/real_fixture_roundtrip.rs`.
fn parse_opencode_export(json: &str) -> Session {
    let v: Value = serde_json::from_str(json).expect("opencode wrapper parse");
    let info = &v["info"];
    let msgs_in = v["messages"].as_array().cloned().unwrap_or_default();

    let str_or = |key: &str, fallback: &str| -> String {
        info.get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let i64_at = |path: &[&str]| -> Option<i64> {
        let mut cur = info;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_i64()
    };

    let mut messages: Vec<Message> = Vec::with_capacity(msgs_in.len());
    for m in msgs_in {
        let mi = m.get("info").cloned().unwrap_or(Value::Null);
        let mi_obj = mi.as_object().cloned().unwrap_or_default();
        let id = mi_obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = mi_obj
            .get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let time_created = mi_obj
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let mut data_obj = mi_obj.clone();
        data_obj.remove("id");
        data_obj.remove("sessionID");
        let data: MessageData =
            serde_json::from_value(Value::Object(data_obj)).unwrap_or(MessageData::Other);

        let mut parts: Vec<Part> = Vec::new();
        if let Some(parts_in) = m.get("parts").and_then(Value::as_array) {
            for p in parts_in {
                let p_obj = p.as_object().cloned().unwrap_or_default();
                let pid = p_obj
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let pmsg = p_obj
                    .get("messageID")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                let psess = p_obj
                    .get("sessionID")
                    .and_then(Value::as_str)
                    .unwrap_or(&session_id)
                    .to_string();
                let mut data_obj = p_obj.clone();
                data_obj.remove("id");
                data_obj.remove("messageID");
                data_obj.remove("sessionID");
                let part_data: PartData =
                    serde_json::from_value(Value::Object(data_obj)).unwrap_or(PartData::Unknown);
                parts.push(Part {
                    id: pid,
                    message_id: pmsg,
                    session_id: psess,
                    time_created,
                    time_updated: time_created,
                    data: part_data,
                });
            }
        }

        messages.push(Message {
            id,
            session_id,
            time_created,
            time_updated: time_created,
            data,
            parts,
        });
    }

    Session {
        id: str_or("id", ""),
        project_id: str_or("projectID", ""),
        workspace_id: info
            .get("workspaceID")
            .and_then(Value::as_str)
            .map(str::to_string),
        parent_id: info
            .get("parentID")
            .and_then(Value::as_str)
            .map(str::to_string),
        slug: str_or("slug", ""),
        directory: PathBuf::from(str_or("directory", "/")),
        title: str_or("title", ""),
        version: str_or("version", "0.0.0"),
        share_url: info
            .get("shareURL")
            .and_then(Value::as_str)
            .map(str::to_string),
        summary_additions: i64_at(&["summary", "additions"]),
        summary_deletions: i64_at(&["summary", "deletions"]),
        summary_files: i64_at(&["summary", "files"]),
        time_created: i64_at(&["time", "created"]).unwrap_or(0),
        time_updated: i64_at(&["time", "updated"])
            .or_else(|| i64_at(&["time", "created"]))
            .unwrap_or(0),
        time_compacting: i64_at(&["time", "compacting"]),
        time_archived: i64_at(&["time", "archived"]),
        messages,
    }
}

fn load_compacted_fixture_session() -> Session {
    let json = std::fs::read_to_string(compacted_fixture_path()).expect("read compacted fixture");
    parse_opencode_export(&json)
}

#[test]
fn real_fixture_emits_one_manual_compaction_item() {
    let session = load_compacted_fixture_session();
    let view = to_view(&session);

    let compactions: Vec<_> = view.compactions().collect();
    assert_eq!(
        compactions.len(),
        1,
        "expected exactly one Item::Compaction in the real fixture; got {}",
        compactions.len()
    );
    let c = compactions[0];
    // The fixture's `/compact` boundary has `auto: false` ⇒ Manual.
    assert_eq!(c.trigger, Some(CompactionTrigger::Manual));
    // No `tailStartId` and no synthetic summary message in this fixture.
    assert!(
        c.kept.is_empty(),
        "no tailStartId ⇒ empty kept range; got {:?}",
        c.kept
    );
    assert!(
        c.parent_id.is_some(),
        "compaction should parent on the turn before it"
    );

    // The compaction is positioned mid-stream, with turns on both sides.
    let compaction_idx = view
        .items
        .iter()
        .position(|i| matches!(i, Item::Compaction(_)))
        .expect("a Compaction item");
    assert!(
        view.items[..compaction_idx]
            .iter()
            .any(|i| matches!(i, Item::Turn(_))),
        "expected turns before the compaction"
    );
    assert!(
        view.items[compaction_idx + 1..]
            .iter()
            .any(|i| matches!(i, Item::Turn(_))),
        "expected turns after the compaction"
    );
}

#[test]
fn projector_reproduces_compaction_item_through_to_view() {
    // Projection round-trip: source Session → view → project → Session →
    // re-read view. Exactly one `Item::Compaction` must survive, carrying
    // the fixture's manual trigger, and it must land between turns — i.e.
    // the projector's inverse of the forward `compaction`-part mapping.
    let source = load_compacted_fixture_session();
    let view = to_view(&source);
    assert_eq!(
        view.compactions().count(),
        1,
        "source view should have exactly one Item::Compaction"
    );

    let projector = OpencodeProjector::new()
        .with_directory(source.directory.clone())
        .with_project_id(source.project_id.clone())
        .with_version(source.version.clone());
    let projected: Session = projector.project(&view).expect("project");

    // The projected Session must carry a `compaction` part so a re-read
    // reproduces the boundary.
    let has_compaction_part = projected.messages.iter().any(|m| {
        m.parts
            .iter()
            .any(|p| matches!(p.data, PartData::Compaction(_)))
    });
    assert!(
        has_compaction_part,
        "projected session should carry a compaction part"
    );

    let reread = to_view(&projected);
    let compactions: Vec<_> = reread.compactions().collect();
    assert_eq!(
        compactions.len(),
        1,
        "exactly one Item::Compaction should survive the projection round-trip; got {}",
        compactions.len()
    );
    assert_eq!(
        compactions[0].trigger,
        Some(CompactionTrigger::Manual),
        "manual trigger (auto=false) should survive the projection round-trip"
    );

    // Positioned between turns: turns on both sides of the boundary.
    let idx = reread
        .items
        .iter()
        .position(|i| matches!(i, Item::Compaction(_)))
        .expect("a Compaction item in the re-read view");
    assert!(
        reread.items[..idx]
            .iter()
            .any(|i| matches!(i, Item::Turn(_))),
        "expected turns before the compaction in the re-read view"
    );
    assert!(
        reread.items[idx + 1..]
            .iter()
            .any(|i| matches!(i, Item::Turn(_))),
        "expected turns after the compaction in the re-read view"
    );
}

#[test]
fn real_fixture_compaction_and_surrounding_turns_survive_roundtrip() {
    let session = load_compacted_fixture_session();
    let view = to_view(&session);
    let after = ir_roundtrip(&view);

    assert_eq!(
        view.compactions().count(),
        after.compactions().count(),
        "compaction count diverged across round-trip"
    );
    assert_eq!(
        after.compactions().count(),
        1,
        "the manual compaction boundary should survive the round-trip"
    );
    assert_eq!(
        after.compactions().next().unwrap().trigger,
        Some(CompactionTrigger::Manual),
        "trigger should survive as Manual"
    );

    // Surrounding turns (pre- and post-compaction) survive intact.
    let before_turns = view.turns().count();
    let after_turns = after.turns().count();
    assert_eq!(
        before_turns, after_turns,
        "turn count diverged across round-trip: {before_turns} → {after_turns}"
    );
    assert!(before_turns >= 2, "fixture should have multiple turns");
}
