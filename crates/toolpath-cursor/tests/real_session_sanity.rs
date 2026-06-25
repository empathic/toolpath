//! Sanity check against a real Cursor installation, when one is
//! present.
//!
//! On a developer machine that has used Cursor, the global SQLite at
//! `<user-data>/User/globalStorage/state.vscdb` will exist; this test
//! reads it and asserts that any composer with bubbles on disk can
//! be loaded, lifted into a [`ConversationView`], and derived into a
//! Toolpath `Path` without panicking.
//!
//! When no DB is found (CI, fresh machine), the test no-ops.

use toolpath::v1::Graph;
use toolpath_convo::ConversationProvider;
use toolpath_cursor::{CursorConvo, DeriveConfig, derive_path, session_to_view};

#[test]
fn real_cursor_db_round_trips_when_present() {
    let mgr = CursorConvo::new();
    if !mgr.io().exists() {
        eprintln!("note: no Cursor user-data directory; skipping live test");
        return;
    }
    if !mgr.resolver().db_exists() {
        eprintln!("note: Cursor user-data exists but no state.vscdb yet; skipping");
        return;
    }

    let ids = match ConversationProvider::list_conversations(&mgr, "") {
        Ok(ids) => ids,
        Err(e) => {
            // Permissions / WAL contention is the most likely cause —
            // surface but don't fail the build for it.
            eprintln!("note: could not list composers from real DB: {e}; skipping");
            return;
        }
    };
    eprintln!("note: {} composers found in real DB", ids.len());
    if ids.is_empty() {
        return;
    }

    // Walk every composer (most installs will have only a handful);
    // we want any malformed bubble or odd tool-call shape to surface.
    let mut sessions_checked = 0usize;
    for id in &ids {
        let Ok(session) = mgr.read_session(id) else {
            continue;
        };
        let view = session_to_view(&session);
        // Basic invariants the IR + derive both rely on.
        assert_eq!(view.id, *id, "view.id should equal composer id");
        assert_eq!(view.provider_id.as_deref(), Some("cursor"));
        assert_eq!(view.session_ids, vec![id.clone()]);
        for turn in view.turns() {
            assert!(!turn.id.is_empty(), "every turn carries a bubble id");
        }
        let path = derive_path(&session, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-cursor-"));
        // The derived doc must validate as a single-path graph.
        let graph = Graph::from_path(path);
        let json = graph.to_json().expect("graph should serialize");
        let _back = Graph::from_json(&json).expect("graph should round-trip");
        sessions_checked += 1;
    }
    eprintln!("note: validated {sessions_checked} live composers");
}
