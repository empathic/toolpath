//! The goldens test is `path goldens check`, called as a library: every set re-runs hermetically
//! and must match its goldens byte-for-byte (or in the canonical form it declares), every pin must
//! match the manifest, and every known defect must still be present. The tool and the test share
//! one implementation (`path_cli::goldens`), so they cannot drift.
//!
//! Regenerate on purpose with `path goldens capture --all` (or `scripts/goldens.sh capture --all`).

use std::path::Path;

#[test]
fn goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let report = path_cli::goldens::check(&root, Path::new(env!("CARGO_BIN_EXE_path"))).unwrap();
    assert!(
        report.problems.is_empty(),
        "goldens check failed:\n{}\nIf the change is intended: path goldens capture --all",
        report.render()
    );
    assert!(
        report.ok.len() >= 10,
        "suspiciously few goldens ran:\n{}",
        report.render()
    );
}
