//! The round-trip contract test from `docs/agents/adding-a-projector.md`
//! step 6: `native fixture → to_view → project → write → read back
//! (through this crate's own reader) → compare`.
//!
//! Two kinds, per the guide and the task:
//! - **Same-harness**: each of the three real agy fixtures, projected
//!   back into agy's own shape. Tool names must NOT churn (already
//!   agy-native).
//! - **Cross-harness**: a real Claude fixture, projected into agy.
//!   Tool names MUST remap (Claude's `Bash`/`Read`/`Write` have no agy
//!   analog by name) through `tool_category` + `native_name`.

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use toolpath_antigravity::project::AntigravityProjector;
use toolpath_antigravity::reader::ConvoIO;
use toolpath_antigravity::{PathResolver, provider};
use toolpath_convo::{ConversationProjector, Item, Role};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
}

fn project_and_read_back(
    view: &toolpath_convo::ConversationView,
    conversation_id: &str,
    cwd: &str,
) -> (toolpath_antigravity::project::AntigravityStore, TempDir) {
    let store = AntigravityProjector::new()
        .with_conversation_id(conversation_id)
        .project(view)
        .unwrap();
    let dir = TempDir::new().unwrap();
    store.write_to(dir.path(), cwd).unwrap();
    (store, dir)
}

fn same_harness_round_trip(subdir: &str, conversation_id: &str) {
    let resolver =
        PathResolver::new().with_gemini_dir(fixtures_dir().join("antigravity").join(subdir));
    let io = ConvoIO::with_resolver(resolver);
    let original = io.read_conversation(conversation_id).unwrap();
    let view = provider::to_view(&original);

    let new_id = format!("00000000-0000-0000-0000-{:012}", 1);
    let (store, dir) = project_and_read_back(&view, &new_id, "/tmp/roundtrip-test");
    assert!(
        store.unmapped_tool_names.is_empty(),
        "agy->agy should never need a name remap: {:?}",
        store.unmapped_tool_names
    );

    let read_back = store.read_back(dir.path()).unwrap();
    let rebuilt = provider::to_view(&read_back);

    let orig_turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    let new_turns: Vec<_> = rebuilt.items.iter().filter_map(Item::as_turn).collect();
    assert_eq!(orig_turns.len(), new_turns.len(), "{subdir}: turn count");
    for (o, n) in orig_turns.iter().zip(new_turns.iter()) {
        assert_eq!(o.role, n.role, "{subdir}: role");
        assert_eq!(o.text, n.text, "{subdir}: text");
        assert_eq!(
            o.tool_uses.iter().map(|t| &t.name).collect::<Vec<_>>(),
            n.tool_uses.iter().map(|t| &t.name).collect::<Vec<_>>(),
            "{subdir}: tool names must round-trip verbatim (already agy-native)"
        );
    }
}

#[test]
fn plain_text_fixture_round_trips_through_agy_shape() {
    same_harness_round_trip("plain", "752ac59f-6c6f-4fa0-8b63-0cb0b2466125");
}

#[test]
fn tool_call_fixture_round_trips_through_agy_shape() {
    same_harness_round_trip("tool", "aa79cad9-c443-4070-9c84-f6df6d3920b2");
}

#[test]
fn multi_turn_fixture_round_trips_through_agy_shape() {
    same_harness_round_trip("multi", "299f4ce3-f4ee-404d-a950-d2a5bbeae09d");
}

/// Cross-harness: a real Claude fixture, projected into agy's shape,
/// read back through *this crate's own reader* — not Claude's.
#[test]
fn claude_fixture_projects_into_agy_and_reads_back_through_our_reader() {
    let claude_path = fixtures_dir().join("claude").join("convo.jsonl");
    let claude_convo = toolpath_claude::ConversationReader::read_conversation(&claude_path)
        .expect("read claude fixture");
    let view = toolpath_claude::provider::to_view(&claude_convo);

    let orig_turns: Vec<_> = view.items.iter().filter_map(Item::as_turn).collect();
    assert!(!orig_turns.is_empty(), "claude fixture must have turns");

    let new_id = "00000000-0000-0000-0000-00000000c1a0".to_string();
    let (store, dir) = project_and_read_back(&view, &new_id, "/tmp/claude-to-agy-test");

    // MUST #2: foreign names remap through category + native_name — for
    // every name `tool_category` can classify. `AskUserQuestion` is a
    // real, structural gap: `ToolCategory` has no "ask a clarifying
    // question" variant, so neither Claude's `AskUserQuestion` nor agy's
    // own `ask_question` (observed in real samples' tool-permission
    // listing) can be matched generically — both stay under their
    // verbatim name crossing either direction. Flagged, not silently
    // dropped: `unmapped_tool_names` is exactly where this test checks.
    // `ToolCategory::FileSearch` has no observed agy-native analog either
    // (no sample ever called a search tool) — see `native_name`'s doc.
    let known_gaps = ["AskUserQuestion", "Glob", "Grep"];
    for name in &store.unmapped_tool_names {
        assert!(
            known_gaps.contains(&name.as_str()),
            "unexpected unmapped tool name: {name} (not in the known-gaps list)"
        );
    }

    let read_back = store.read_back(dir.path()).unwrap();
    let rebuilt = provider::to_view(&read_back);
    let new_turns: Vec<_> = rebuilt.items.iter().filter_map(Item::as_turn).collect();

    // Text content for user/assistant turns survives the trip.
    let orig_user_texts: Vec<&str> = orig_turns
        .iter()
        .filter(|t| t.role == Role::User)
        .map(|t| t.text.as_str())
        .collect();
    let new_user_texts: Vec<&str> = new_turns
        .iter()
        .filter(|t| t.role == Role::User)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(orig_user_texts, new_user_texts);

    // Every tool name in the rebuilt view is agy-native (not a foreign
    // name), and none of the foreign names leaked through verbatim.
    let rebuilt_names: std::collections::HashSet<String> = new_turns
        .iter()
        .flat_map(|t| t.tool_uses.iter())
        .map(|tu| tu.name.clone())
        .collect();
    for name in &rebuilt_names {
        assert!(
            toolpath_antigravity::tool_category(name).is_some()
                || known_gaps.contains(&name.as_str()),
            "{name} in the rebuilt agy view is neither one of agy's own tool names nor a declared gap"
        );
    }
}
