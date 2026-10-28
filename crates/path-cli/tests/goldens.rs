//! The manifest goldens, half of the baseline check: `path d baseline check` runs
//! `cargo test -p toolpath-goldens` and then this test (`path_cli::goldens::CHECK_COMMANDS`). Every
//! set re-runs hermetically and must match its goldens byte-for-byte (or in the canonical form it
//! declares), every pin must match the manifest, and every known defect must still be present. This
//! test calls `Goldens::check`, the same code `check <set>` runs in-process, so they cannot drift.
//!
//! ONE TEST PER SET (`goldens::claude`, `goldens::codex`, …), so CI names the harness that moved instead of
//! one `goldens` line for all of them. `every_manifest_set_has_a_test` fails when goldens/manifest.json
//! gains or loses a set the list below does not name, so a new set can never run untested.
//!
//! Regenerate on purpose with `path d baseline capture --all` (or `scripts/goldens.sh capture --all`).

use std::path::Path;

use path_cli::goldens::{Goldens, QualifierReview, qualifier_bin};

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// `Goldens::check` for one set (`only`), with the qualifier rule the whole-manifest run had.
fn check_set(set: &str) {
    let mut g = Goldens::new(root(), Path::new(env!("CARGO_BIN_EXE_path")).to_path_buf());
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
    let report = g.check(Some(set)).unwrap();
    assert!(
        report.problems.is_empty(),
        "goldens check failed for set {set}:\n{}\nIf the change is intended: path d baseline capture --all",
        report.render()
    );
    assert!(
        !report.ok.is_empty(),
        "set {set}: nothing ran:\n{}",
        report.render()
    );
}

macro_rules! golden_sets {
    ($($test:ident => $set:literal),* $(,)?) => {
        const SETS: &[&str] = &[$($set),*];
        mod goldens {
            $(#[test]
            fn $test() {
                super::check_set($set);
            })*
        }
    };
}

// One line per set in goldens/manifest.json, in manifest order.
golden_sets! {
    claude => "claude",
    claude_compacted => "claude-compacted",
    claude_session => "claude-session",
    codex => "codex",
    copilot => "copilot",
    pi => "pi",
}

#[test]
fn every_manifest_set_has_a_test() {
    let m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root().join("goldens/manifest.json")).unwrap())
            .unwrap();
    let in_manifest: Vec<&str> = m["goldens"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(
        in_manifest, SETS,
        "goldens/manifest.json and the golden_sets! list in tests/goldens.rs differ: add or remove the test line"
    );
}
