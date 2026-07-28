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
        Some("Amp session: T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0000")
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

/// A claude-shaped foreign session (the piece-05 cc-to-amp direction)
/// projected into an amp export and read back through the forward path:
/// conversation beats survive, every tool lands on an amp-native name
/// whose args carry the key amp's UI reads, and the projection is a
/// fixed point.
#[test]
fn foreign_claude_session_round_trips_through_amp() {
    use serde_json::{Value, json};
    use toolpath_amp::AmpProjector;
    use toolpath_convo::{
        ConversationProjector, ConversationView, ToolInvocation, ToolResult, Turn,
    };

    let tool = |id: &str, name: &str, input: Value, cat, content: &str, is_error| ToolInvocation {
        id: id.to_string(),
        name: name.to_string(),
        input,
        result: Some(ToolResult {
            content: content.to_string(),
            is_error,
        }),
        category: Some(cat),
    };
    let turn = |id: &str, role, text: &str, tools: Vec<ToolInvocation>| Turn {
        id: id.to_string(),
        parent_id: None,
        group_id: None,
        role,
        timestamp: "2026-07-28T10:00:00.000Z".to_string(),
        text: text.to_string(),
        thinking: None,
        tool_uses: tools,
        model: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    };

    let view = ConversationView {
        id: "T-cc".into(),
        turns: vec![
            turn("u1", Role::User, "set up the fixture files", Vec::new()),
            turn(
                "a1",
                Role::Assistant,
                "Listing, then writing.",
                vec![
                    tool(
                        "c1",
                        "Bash",
                        json!({"command": "ls -la"}),
                        ToolCategory::Shell,
                        "total 0",
                        false,
                    ),
                    tool(
                        "c2",
                        "Write",
                        json!({"file_path": "/w/notes.md", "content": "scratch\nsecond"}),
                        ToolCategory::FileWrite,
                        "wrote /w/notes.md",
                        false,
                    ),
                ],
            ),
            turn(
                "a2",
                Role::Assistant,
                "Editing and searching.",
                vec![
                    tool(
                        "c3",
                        "Edit",
                        json!({"file_path": "/w/notes.md", "old_string": "scratch",
                               "new_string": "fixture"}),
                        ToolCategory::FileWrite,
                        "edited",
                        false,
                    ),
                    tool(
                        "c4",
                        "Grep",
                        json!({"pattern": "fixture", "path": "/w"}),
                        ToolCategory::FileSearch,
                        "notes.md:1:fixture",
                        false,
                    ),
                    tool(
                        "c5",
                        "Read",
                        json!({"file_path": "/w/does-not-exist.txt"}),
                        ToolCategory::FileRead,
                        "No such file",
                        true,
                    ),
                    tool(
                        "c6",
                        "WebFetch",
                        json!({"url": "https://e.x/doc", "prompt": "summarize"}),
                        ToolCategory::Network,
                        "the doc says hi",
                        false,
                    ),
                ],
            ),
            turn("a3", Role::Assistant, "All done.", Vec::new()),
        ],
        ..Default::default()
    };

    let projector = AmpProjector::new();
    let export = projector.project(&view).expect("project");
    let back = to_view(&Session::from_export(export.clone()));

    // Beats survive.
    assert_eq!(back.turns.len(), view.turns.len());
    for (a, b) in view.turns.iter().zip(&back.turns) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.role, b.role);
        assert_eq!(a.text, b.text);
    }

    // Every foreign tool landed on an amp-native name with the arg key
    // amp's UI reads, keeping its id, result, and error flag — and the
    // native name re-classifies to the category the remap was chosen for.
    let back_tools: Vec<&ToolInvocation> = back.turns.iter().flat_map(|t| &t.tool_uses).collect();
    let expect: [(&str, &str, &str, &str, bool); 6] = [
        ("c1", "shell_command", "command", "total 0", false),
        ("c2", "apply_patch", "patchText", "wrote /w/notes.md", false),
        ("c3", "apply_patch", "patchText", "edited", false),
        ("c4", "finder", "query", "notes.md:1:fixture", false),
        ("c5", "shell_command", "command", "No such file", true),
        ("c6", "read_web_page", "url", "the doc says hi", false),
    ];
    assert_eq!(back_tools.len(), expect.len());
    for (id, name, arg_key, content, is_error) in expect {
        let t = back_tools.iter().find(|t| t.id == id).unwrap();
        assert_eq!(t.name, name, "{id}");
        assert!(t.input.get(arg_key).is_some(), "{id}: missing {arg_key}");
        let r = t.result.as_ref().unwrap();
        assert_eq!(r.content, content, "{id}");
        assert_eq!(r.is_error, is_error, "{id}");
        assert_eq!(
            t.category,
            toolpath_amp::tool_category(name),
            "{id}: category consistent with the projected name"
        );
    }

    // The synthesized patches carry the real content.
    let patch = back_tools.iter().find(|t| t.id == "c2").unwrap();
    let pt = patch.input["patchText"].as_str().unwrap();
    assert!(pt.contains("*** Add File: /w/notes.md"));
    assert!(pt.contains("+scratch\n+second"));
    let edit = back_tools.iter().find(|t| t.id == "c3").unwrap();
    let pt = edit.input["patchText"].as_str().unwrap();
    assert!(pt.contains("*** Update File: /w/notes.md"));
    assert!(pt.contains("-scratch\n+fixture"));

    // Fixed point at the wire level: projecting the read-back view again
    // yields a byte-identical export.
    let export2 = projector.project(&back).expect("re-project");
    assert_eq!(
        serde_json::to_value(&export).unwrap(),
        serde_json::to_value(&export2).unwrap()
    );

    // And the projected export re-parses strictly (no Unknown blocks).
    let json = serde_json::to_string(&export).unwrap();
    let strict = ExportReader::parse_export_with(&json, true).expect("strict re-parse");
    for m in &strict.messages {
        for b in &m.content {
            assert!(
                matches!(b, toolpath_amp::Block::Known(_)),
                "unknown block in projection"
            );
        }
    }
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
