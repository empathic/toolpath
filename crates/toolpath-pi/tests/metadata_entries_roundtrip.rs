//! Metadata-entry round-trip: Pi's `ModelChange` / `ThinkingLevelChange` /
//! `Label` entries have no `Turn` mapping, so the forward path routes them
//! into `ConversationView.events`. The shared derive emits those as
//! `conversation.event` steps, `extract_conversation` restores them, and
//! `PiProjector` re-materializes them as real Pi entries — so a
//! pi → view → Path → view → pi chain preserves the entries (ids,
//! parentIds, payload fields) instead of dropping them.
//!
//! Synthetic fixture is justified per project policy: model changes and
//! labels are user-initiated UI actions that a capture prompt can't
//! reliably trigger mid-session.

use std::collections::HashMap;

use toolpath::v1::Graph;
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_pi::project::PiProjector;
use toolpath_pi::reader::PiSession;
use toolpath_pi::session_to_view;
use toolpath_pi::types::{
    AgentMessage, ContentBlock, CostBreakdown, Entry, EntryBase, KnownStopReason, MessageContent,
    SessionHeader, StopReason, Usage,
};

fn base(id: &str, parent: Option<&str>, ts: &str) -> EntryBase {
    EntryBase {
        id: id.into(),
        parent_id: parent.map(String::from),
        timestamp: ts.into(),
    }
}

fn source_session() -> PiSession {
    let header = SessionHeader {
        version: 3,
        id: "sess-meta".into(),
        timestamp: "2026-04-16T00:00:00Z".into(),
        cwd: "/tmp/proj".into(),
        parent_session: None,
        extra: HashMap::new(),
    };
    let entries = vec![
        Entry::Session(header.clone()),
        Entry::Message {
            base: base("u1", None, "2026-04-16T00:00:01Z"),
            message: AgentMessage::User {
                content: MessageContent::Text("switch to opus please".into()),
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        },
        Entry::ModelChange {
            base: base("mc-1", Some("u1"), "2026-04-16T00:00:02Z"),
            provider: "anthropic".into(),
            model_id: "claude-opus-4-7".into(),
            extra: HashMap::new(),
        },
        Entry::Message {
            base: base("a1", Some("mc-1"), "2026-04-16T00:00:03Z"),
            message: AgentMessage::Assistant {
                content: vec![ContentBlock::Text {
                    text: "switched; carrying on".into(),
                    extra: HashMap::new(),
                }],
                api: "anthropic".into(),
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                usage: Usage {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 15,
                    cost: CostBreakdown::default(),
                },
                stop_reason: StopReason::Known(KnownStopReason::Stop),
                error_message: None,
                timestamp: 3,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        },
        Entry::Label {
            base: base("lbl-1", Some("a1"), "2026-04-16T00:00:04Z"),
            extra: HashMap::from([("label".to_string(), serde_json::json!("checkpoint"))]),
        },
    ];
    PiSession {
        header,
        entries,
        file_path: std::path::PathBuf::from("/tmp/fake.jsonl"),
        parent: None,
    }
}

/// Full chain with an on-disk-equivalent serialization in the middle.
fn full_roundtrip(source: &PiSession) -> PiSession {
    let view_forward: ConversationView = session_to_view(source);
    let path = derive_path(&view_forward, &DeriveConfig::default());
    let graph = Graph::from_path(path);
    let json = graph.to_json().expect("serialize Graph");
    let back = Graph::from_json(&json).expect("parse Graph");
    let reparsed = back.into_single_path().expect("single path");
    let view_back = extract_conversation(&reparsed);
    PiProjector::new()
        .with_cwd(source.header.cwd.clone())
        .project(&view_back)
        .expect("project")
}

#[test]
fn model_change_survives_full_roundtrip() {
    let source = source_session();
    let rebuilt = full_roundtrip(&source);
    let mc = rebuilt
        .entries
        .iter()
        .find_map(|e| match e {
            Entry::ModelChange {
                base,
                provider,
                model_id,
                ..
            } => Some((base.clone(), provider.clone(), model_id.clone())),
            _ => None,
        })
        .expect("ModelChange entry should survive the round-trip");
    assert_eq!(mc.0.id, "mc-1");
    assert_eq!(mc.0.parent_id.as_deref(), Some("u1"));
    assert_eq!(mc.1, "anthropic");
    assert_eq!(mc.2, "claude-opus-4-7");
}

#[test]
fn label_survives_full_roundtrip() {
    let source = source_session();
    let rebuilt = full_roundtrip(&source);
    let lbl = rebuilt
        .entries
        .iter()
        .find_map(|e| match e {
            Entry::Label { base, extra } => Some((base.clone(), extra.clone())),
            _ => None,
        })
        .expect("Label entry should survive the round-trip");
    assert_eq!(lbl.0.id, "lbl-1");
    assert_eq!(lbl.0.parent_id.as_deref(), Some("a1"));
    assert_eq!(lbl.1.get("label"), Some(&serde_json::json!("checkpoint")));
}

#[test]
fn metadata_entries_sit_after_their_parents_in_file_order() {
    let source = source_session();
    let rebuilt = full_roundtrip(&source);
    let ids: Vec<String> = rebuilt
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Session(_) => None,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. } => Some(base.id.clone()),
        })
        .collect();
    let pos = |id: &str| ids.iter().position(|i| i == id).unwrap();
    assert!(pos("u1") < pos("mc-1"), "order: {:?}", ids);
    assert!(pos("a1") < pos("lbl-1"), "order: {:?}", ids);
}

#[test]
fn rebuilt_jsonl_reparses_through_pi_reader() {
    let source = source_session();
    let rebuilt = full_roundtrip(&source);
    let lines: Vec<String> = rebuilt
        .entries
        .iter()
        .map(|e| serde_json::to_string(e).expect("serialize entry"))
        .collect();
    let tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .expect("tempfile");
    std::fs::write(tmp.path(), lines.join("\n")).expect("write");
    let reread =
        toolpath_pi::reader::read_session_from_file(tmp.path()).expect("re-read projected JSONL");
    assert!(
        reread
            .entries
            .iter()
            .any(|e| matches!(e, Entry::ModelChange { .. })),
        "re-read session should still contain the ModelChange entry"
    );
}
