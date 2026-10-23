//! The manifest goldens, half of the baseline check: `path d baseline check` runs
//! `cargo test -p toolpath-goldens` and then this test (`path_cli::goldens::CHECK_COMMANDS`). Every
//! set re-runs hermetically and must match its goldens byte-for-byte (or in the canonical form it
//! declares), every pin must match the manifest, and every known defect must still be present. This
//! test calls `Goldens::check`, the same code `check <set>` runs in-process, so they cannot drift.
//!
//! Regenerate on purpose with `path d baseline capture --all` (or `scripts/goldens.sh capture --all`).

use std::path::Path;

use path_cli::goldens::{Goldens, QualifierReview, qualifier_bin};

#[test]
fn goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut g = Goldens::new(root, Path::new(env!("CARGO_BIN_EXE_path")).to_path_buf());
    // `qualifier review` is part of the check (fail closed in `path d baseline check`). The dev shell
    // does not ship qualifier, so the test opts out explicitly when it cannot find the binary
    // (set TOOLPATH_GOLDENS_QUALIFIER to run it here too).
    if qualifier_bin().is_none() {
        // Under CI the workflow installs the pinned qualifier, so a missing binary there is a
        // broken setup, not a dev shell: fail rather than pass without the review.
        let ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false" && v != "0");
        assert!(
            !ci,
            "goldens test: no qualifier binary and CI is set; the review would be skipped silently"
        );
        eprintln!("goldens test: no qualifier binary; qualifier review skipped on purpose");
        g.qualifier = QualifierReview::Skip;
    }
    let report = g.check(None).unwrap();
    assert!(
        report.problems.is_empty(),
        "goldens check failed:\n{}\nIf the change is intended: path d baseline capture --all",
        report.render()
    );
    assert!(
        report.ok.len() >= 10,
        "suspiciously few goldens ran:\n{}",
        report.render()
    );
}
