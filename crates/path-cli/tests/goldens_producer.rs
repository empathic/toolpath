//! Producer pins: a capture records the `path` binary (by sha256) and the repo rev that produced
//! it, and `check` fails when the running binary or the repo code differs. Every case starts from a
//! real capture of the committed `claude` fixture into a throwaway git repo; nothing is invented
//! beyond the edit each case makes to that capture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use path_cli::goldens::{Goldens, Harness, Report};
use serde_json::Value;
use sha2::{Digest, Sha256};

const EXE: &str = env!("CARGO_BIN_EXE_path");
const FIXTURE: &str = "test-fixtures/claude/convo.jsonl";
/// What `pins()` reads from the root, plus the fixture.
const COPIED: &[&str] = &[
    FIXTURE,
    "Cargo.lock",
    "flake.lock",
    "rust-toolchain.toml",
    "crates/path-cli/src/goldens.rs",
    "scripts/goldens.sh",
    "docs/GOLDENS.md",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git repo holding the files a capture reads, committed once, with the claude set captured.
fn captured() -> (tempfile::TempDir, Goldens) {
    capture_with(false)
}

/// Like `captured`, but the producing binary is a copy of `path` under a directory the check
/// treats as the nix store, so its sha is compared strictly.
fn captured_in_store() -> (tempfile::TempDir, Goldens) {
    capture_with(true)
}

fn capture_with(store: bool) -> (tempfile::TempDir, Goldens) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for rel in COPIED {
        let to = root.join(rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(repo().join(rel), to).unwrap();
    }
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let bin = if store {
        let bin = root.join("store/path");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::copy(EXE, &bin).unwrap();
        bin
    } else {
        PathBuf::from(EXE)
    };
    let mut g = Goldens::new(root.clone(), bin);
    if store {
        g.store_root = root.join("store");
    }
    g.capture(Harness::Claude, &root.join(FIXTURE), "claude", None, None)
        .unwrap();
    (tmp, g)
}

fn manifest(g: &Goldens) -> Value {
    serde_json::from_slice(&fs::read(g.root.join("goldens/manifest.json")).unwrap()).unwrap()
}

fn write_manifest(g: &Goldens, m: &Value) {
    fs::write(
        g.root.join("goldens/manifest.json"),
        serde_json::to_string_pretty(m).unwrap(),
    )
    .unwrap();
}

fn mentions(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|r| r.contains(needle))
}

fn shown(r: &Report) -> String {
    r.render()
}

#[test]
fn capture_pins_the_running_binary_and_rev() {
    let (_tmp, g) = captured();
    let m = manifest(&g);
    let p = &m["goldens"][0]["pins"]["producer"];
    let exe = fs::read(EXE).unwrap();
    assert_eq!(p["binary_sha256"], hex::encode(Sha256::digest(&exe)));
    assert_eq!(p["repo_rev"].as_str().unwrap().len(), 40);
    // target/debug/path is not a nix store binary: recorded as a dev build.
    assert_eq!(p["dirty"], true);
    assert!(p["binary_path"].as_str().unwrap().ends_with("/path"));
    assert!(
        m["roundtrip"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["producer"] == *p),
        "round trip rows carry the same producer"
    );
    let r = g.check(None).unwrap();
    assert!(r.problems.is_empty(), "{}", shown(&r));
}

#[test]
fn a_moved_dev_build_is_information_never_a_failure() {
    let (_tmp, g) = captured();
    let mut m = manifest(&g);
    m["goldens"][0]["pins"]["producer"]["binary_sha256"] = "0".repeat(64).into();
    write_manifest(&g, &m);
    let r = g.check(None).unwrap();
    assert!(r.problems.is_empty(), "{}", shown(&r));
    assert!(
        mentions(&r.info, "producer: unpinned dev build"),
        "{}",
        shown(&r)
    );
}

#[test]
fn moved_store_binary_is_a_problem_unless_allowed() {
    let (_tmp, mut g) = captured_in_store();
    let mut m = manifest(&g);
    assert_eq!(m["goldens"][0]["pins"]["producer"]["dirty"], false);
    m["goldens"][0]["pins"]["producer"]["binary_sha256"] = "0".repeat(64).into();
    write_manifest(&g, &m);

    let r = g.check(None).unwrap();
    assert!(
        mentions(&r.problems, "PRODUCER MOVED binary_sha256"),
        "{}",
        shown(&r)
    );

    g.allow_producer_drift = true;
    let r = g.check(None).unwrap();
    assert!(r.problems.is_empty(), "{}", shown(&r));
    assert!(
        mentions(&r.warnings, "PRODUCER MOVED binary_sha256"),
        "{}",
        shown(&r)
    );
    assert!(r.render().contains("warn  claude: PRODUCER MOVED"));
}

#[test]
fn a_golden_without_a_producer_is_information_not_failure() {
    let (_tmp, g) = captured();
    let mut m = manifest(&g);
    m["goldens"][0]["pins"]
        .as_object_mut()
        .unwrap()
        .remove("producer");
    for d in m["roundtrip"].as_array_mut().unwrap() {
        d.as_object_mut().unwrap().remove("producer");
    }
    write_manifest(&g, &m);

    let r = g.check(None).unwrap();
    assert!(r.problems.is_empty(), "{}", shown(&r));
    assert!(
        mentions(&r.info, "claude: unpinned producer"),
        "{}",
        shown(&r)
    );
}

#[test]
fn a_rev_that_changes_no_code_is_information_and_one_that_does_is_a_problem() {
    let (_tmp, g) = captured();
    // Nothing here can change a golden: docs, a qualifier record file under crates/ (the 0cac69e
    // case), and a test source.
    fs::write(g.root.join("NOTES.md"), "docs only\n").unwrap();
    fs::create_dir_all(g.root.join("crates/x/src")).unwrap();
    fs::write(
        g.root.join("crates/x/src/.qual"),
        "{\"type\":\"annotation\"}\n",
    )
    .unwrap();
    fs::create_dir_all(g.root.join("crates/x/tests")).unwrap();
    fs::write(g.root.join("crates/x/tests/t.rs"), "#[test] fn t() {}\n").unwrap();
    git(&g.root, &["add", "."]);
    git(&g.root, &["commit", "-q", "-m", "docs, .qual, test"]);
    let r = g.check(None).unwrap();
    assert!(r.problems.is_empty(), "{}", shown(&r));
    assert!(
        mentions(&r.info, "code and fixtures unchanged"),
        "{}",
        shown(&r)
    );

    // A Rust source under crates/ does count.
    fs::write(g.root.join("crates/x/src/lib.rs"), "pub fn moved() {}\n").unwrap();
    git(&g.root, &["add", "."]);
    git(&g.root, &["commit", "-q", "-m", "code"]);
    let r = g.check(None).unwrap();
    assert!(
        mentions(&r.problems, "PRODUCER MOVED repo_rev"),
        "{}",
        shown(&r)
    );
}
