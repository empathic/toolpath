//! `qualifier review` over this crate: the findings behind the goldens are
//! qualifier records anchored on the code (`src/.qual`, `tests/.qual`), and
//! an anchor that drifted or went missing means the code moved under a
//! finding nobody re-read. The binary is `$TOOLPATH_GOLDENS_QUALIFIER`, else
//! `qualifier` on PATH; with neither the test prints why it skipped and
//! passes, so a dev shell without qualifier still runs the goldens.
//!
//! `qualifier review <subject>` matches one subject exactly (a file, not a
//! directory), so the test reviews the whole repo as JSON and keeps the
//! records whose subject lies under this crate.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// `review` resolves `.qual` files from the repo root.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const CRATE: &str = "crates/toolpath-goldens/";

#[test]
fn qualifier_anchors_on_this_crate_are_fresh() {
    let explicit = std::env::var_os("TOOLPATH_GOLDENS_QUALIFIER").filter(|v| !v.is_empty());
    let Some(bin) = explicit.map(PathBuf::from).or_else(|| on_path("qualifier")) else {
        eprintln!(
            "qualifier review skipped: no `qualifier` on PATH and TOOLPATH_GOLDENS_QUALIFIER unset"
        );
        return;
    };
    let out = Command::new(&bin)
        .args(["review", "--format", "json"])
        .current_dir(ROOT)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", bin.display()));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "qualifier review failed:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reviewed: Vec<Value> = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("qualifier review --format json is not a JSON array: {e}\n{text}")
    });
    let ours: Vec<&Value> = reviewed
        .iter()
        .filter(|r| r["subject"].as_str().is_some_and(|s| s.starts_with(CRATE)))
        .collect();
    let stale: Vec<String> = ours
        .iter()
        .filter(|r| r["status"] != "fresh")
        .map(|r| {
            format!(
                "{} {} {}: {}",
                r["status"].as_str().unwrap_or("?"),
                r["location"].as_str().unwrap_or("?"),
                r["kind"].as_str().unwrap_or("?"),
                r["summary"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert!(
        stale.is_empty(),
        "qualifier anchors moved under {CRATE}; re-record (--supersedes) or resolve them:\n{}",
        stale.join("\n")
    );
    assert!(
        !ours.is_empty(),
        "qualifier review found no span-anchored record under {CRATE}; the crate's .qual files should anchor here"
    );
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}
