//! Round-trip tests on the synthetic fixture
//! (`tests/fixtures/sample-session.json`): wire serde value-identity,
//! source → view → path derivation, and document-level JSON round-trip.

use toolpath::v1::{Graph, PATH_KIND_AGENT_CODING_SESSION, query};
use toolpath_amp::derive::{DeriveConfig, derive_path};
use toolpath_amp::{ExportReader, Session, to_view};
use toolpath_convo::{Role, ToolCategory};

const SAMPLE: &str = include_str!("fixtures/sample-session.json");

fn sample_session() -> Session {
    Session::from_export(ExportReader::parse_export_with(SAMPLE, true).expect("parse sample"))
}

#[test]
fn wire_serde_value_identity() {
    let orig: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
    let session = sample_session();
    let back = serde_json::to_value(&session.export).unwrap();
    assert_eq!(orig, back, "sample export not value-identical after serde");
}

#[test]
fn view_shape_matches_source() {
    let view = to_view(&sample_session());
    // 6 messages = 1 user + 3 assistant + 2 tool-result-only.
    assert_eq!(view.turns.len(), 4);
    assert_eq!(view.turns[0].role, Role::User);
    assert_eq!(view.turns[0].text, "add a main and run it");

    let tools: Vec<_> = view.turns.iter().flat_map(|t| &t.tool_uses).collect();
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|t| t.result.is_some()));

    // The failing cargo run is the errored one (exitCode 101).
    let shell = tools.iter().find(|t| t.name == "shell_command").unwrap();
    assert_eq!(shell.category, Some(ToolCategory::Shell));
    assert!(shell.result.as_ref().unwrap().is_error);
    let patch = tools.iter().find(|t| t.name == "apply_patch").unwrap();
    assert!(!patch.result.as_ref().unwrap().is_error);

    // Mutation from the apply_patch result, relativized.
    assert_eq!(view.files_changed, vec!["a.rs"]);

    // Per-message usage; empty thinking dropped.
    let total = view.total_usage.as_ref().unwrap();
    assert_eq!(total.output_tokens, Some(42 + 21 + 7));
    assert_eq!(total.cache_read_tokens, Some(60 + 100 + 140));
    assert_eq!(
        view.turns[3].thinking, None,
        "empty summary is not Some(\"\")"
    );
}

#[test]
fn derived_path_round_trips_as_document() {
    let path = derive_path(&sample_session(), &DeriveConfig::default());
    assert_eq!(
        path.meta.as_ref().unwrap().kind.as_deref(),
        Some(PATH_KIND_AGENT_CODING_SESSION)
    );
    assert_eq!(
        path.meta.as_ref().unwrap().title.as_deref(),
        Some("Amp session: T-0199aa")
    );

    // Serialize → reparse → same shape, single path, connected ancestry.
    let doc = Graph::from_path(path);
    let json = doc.to_json().unwrap();
    let parsed = Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    let anc = query::ancestors(&p.steps, &p.path.head);
    assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");

    // The file artifact carries the real diff from the wire.
    let file_step = p
        .steps
        .iter()
        .find(|s| s.change.contains_key("a.rs"))
        .expect("file artifact step");
    assert!(
        file_step.change["a.rs"]
            .raw
            .as_ref()
            .unwrap()
            .contains("+fn main() {}")
    );
}

#[test]
fn view_survives_its_own_serde() {
    let view = to_view(&sample_session());
    let json = serde_json::to_string(&view).unwrap();
    let back: toolpath_convo::ConversationView = serde_json::from_str(&json).unwrap();
    assert_eq!(back.turns.len(), view.turns.len());
    assert_eq!(back.total_usage, view.total_usage);
    assert_eq!(back.files_changed, view.files_changed);
}
