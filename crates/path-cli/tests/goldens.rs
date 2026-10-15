//! The goldens test is `path goldens check`, called as a library: every set re-runs hermetically
//! and must match its goldens byte-for-byte (or in the canonical form it declares), every pin must
//! match the manifest, and every known defect must still be present. The tool and the test share
//! one implementation (`path_cli::goldens`), so they cannot drift.
//!
//! Regenerate on purpose with `path goldens capture --all` (or `scripts/goldens.sh capture --all`).

use std::path::Path;

use path_cli::goldens::{Goldens, QualifierReview, qualifier_bin};

#[test]
fn goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut g = Goldens::new(root, Path::new(env!("CARGO_BIN_EXE_path")).to_path_buf());
    // `qualifier review` is part of the check (fail closed in `path goldens check`). The dev shell
    // does not ship qualifier, so the test opts out explicitly when it cannot find the binary
    // (set TOOLPATH_GOLDENS_QUALIFIER to run it here too).
    if qualifier_bin().is_none() {
        eprintln!("goldens test: no qualifier binary; qualifier review skipped on purpose");
        g.qualifier = QualifierReview::Skip;
    }
    let report = g.check(None).unwrap();
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
