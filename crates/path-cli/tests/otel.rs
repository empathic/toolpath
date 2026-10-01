//! `path p import otel` end to end: the directory rule, the summary line,
//! `--session`, the cache, and schema validation of every derived document.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The CLI under test, with the developer's Claude config root kept out
/// of it (see `tests/integration.rs`).
fn cmd() -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env_remove("CLAUDE_CONFIG_DIR");
    c
}

/// `path p import otel`, sandboxed under `home` (cache and config).
fn import(home: &Path) -> Command {
    let mut c = cmd();
    c.env("HOME", home)
        .env("TOOLPATH_CONFIG_DIR", home.join(".toolpath"))
        .args(["p", "import", "otel"]);
    c
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel/openrouter")
}

fn encodings() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/otel/encodings")
}

/// (stdout, stderr) of a finished command.
fn output(a: &assert_cmd::assert::Assert) -> (String, String) {
    let o = a.get_output();
    (
        String::from_utf8(o.stdout.clone()).unwrap(),
        String::from_utf8(o.stderr.clone()).unwrap(),
    )
}

/// The documents a `--no-cache` import printed, one JSON per line.
fn docs(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// One OpenRouter `LLM Generation` delivery with a one-message prompt.
fn one_request(id: &str, start: u64, session: &str) -> Value {
    let attr = |k: &str, v: &str| json!({"key": k, "value": {"stringValue": v}});
    json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": format!("t-{id}"), "spanId": "r", "name": "LLM Generation",
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": (start + 1).to_string(),
        "attributes": [
            attr("gen_ai.response.id", id), attr("session.id", session),
            attr("gen_ai.prompt", r#"{"messages":[{"role":"user","content":"hi"}]}"#),
            attr("gen_ai.completion", r#"{"completion":"ok"}"#)
        ]
    }]}]}]})
}

fn copy(from: &Path, to_dir: &Path) {
    std::fs::copy(from, to_dir.join(from.file_name().unwrap())).unwrap();
}

#[test]
fn a_capture_directory_imports_one_document_per_session() {
    let home = tempfile::tempdir().unwrap();
    let a = import(home.path())
        .arg("--input")
        .arg(fixtures())
        .assert()
        .success();
    let (stdout, stderr) = output(&a);
    let files: Vec<&str> = stdout.lines().collect();
    assert_eq!(files.len(), 5, "stdout: {stdout}\nstderr: {stderr}");
    for f in &files {
        assert!(f.contains("otel-"), "{f}");
    }
    for want in [
        "otel: 5 sessions;",
        "error-status=1",
        "connection-test=1",
        "unclaimed=0",
        "not-otlp=1",
    ] {
        assert!(stderr.contains(want), "stderr lacks {want:?}: {stderr}");
    }
    assert!(
        !stderr.contains("expected.json"),
        "per-file skip line: {stderr}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|l| l.starts_with("otel: ") && l.contains(" session"))
            .count(),
        1,
        "exactly one summary line: {stderr}"
    );
    // Spec: every derived document validates against the v1.1.0 schema.
    for f in files {
        cmd()
            .args(["p", "validate", "--input", f])
            .assert()
            .success()
            .stdout(predicate::str::contains("Valid"));
    }
}

#[test]
fn a_single_non_otlp_file_fails() {
    let home = tempfile::tempdir().unwrap();
    import(home.path())
        .arg("--input")
        .arg(fixtures().join("expected.json"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("expected.json").and(predicate::str::contains("not OTLP")),
        );
}

#[test]
fn session_filter_selects_one_session() {
    let home = tempfile::tempdir().unwrap();
    let a = import(home.path())
        .args([
            "--no-cache",
            "--session",
            "177b923f-8cf6-42fc-9f30-9a7b86236265",
            "--input",
        ])
        .arg(fixtures().join("claude-code.ndjson"))
        .assert()
        .success();
    let docs = docs(&output(&a).0);
    assert_eq!(docs.len(), 1);
    let meta = &docs[0]["paths"][0]["meta"];
    assert_eq!(meta["source"], "otel");
    assert_eq!(meta["otel"]["harness"], "claude-code");
    assert_eq!(
        meta["otel"]["derived_session_id"],
        "218c08e0-b5d5-8a6a-8b60-a48793ec6a2b"
    );
}

#[test]
fn session_filter_without_a_match_fails() {
    let home = tempfile::tempdir().unwrap();
    import(home.path())
        .args(["--no-cache", "--session", "nope", "--input"])
        .arg(fixtures().join("claude-code.ndjson"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("nope"));
}

#[test]
fn session_filter_matching_two_sessions_fails_and_lists_both() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Session B's client id is session A's derived id.
    let derived_a = toolpath_otel::derived_session_id("sess-a");
    std::fs::write(
        dir.path().join("a.json"),
        one_request("g-a", 10, "sess-a").to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.json"),
        one_request("g-b", 20, &derived_a).to_string(),
    )
    .unwrap();
    import(home.path())
        .args(["--no-cache", "--session", &derived_a, "--input"])
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("matches 2 sessions")
                .and(predicate::str::contains("sess-a"))
                .and(predicate::str::contains(derived_a.as_str())),
        );
}

#[test]
fn reimport_needs_force() {
    let home = tempfile::tempdir().unwrap();
    let input = fixtures().join("claude-code.ndjson");
    import(home.path())
        .arg("--input")
        .arg(&input)
        .assert()
        .success();
    import(home.path())
        .arg("--input")
        .arg(&input)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    import(home.path())
        .args(["--force", "--input"])
        .arg(&input)
        .assert()
        .success();
}

#[test]
fn subdirectories_are_not_entered() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    copy(&fixtures().join("claude-code.ndjson"), dir.path());
    // A nested capture layout (`<provider>/span/…`) holding a session
    // that exists nowhere else: entering it would add a document.
    let nested = dir.path().join("openrouter").join("span");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("extra.json"),
        one_request("g-nested", 10, "nested-only").to_string(),
    )
    .unwrap();
    let a = import(home.path())
        .args(["--no-cache", "--input"])
        .arg(dir.path())
        .assert()
        .success();
    let (stdout, stderr) = output(&a);
    assert_eq!(docs(&stdout).len(), 1, "stderr: {stderr}");
    assert!(stderr.contains("otel: 1 session;"), "{stderr}");
    assert!(has_token(&stderr, "not-otlp=0"), "{stderr}");
    assert!(!stderr.contains("extra.json"), "{stderr}");
}

/// `token` is one whole whitespace-separated word of the summary (so
/// `duplicate=1` does not match `duplicate=10`).
fn has_token(summary: &str, token: &str) -> bool {
    summary
        .split_whitespace()
        .any(|w| w.trim_end_matches([';', ',']) == token)
}

#[test]
fn one_capture_in_two_encodings_imports_once() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    copy(&encodings().join("body.json"), dir.path());
    copy(&encodings().join("body.json.gz"), dir.path());
    let a = import(home.path())
        .args(["--no-cache", "--input"])
        .arg(dir.path())
        .assert()
        .success();
    let (stdout, stderr) = output(&a);
    assert_eq!(docs(&stdout).len(), 1, "stderr: {stderr}");
    assert!(has_token(&stderr, "duplicate=1"), "{stderr}");
}

#[test]
fn session_filter_takes_cluster_keys_and_derived_ids() {
    let home = tempfile::tempdir().unwrap();
    // pi sends no session id: its session key is a Layer 2 cluster key.
    let a = import(home.path())
        .args([
            "--no-cache",
            "--session",
            "otel-cluster:4c9f0c410e076f96",
            "--input",
        ])
        .arg(fixtures().join("pi.ndjson"))
        .assert()
        .success();
    assert_eq!(docs(&output(&a).0).len(), 1);
    // A derived id copied from an earlier import.
    let a = import(home.path())
        .args([
            "--no-cache",
            "--session",
            "218c08e0-b5d5-8a6a-8b60-a48793ec6a2b",
            "--input",
        ])
        .arg(fixtures().join("claude-code.ndjson"))
        .assert()
        .success();
    let docs = docs(&output(&a).0);
    assert_eq!(docs.len(), 1);
    assert_eq!(
        docs[0]["paths"][0]["meta"]["otel"]["session_id"],
        "177b923f-8cf6-42fc-9f30-9a7b86236265"
    );
}

#[test]
fn a_broken_file_in_a_directory_fails_naming_it() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    copy(&fixtures().join("claude-code.ndjson"), dir.path());
    std::fs::write(dir.path().join("broken.json"), "{").unwrap();
    import(home.path())
        .args(["--no-cache", "--input"])
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("broken.json"));
}

#[test]
fn a_gzip_file_imports() {
    let home = tempfile::tempdir().unwrap();
    let a = import(home.path())
        .args(["--no-cache", "--input"])
        .arg(encodings().join("body.json.gz"))
        .assert()
        .success();
    assert_eq!(docs(&output(&a).0).len(), 1);
}

#[test]
fn a_body_mixing_sessions_imports_one_document_each() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let span = |v: Value| v["resourceSpans"][0]["scopeSpans"][0]["spans"][0].clone();
    let mut idless = one_request("g-c", 30, "unused");
    idless["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
        .as_array_mut()
        .unwrap()
        .retain(|kv| kv["key"] != "session.id");
    let body = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
        span(one_request("g-a", 10, "sess-a")),
        span(one_request("g-b", 20, "sess-b")),
        span(idless),
    ]}]}]});
    let file = dir.path().join("mixed.json");
    std::fs::write(&file, body.to_string()).unwrap();
    let a = import(home.path())
        .args(["--no-cache", "--input"])
        .arg(&file)
        .assert()
        .success();
    let (stdout, stderr) = output(&a);
    let docs = docs(&stdout);
    assert_eq!(docs.len(), 3, "{stderr}");
    assert!(stderr.contains("otel: 3 sessions;"), "{stderr}");
    let ids: Vec<&str> = docs
        .iter()
        .map(|d| {
            d["paths"][0]["meta"]["otel"]["session_id"]
                .as_str()
                .unwrap_or("-")
        })
        .collect();
    assert_eq!(ids, ["sess-a", "sess-b", "-"]);
}
