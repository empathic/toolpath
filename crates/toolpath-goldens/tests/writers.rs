//! Each writer on its own. The goldens measure losses end to end on real
//! sessions, where a loss could come from the source agent's reader or from
//! the target agent's writer. Here every writer gets the same generated
//! document, built from `fixtures/full-view.json`, which fills every field
//! the conversation view carries (thinking, tool results including an error,
//! token usage with cache and breakdowns, file mutations, a delegation,
//! environment, base and producer). One snapshot per writer,
//! `writer@<agent>`: what that writer, read back by its own adapter, drops,
//! renames or invents from an input that is identical for all eight. One
//! test per writer (`writer_claude`, … `writer_cursor_agent`), so a broken
//! writer is named on its own and the others stay green.

use std::sync::OnceLock;

use toolpath::v1::Graph;
use toolpath_convo::{ConversationView, DeriveConfig, derive_path};
use toolpath_goldens::model::Agent;
use toolpath_goldens::pipeline::writer;

const VIEW: &str = include_str!("../fixtures/full-view.json");

/// The generated document, built once and shared by every writer's test.
fn document_json() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let view: ConversationView =
            serde_json::from_str(VIEW).expect("fixtures/full-view.json is a ConversationView");
        let cfg = DeriveConfig {
            include_thinking: true,
            include_tool_uses: true,
            ..DeriveConfig::default()
        };
        Graph::from_path(derive_path(&view, &cfg))
            .to_json()
            .expect("document serializes")
    })
}

fn check_writer(agent: Agent) {
    let home = tempfile::tempdir().expect("temp HOME");
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.set_sort_maps(true);
    settings.bind(|| {
        let trip = writer(document_json(), agent, home.path())
            .unwrap_or_else(|e| panic!("writer {}: {e}", agent.name()));
        insta::assert_json_snapshot!(format!("writer@{}", agent.name()), trip);
    });
}

/// One test per writer; a match on [`Agent`] so a new agent without a test
/// here does not compile.
macro_rules! writers {
    ($($test:ident => $agent:ident),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                check_writer(Agent::$agent);
            }
        )*

        #[allow(dead_code)]
        fn every_agent_has_a_writer_test(agent: Agent) {
            match agent {
                $(Agent::$agent => {})*
            }
        }
    };
}

writers! {
    writer_claude => Claude,
    writer_codex => Codex,
    writer_pi => Pi,
    writer_copilot => Copilot,
    writer_gemini => Gemini,
    writer_opencode => Opencode,
    writer_cursor => Cursor,
    writer_cursor_agent => CursorAgent,
}
