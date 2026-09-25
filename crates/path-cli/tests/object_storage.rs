//! Integration tests for object storage: `p export object`,
//! `p import object`, and `path auth s3`.
//!
//! Every test runs against a local folder — the same `object_store`
//! code path an `s3://` bucket takes, minus the network. That's the
//! point of making folders first-class: the plumbing under test
//! (destination parsing, naming, upload, download, cache landing) is
//! backend-independent, so exercising it locally covers the S3 case
//! without credentials or a mock endpoint.

#![cfg(not(target_os = "emscripten"))]

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn cmd(config_dir: &Path) -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env("TOOLPATH_CONFIG_DIR", config_dir);
    for k in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_ENDPOINT_URL",
        "AWS_ENDPOINT_URL_S3",
        "AWS_PROFILE",
    ] {
        c.env_remove(k);
    }
    // Credential resolution reads `~/.aws`, so point it at files that
    // don't exist. Without this a developer's real default profile
    // leaks in and the "no credentials" cases make real network calls.
    c.env(
        "AWS_SHARED_CREDENTIALS_FILE",
        "/nonexistent/toolpath-test/credentials",
    );
    c.env("AWS_CONFIG_FILE", "/nonexistent/toolpath-test/config");
    c
}

/// A minimal single-step agent document, written to `dir/doc.json`.
fn write_doc(dir: &Path) -> std::path::PathBuf {
    let body = serde_json::json!({
        "graph": { "id": "g1" },
        "paths": [{
            "path": { "id": "p1", "head": "s1" },
            "steps": [{
                "step": {
                    "id": "s1", "parents": [],
                    "actor": "agent:claude-code",
                    "timestamp": "2026-01-01T00:00:00Z"
                },
                "change": { "claude-code://object-int": { "structural": {
                    "type": "conversation.append", "role": "user", "text": "hello"
                }}}
            }]
        }]
    });
    let p = dir.join("doc.json");
    std::fs::write(&p, serde_json::to_string(&body).unwrap()).unwrap();
    p
}

// ── p export object / p import object ───────────────────────────────

#[test]
fn export_then_import_round_trips_through_a_folder() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    let out = cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();
    let uri = String::from_utf8(out.get_output().stdout.clone())
        .unwrap()
        .trim()
        .to_string();

    // Legible name: date and topic lead, cache id trails. The fixture
    // is a 2026-01-01 session whose first prompt is "hello".
    assert!(
        uri.ends_with("/2026-01-01-hello-doc.json"),
        "unexpected location: {uri}"
    );
    assert!(folder.path().join("2026-01-01-hello-doc.json").is_file());

    cmd(config.path())
        .args(["p", "import", "object", &format!("file://{uri}")])
        .assert()
        .success();

    let ids: Vec<String> = std::fs::read_dir(config.path().join("documents"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "expected one cached doc, got {ids:?}");
    assert!(ids[0].starts_with("file-"), "unexpected cache id: {ids:?}");
}

#[test]
fn re_exporting_a_session_overwrites_its_own_object() {
    // Every part of the name is a pure function of the document, so a
    // re-share must not leave a trail of near-duplicates.
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    for _ in 0..2 {
        cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }

    let objects: Vec<String> = std::fs::read_dir(folder.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(objects, vec!["2026-01-01-hello-doc.json".to_string()]);
}

#[test]
fn the_s3_subcommand_alias_still_works() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["p", "export", "s3"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();
    assert!(folder.path().join("2026-01-01-hello-doc.json").is_file());
}

#[test]
fn a_bare_relative_destination_is_rejected_rather_than_creating_a_folder() {
    // The trap: a bucket name typed from memory becomes ./my-bucket
    // under the cwd, and the export reports success.
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .current_dir(work.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", "my-bucket/traces"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("s3://my-bucket/traces"))
        .stderr(predicate::str::contains("./my-bucket/traces"));

    assert!(
        !work.path().join("my-bucket").exists(),
        "a rejected destination must not leave a directory behind"
    );
}

#[test]
fn an_unsupported_scheme_is_rejected() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", "gs://bucket/x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("s3://"));
}

#[test]
fn import_object_reports_a_missing_object_clearly() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args([
            "p",
            "import",
            "object",
            &format!("file://{}/nope.json", folder.path().display()),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn import_object_rejects_a_non_toolpath_object() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("junk.json"), "{\"hello\":1}").unwrap();
    cmd(config.path())
        .args([
            "p",
            "import",
            "object",
            &format!("file://{}/junk.json", folder.path().display()),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a toolpath document"));
}

// ── path auth s3 ────────────────────────────────────────────────────

#[test]
fn auth_s3_login_stores_status_shows_and_logout_clears() {
    let config = tempfile::tempdir().unwrap();

    cmd(config.path())
        .args(["auth", "s3", "login"])
        .args(["--region", "eu-west-1"])
        .args(["--access-key-id", "AKIAEXAMPLE"])
        .args(["--secret-access-key", "supersecretvalue"])
        .assert()
        .success()
        .stdout(predicate::str::contains("S3 settings saved"));

    let stored = config.path().join("s3.json");
    assert!(stored.is_file(), "s3.json not written");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "credentials must not be world-readable"
        );
    }

    cmd(config.path())
        .args(["auth", "s3", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("eu-west-1"))
        .stdout(predicate::str::contains("AKIAEXAMPLE"))
        // The secret is stored but never printed back in full.
        .stdout(predicate::str::contains("supersecretvalue").not())
        .stdout(predicate::str::contains("****alue"))
        // And status says which source a share would actually use.
        .stdout(predicate::str::contains("credentials: stored by"));

    cmd(config.path())
        .args(["auth", "s3", "logout"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cleared"));
    assert!(!stored.exists());
}

#[test]
fn auth_s3_login_merges_into_the_existing_settings() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "login", "--access-key-id", "AKIAEXAMPLE"])
        .assert()
        .success();
    // A later, narrower call must not wipe the key.
    cmd(config.path())
        .args(["auth", "s3", "login", "--region", "us-west-2"])
        .assert()
        .success();

    let raw = std::fs::read_to_string(config.path().join("s3.json")).unwrap();
    assert!(raw.contains("AKIAEXAMPLE"), "{raw}");
    assert!(raw.contains("us-west-2"), "{raw}");
}

#[test]
fn auth_s3_status_marks_env_supplied_values() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_REGION", "ap-south-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("ap-south-1 (env)"));
}

#[test]
fn auth_s3_login_without_a_terminal_or_flags_is_an_error() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "login"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Nothing to store"));
}

// ── credential resolution, end to end ───────────────────────────────

#[test]
fn a_profile_is_picked_up_with_no_toolpath_configuration_at_all() {
    // The whole point: someone who has run `aws configure` gets S3
    // access without telling us anything.
    let config = tempfile::tempdir().unwrap();
    let aws = tempfile::tempdir().unwrap();
    let creds = aws.path().join("credentials");
    std::fs::write(
        &creds,
        "[default]\naws_access_key_id = AKIAPROFILE\naws_secret_access_key = s3cret\n",
    )
    .unwrap();

    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_SHARED_CREDENTIALS_FILE", &creds)
        .assert()
        .success()
        .stdout(predicate::str::contains("credentials: profile `default`"));
}

#[test]
fn no_credentials_anywhere_reports_the_instance_chain_not_a_failure() {
    // On a server this is the correct answer, not an error.
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EC2/ECS/EKS credential chain"));
}

#[test]
fn an_unknown_profile_says_which_profile_and_how_to_list_them() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_PROFILE", "typo")
        .assert()
        .success()
        .stdout(predicate::str::contains("no such profile"))
        .stdout(predicate::str::contains("aws configure list-profiles"));
}
