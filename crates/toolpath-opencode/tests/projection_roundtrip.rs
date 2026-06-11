//! End-to-end projection round-trip:
//! opencode `Session` → `ConversationView` → `Path` (serialized) →
//! `ConversationView` → `Session` via [`OpencodeProjector`].
//!
//! Contract: after the full chain the projected session is
//! *functionally* equivalent to the source — same user/assistant
//! messages, same text, tool invocations preserved by call_id with
//! matching tool names and outputs.

use std::fs;
use tempfile::TempDir;

use rusqlite::Connection;
use toolpath::v1::{Graph, Path};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_opencode::project::OpencodeProjector;
use toolpath_opencode::types::{MessageData, PartData, ToolState};
use toolpath_opencode::{OpencodeConvo, PathResolver, Session, to_view};

const BASIC_SQL: &str = r#"
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
                         time_created, time_updated)
      VALUES ('ses_pickle', 'proj-id', 'crisp-nebula', '/tmp/proj',
              'Pickle demo', '1.3.10', 1000, 9000);
    INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
      ('msg_u1','ses_pickle',1001,1001,
       '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
      ('msg_a1','ses_pickle',1002,1500,
       '{"parentID":"msg_u1","role":"assistant","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1002,"completed":1500},"finish":"stop"}'),
      ('msg_u2','ses_pickle',1600,1600,
       '{"role":"user","time":{"created":1600},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
      ('msg_a2','ses_pickle',1700,2000,
       '{"parentID":"msg_u2","role":"assistant","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":50,"output":10,"reasoning":0,"cache":{"read":5,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1700,"completed":2000},"finish":"stop"}');
    INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
      ('prt_u1_1','msg_u1','ses_pickle',1001,1001,'{"type":"text","text":"build a pickle in c++"}'),
      ('prt_a1_1','msg_a1','ses_pickle',1002,1002,'{"type":"step-start","snapshot":"snap_a"}'),
      ('prt_a1_2','msg_a1','ses_pickle',1100,1100,'{"type":"reasoning","text":"need to set up a Cargo project"}'),
      ('prt_a1_3','msg_a1','ses_pickle',1200,1200,'{"type":"tool","tool":"bash","callID":"call_ls","state":{"status":"completed","input":{"command":"ls"},"output":"src\nCargo.toml\n","title":"List files","metadata":{"exit":0},"time":{"start":1200,"end":1210}}}'),
      ('prt_a1_4','msg_a1','ses_pickle',1300,1300,'{"type":"tool","tool":"write","callID":"call_w","state":{"status":"completed","input":{"filePath":"/tmp/proj/main.cpp","content":"int main(){}\n"},"output":"wrote","title":"Write main","metadata":{"bytes":13},"time":{"start":1300,"end":1310}}}'),
      ('prt_a1_5','msg_a1','ses_pickle',1400,1400,'{"type":"text","text":"created the file"}'),
      ('prt_a1_6','msg_a1','ses_pickle',1500,1500,'{"type":"step-finish","reason":"stop","snapshot":"snap_b","tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":10,"write":0}},"cost":0.01}'),
      ('prt_u2_1','msg_u2','ses_pickle',1600,1600,'{"type":"text","text":"now run it"}'),
      ('prt_a2_1','msg_a2','ses_pickle',1700,1700,'{"type":"step-start"}'),
      ('prt_a2_2','msg_a2','ses_pickle',1800,1800,'{"type":"tool","tool":"bash","callID":"call_run","state":{"status":"error","input":{"command":"./pickle"},"error":"command not found","time":{"start":1800,"end":1810}}}'),
      ('prt_a2_3','msg_a2','ses_pickle',1900,1900,'{"type":"text","text":"ah, I need to compile first"}'),
      ('prt_a2_4','msg_a2','ses_pickle',2000,2000,'{"type":"step-finish","reason":"stop","tokens":{"input":50,"output":10,"reasoning":0,"cache":{"read":5,"write":0}},"cost":0.01}');
"#;

fn setup_session() -> (TempDir, Session) {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&data).unwrap();
    let conn = Connection::open(data.join("opencode.db")).unwrap();
    conn.execute_batch(BASIC_SQL).unwrap();
    drop(conn);
    let resolver = PathResolver::new()
        .with_home(temp.path())
        .with_data_dir(&data);
    let mgr = OpencodeConvo::with_resolver(resolver);
    let session = mgr.read_session("ses_pickle").unwrap();
    (temp, session)
}

fn roundtrip(source: &Session) -> (ConversationView, Session, Path) {
    let view_forward: ConversationView = to_view(source);

    let path = derive_path(&view_forward, &DeriveConfig::default()).expect("derive");
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single path");

    let view_back = extract_conversation(&reparsed);
    let projector = OpencodeProjector::new()
        .with_directory(source.directory.clone())
        .with_project_id(source.project_id.clone())
        .with_version(source.version.clone());
    let rebuilt = projector.project(&view_back).expect("project");
    (view_back, rebuilt, reparsed)
}

#[test]
fn rebuilt_session_keeps_message_role_sequence() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);

    let role_seq = |s: &Session| -> Vec<&'static str> {
        s.messages
            .iter()
            .map(|m| match m.data {
                MessageData::User(_) => "user",
                MessageData::Assistant(_) => "assistant",
                MessageData::Other => "other",
            })
            .collect()
    };
    assert_eq!(role_seq(&rebuilt), role_seq(&source));
}

#[test]
fn rebuilt_session_carries_directory_and_project() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);
    assert_eq!(rebuilt.directory, source.directory);
    assert_eq!(rebuilt.project_id, source.project_id);
    assert_eq!(rebuilt.version, source.version);
}

#[test]
fn user_text_survives_round_trip() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);
    let user_texts = |s: &Session| -> Vec<String> {
        s.messages
            .iter()
            .filter(|m| matches!(m.data, MessageData::User(_)))
            .map(|m| {
                m.parts
                    .iter()
                    .filter_map(|p| match &p.data {
                        PartData::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .collect()
    };
    assert_eq!(user_texts(&rebuilt), user_texts(&source));
}

#[test]
fn assistant_tool_calls_paired_by_call_id() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);

    let collect = |s: &Session| -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for m in &s.messages {
            for p in &m.parts {
                if let PartData::Tool(tp) = &p.data {
                    let output = match &tp.state {
                        ToolState::Completed(c) => c.output.clone(),
                        ToolState::Error(e) => e.error.clone(),
                        _ => String::new(),
                    };
                    out.push((tp.call_id.clone(), tp.tool.clone(), output));
                }
            }
        }
        out.sort();
        out
    };

    let src = collect(&source);
    let rb = collect(&rebuilt);
    assert_eq!(rb.len(), src.len(), "tool count: {:?} vs {:?}", src, rb);
    for (s, r) in src.iter().zip(rb.iter()) {
        assert_eq!(s.0, r.0, "call_id");
        assert_eq!(s.1, r.1, "tool name");
        assert_eq!(s.2, r.2, "output");
    }
}

#[test]
fn errored_tool_state_round_trips() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);

    let has_error = |s: &Session| -> bool {
        s.messages.iter().any(|m| {
            m.parts.iter().any(|p| {
                matches!(
                    p.data,
                    PartData::Tool(ref tp) if matches!(tp.state, ToolState::Error(_))
                )
            })
        })
    };
    assert!(has_error(&source), "fixture should have an errored tool");
    assert!(has_error(&rebuilt), "round-trip dropped the error state");
}

#[test]
fn assistant_messages_have_step_start_and_step_finish() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);

    for m in &rebuilt.messages {
        if matches!(m.data, MessageData::Assistant(_)) {
            let kinds: Vec<&str> = m.parts.iter().map(|p| p.data.kind()).collect();
            assert_eq!(
                kinds.first().copied(),
                Some("step-start"),
                "first part should be step-start, got {:?}",
                kinds
            );
            assert_eq!(
                kinds.last().copied(),
                Some("step-finish"),
                "last part should be step-finish, got {:?}",
                kinds
            );
        }
    }
}

#[test]
fn assistant_thinking_emits_reasoning_part() {
    let (_t, source) = setup_session();
    let (_, rebuilt, _) = roundtrip(&source);

    let count_reasoning = |s: &Session| -> usize {
        s.messages
            .iter()
            .map(|m| {
                m.parts
                    .iter()
                    .filter(|p| matches!(p.data, PartData::Reasoning(_)))
                    .count()
            })
            .sum()
    };
    let src = count_reasoning(&source);
    let rb = count_reasoning(&rebuilt);
    assert!(rb >= src.min(1), "reasoning lost: src={}, rb={}", src, rb);
}
