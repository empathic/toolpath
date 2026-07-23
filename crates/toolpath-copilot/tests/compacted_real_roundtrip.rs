//! Round-trip tests against a REAL captured Copilot CLI session containing a
//! context compaction.
//!
//! The fixture (`test-fixtures/copilot/compacted-real.jsonl`) was captured
//! live at `copilotVersion` 1.0.68 (2026-07-21) by driving the copilot TUI
//! interactively: a file-creation exchange, a `/compact`, and a
//! post-compaction question. It is the first observed instance of the
//! `session.compaction_start` / `session.compaction_complete` encoding, and
//! also carries `session.model_change`, `permission.requested`/`completed`,
//! and `system.message` events.

use std::path::{Path, PathBuf};

use toolpath_convo::testing::{assert_fixpoint, check_view_invariants};
use toolpath_convo::{
    ConversationProjector, ConversationView, DeriveConfig, derive_path, extract_conversation,
};
use toolpath_copilot::{CopilotProjector, EventReader, Session, to_view};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("copilot")
        .join("compacted-real.jsonl")
}

fn load_view() -> ConversationView {
    let path = fixture_path();
    let lines = EventReader::read_lines(&path).expect("parse compacted fixture");
    let session = Session {
        id: "52caac9f-compacted-real".to_string(),
        dir_path: path,
        lines,
        workspace: None,
    };
    to_view(&session)
}

fn reread(v: &ConversationView) -> ConversationView {
    let s = CopilotProjector::new().project(v).expect("project view");
    to_view(&s)
}

#[test]
fn compaction_is_read_as_a_typed_boundary() {
    let view = load_view();
    assert_eq!(view.compactions().count(), 1);
    let c = view.compactions().next().unwrap();
    assert_eq!(c.pre_tokens, Some(427));
    assert!(
        c.summary.as_deref().is_some_and(|s| s.contains("<overview>")),
        "summaryContent must land on Compaction.summary"
    );
    assert_eq!(c.kept_from, None, "copilot reports counts, not kept ids");

    // The boundary sits on the head ancestry: the post-compaction user turn
    // parents on it.
    let post_user = view
        .turns()
        .find(|t| t.text.contains("what is in notes.md"))
        .expect("post-compaction user turn");
    assert_eq!(post_user.parent_id.as_deref(), Some(c.id.as_str()));
}

#[test]
fn oracle_invariants_stability_fixpoint() {
    let view = load_view();
    let problems = check_view_invariants(&view);
    assert!(problems.is_empty(), "source invariants: {problems:?}");

    // Looped: step classification once depended on HashMap order, so a
    // single pass can pass by luck.
    for _ in 0..24 {
        let gen1 = derive_path(&view, &DeriveConfig::default());
        let gen2 = derive_path(&extract_conversation(&gen1), &DeriveConfig::default());
        assert_eq!(
            serde_json::to_value(&gen1).unwrap(),
            serde_json::to_value(&gen2).unwrap(),
            "derive → extract → derive changed the document"
        );
    }

    let once = reread(&view);
    let twice = reread(&once);
    assert_fixpoint(&view, &once, &twice);
}

#[test]
fn projection_emits_the_observed_compaction_pair() {
    let view = load_view();
    let session = CopilotProjector::new().project(&view).expect("project");
    let kinds: Vec<&str> = session.lines.iter().map(|l| l.kind.as_str()).collect();
    let start = kinds
        .iter()
        .position(|k| *k == "session.compaction_start")
        .expect("start marker emitted");
    let complete = kinds
        .iter()
        .position(|k| *k == "session.compaction_complete")
        .expect("complete emitted");
    assert_eq!(complete, start + 1, "pair is adjacent, start first");

    let line = &session.lines[complete];
    let data = line.data.as_ref().expect("complete carries data");
    assert_eq!(data.get("success"), Some(&serde_json::json!(true)));
    assert_eq!(
        data.get("preCompactionTokens"),
        Some(&serde_json::json!(427))
    );
    assert!(
        data.get("summaryContent")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("<overview>")),
        "summary re-emitted as summaryContent"
    );

    // Round-trip: the projected wire re-reads to the same boundary.
    let view2 = to_view(&session);
    assert_eq!(view2.compactions().count(), 1);
    let c2 = view2.compactions().next().unwrap();
    assert_eq!(c2.pre_tokens, Some(427));
}
