#![allow(deprecated)]

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}

fn cmd() -> Command {
    Command::cargo_bin("path").unwrap()
}

// ── Git fixture ──────────────────────────────────────────────────────

/// Creates a temporary git repo with a known commit history for testing.
///
/// Layout (all on branch `main`):
///   commit 1: "initial commit"  — creates main.rs with "fn main() {}"
///   commit 2: "fix the bug"     — changes main.rs to "fn main() { fixed() }"
///
/// Returns (temp_dir, branch_name). Temp dir must be kept alive for the
/// repo to remain on disk.
fn git_fixture() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Alice Dev").unwrap();
    config.set_str("user.email", "alice@example.com").unwrap();

    // Commit 1
    let mut index = repo.index().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
    index
        .add_path(std::path::Path::new("main.rs"))
        .unwrap();
    index.write().unwrap();
    let tree1 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    let oid1 = repo
        .commit(Some("HEAD"), &sig, &sig, "initial commit", &tree1, &[])
        .unwrap();
    let commit1 = repo.find_commit(oid1).unwrap();

    // Commit 2
    std::fs::write(dir.path().join("main.rs"), "fn main() { fixed() }").unwrap();
    index
        .add_path(std::path::Path::new("main.rs"))
        .unwrap();
    index.write().unwrap();
    let tree2 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "fix the bug",
        &tree2,
        &[&commit1],
    )
    .unwrap();

    // Determine the branch name (main or master depending on git config)
    let head = repo.head().unwrap();
    let branch = head.shorthand().unwrap().to_string();

    (dir, branch)
}

// ── Validate ─────────────────────────────────────────────────────────

#[test]
fn validate_valid_step() {
    cmd()
        .arg("validate")
        .arg("--input")
        .arg(examples_dir().join("step-01-minimal.json"))
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid"));
}

#[test]
fn validate_invalid_json() {
    let dir = std::env::temp_dir();
    let tmp_file = dir.join("toolpath-integration-invalid.json");
    std::fs::write(&tmp_file, "{ not valid json }").unwrap();

    cmd()
        .arg("validate")
        .arg("--input")
        .arg(&tmp_file)
        .assert()
        .failure();

    let _ = std::fs::remove_file(&tmp_file);
}

// ── Derive git ───────────────────────────────────────────────────────

#[test]
fn derive_git_produces_path() {
    let (dir, branch) = git_fixture();

    cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"Path\""))
        .stdout(predicate::str::contains("\"head\":"))
        .stdout(predicate::str::contains("\"steps\""));
}

#[test]
fn derive_git_has_correct_actor() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .arg("--pretty")
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let path = &json["Path"];

    // Actor is derived from git author email username (alice@example.com → alice)
    let step = &path["steps"][0];
    assert_eq!(step["step"]["actor"], "human:alice");

    // Actor metadata in path.meta.actors
    let actors = &path["meta"]["actors"];
    let alice = &actors["human:alice"];
    assert_eq!(alice["name"], "Alice Dev");
    assert_eq!(alice["identities"][0]["id"], "alice@example.com");
}

#[test]
fn derive_git_has_change_with_diff() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .arg("--pretty")
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let step = &json["Path"]["steps"][0];

    // The step should have a change for main.rs with a raw diff
    let change = &step["change"]["main.rs"];
    let raw = change["raw"].as_str().unwrap();
    assert!(raw.contains("-fn main() {}"), "diff should show old content");
    assert!(
        raw.contains("+fn main() { fixed() }"),
        "diff should show new content"
    );
}

#[test]
fn derive_git_has_intent_from_commit_message() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .arg("--pretty")
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let step = &json["Path"]["steps"][0];

    // meta.intent is the commit message
    assert_eq!(step["meta"]["intent"], "fix the bug");
}

#[test]
fn derive_git_has_base_uri() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .arg("--pretty")
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let base = &json["Path"]["path"]["base"];

    // base.uri should be a file:// URL pointing to the repo
    let uri = base["uri"].as_str().unwrap();
    assert!(uri.starts_with("file://"), "Expected file:// URI, got {}", uri);

    // base.ref should be a commit hash (40 hex chars)
    let git_ref = base["ref"].as_str().unwrap();
    assert_eq!(git_ref.len(), 40);
    assert!(git_ref.chars().all(|c| c.is_ascii_hexdigit()));
}

// ── Derive → validate roundtrip ─────────────────────────────────────

#[test]
fn derive_git_validate_roundtrip() {
    let (dir, branch) = git_fixture();
    let tmp_file = std::env::temp_dir().join("toolpath-integration-roundtrip.json");

    let derive_output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .output()
        .unwrap();
    assert!(derive_output.status.success());
    std::fs::write(&tmp_file, &derive_output.stdout).unwrap();

    cmd()
        .arg("validate")
        .arg("--input")
        .arg(&tmp_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid"));

    let _ = std::fs::remove_file(&tmp_file);
}

// ── Render ───────────────────────────────────────────────────────────

#[test]
fn render_dot_from_stdin() {
    let input = std::fs::read_to_string(examples_dir().join("path-01-pr.json")).unwrap();

    cmd()
        .arg("render")
        .arg("dot")
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("digraph"));
}

// ── Query ────────────────────────────────────────────────────────────

#[test]
fn query_dead_ends() {
    cmd()
        .arg("query")
        .arg("dead-ends")
        .arg("--input")
        .arg(examples_dir().join("path-01-pr.json"))
        .assert()
        .success()
        .stdout(predicate::str::contains("step-002a"));
}

#[test]
fn query_ancestors() {
    cmd()
        .arg("query")
        .arg("ancestors")
        .arg("--input")
        .arg(examples_dir().join("path-01-pr.json"))
        .arg("--step-id")
        .arg("step-004")
        .assert()
        .success()
        .stdout(predicate::str::contains("step-001"))
        .stdout(predicate::str::contains("step-004"));
}

// ── Merge ────────────────────────────────────────────────────────────

#[test]
fn merge_produces_graph() {
    cmd()
        .arg("merge")
        .arg(examples_dir().join("path-01-pr.json"))
        .arg(examples_dir().join("path-02-local-session.json"))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"Graph\""));
}
