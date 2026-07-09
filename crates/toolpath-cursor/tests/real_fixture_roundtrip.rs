//! Real-fixture invariants for #124 (relativized file-change keys).
//!
//! Loads the shared real-world fixture at `test-fixtures/cursor/convo.json`
//! (refreshed via `scripts/capture-elicit-fixtures.sh`) and derives it
//! through the full `CursorSession` -> `ConversationView` -> `Path`
//! pipeline. Complements `tests/projection_roundtrip.rs`'s synthetic
//! minimum-shape coverage by running on production-shape input, and
//! `crates/path-cli/tests/cross_harness_matrix.rs::CursorHarness`'s
//! generic cross-provider projection roundtrip with a key-relativization
//! invariant specific to this fixture.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toolpath_convo::{ConversationView, DeriveConfig, derive_path, extract_conversation};
use toolpath_cursor::{CursorSession, session_to_view};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-fixtures")
        .join("cursor")
        .join("convo.json")
}

fn load_fixture_view() -> ConversationView {
    let json = std::fs::read_to_string(fixture_path()).expect("read cursor fixture");
    let session: CursorSession = serde_json::from_str(&json).expect("parse cursor fixture");
    session_to_view(&session)
}

#[test]
fn fixture_loads() {
    let view = load_fixture_view();
    assert!(
        !view.turns.is_empty(),
        "cursor fixture should produce a non-empty view"
    );
}

/// Ground-truth invariant for #124 (relativized file-change keys) run
/// against a real recorded session: derive `view`, then for every
/// pre-derive `FileMutation::path` assert (a) it produced a relativized
/// key iff it actually sat under `path.base` on a path-component boundary
/// -- no absolute-under-base leak, and no wrongly-relativized outside-base
/// key -- and (b) extracting and re-deriving reproduces the identical
/// `file.write` key set (idempotency).
///
/// "Under base" is independently recomputed here via `std::path::Path`
/// component stripping rather than by calling `toolpath_convo`'s own
/// (private) `relativize_key`, so this exercises its output rather than
/// re-asserting its internals.
#[test]
fn file_write_keys_relativized_with_no_leak_and_stable_on_re_derive() {
    let view = load_fixture_view();
    let path = derive_path(&view, &DeriveConfig::default());
    let base_root: Option<String> = path
        .path
        .base
        .as_ref()
        .and_then(|b| b.uri.strip_prefix("file://"))
        .map(|s| s.trim_end_matches('/').to_string());

    let ground_truth: Vec<&str> = view
        .turns
        .iter()
        .flat_map(|t| t.file_mutations.iter().map(|fm| fm.path.as_str()))
        .collect();
    assert!(
        !ground_truth.is_empty(),
        "fixture must exercise at least one file mutation for this test to be meaningful"
    );

    let file_write_keys = |p: &toolpath::v1::Path| -> BTreeSet<String> {
        p.steps
            .iter()
            .flat_map(|s| s.change.iter())
            .filter(|(_, ch)| {
                ch.structural
                    .as_ref()
                    .is_some_and(|s| s.change_type == "file.write")
            })
            .map(|(k, _)| k.clone())
            .collect()
    };
    let derived_keys = file_write_keys(&path);

    for gt in &ground_truth {
        let under_base = base_root.as_deref().is_some_and(|root| {
            std::path::Path::new(gt)
                .strip_prefix(root)
                .is_ok_and(|rest| rest != std::path::Path::new(""))
        });
        if under_base {
            let root = base_root.as_deref().unwrap();
            let expected_relative = gt.strip_prefix(root).unwrap().trim_start_matches('/');
            assert!(
                derived_keys.contains(expected_relative),
                "expected relativized key {expected_relative:?} for {gt:?} under base {root:?}, got {derived_keys:?}"
            );
            assert!(
                !derived_keys.contains(*gt),
                "absolute-under-base leak: {gt:?} should have been relativized but the absolute form is still a key"
            );
        } else {
            assert!(
                derived_keys.contains(*gt),
                "expected {gt:?} to remain an absolute (or opaque) key outside the base, got {derived_keys:?}"
            );
        }
    }

    let view2 = extract_conversation(&path);
    let path2 = derive_path(&view2, &DeriveConfig::default());
    assert_eq!(
        derived_keys,
        file_write_keys(&path2),
        "re-derive must reproduce the identical file.write key set"
    );
}
