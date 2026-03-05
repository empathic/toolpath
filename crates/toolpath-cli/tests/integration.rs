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

#[test]
fn derive_git_produces_path() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(&repo_root)
        .arg("--branch")
        .arg("main")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"Path\""));
}

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

#[test]
fn derive_git_validate_roundtrip() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    let dir = std::env::temp_dir();
    let tmp_file = dir.join("toolpath-integration-roundtrip.json");

    let derive_output = cmd()
        .arg("derive")
        .arg("git")
        .arg("--repo")
        .arg(&repo_root)
        .arg("--branch")
        .arg("main")
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
