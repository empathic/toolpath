//! Integration tests for share targets: the configured default, and
//! object storage (S3 or a folder).
//!
//! Every test runs against a local folder — the same `object_store`
//! code path an `s3://` bucket takes, minus the network. That's the
//! point of making folders first-class: the plumbing under test
//! (target resolution, destination parsing, key layout, upload,
//! download, cache landing) is backend-independent, so exercising it
//! locally covers the S3 case without credentials or a mock endpoint.

#![cfg(not(target_os = "emscripten"))]

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn cmd(config_dir: &Path) -> Command {
    let mut c = Command::cargo_bin("path").unwrap();
    c.env("TOOLPATH_CONFIG_DIR", config_dir);
    // Keep an ambient developer AWS profile or share target out of the
    // test's way.
    for k in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_ENDPOINT_URL",
        "AWS_ENDPOINT_URL_S3",
        "AWS_PROFILE",
        "TOOLPATH_SHARE_TARGET",
    ] {
        c.env_remove(k);
    }
    // Credential resolution reads `~/.aws` now, so point it at files
    // that don't exist. Without this a developer's real default profile
    // leaks in and the "no credentials" tests make real network calls.
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
                    "id": "s1",
                    "parents": [],
                    "actor": "agent:claude-code",
                    "timestamp": "2026-01-01T00:00:00Z"
                },
                "change": {
                    "claude-code://share-target-int": {
                        "structural": {
                            "type": "conversation.append",
                            "role": "user",
                            "text": "hello"
                        }
                    }
                }
            }]
        }]
    });
    let p = dir.join("doc.json");
    std::fs::write(&p, serde_json::to_string(&body).unwrap()).unwrap();
    p
}

// ── path target ───────────────────────────────────────────────

#[test]
fn a_folder_can_be_designated_as_the_share_target() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let folder_str = folder.path().to_string_lossy().into_owned();

    cmd(config.path())
        .args(["target", &folder_str])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Share target set to {folder_str}"
        )))
        // A folder needs no credentials, so nothing should nag about them.
        .stdout(predicate::str::contains("credentials").not());

    // Stored as a URL so it can't be re-read relative to another cwd…
    let raw = std::fs::read_to_string(config.path().join("config.json")).unwrap();
    assert!(raw.contains("file://"), "{raw}");
    // …but reported back as the path the user typed.
    cmd(config.path())
        .args(["target"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&folder_str))
        .stdout(predicate::str::contains("configured default"));
}

/// Point the CLI at an endpoint nothing is listening on, so
/// verification fails deterministically and offline.
fn unreachable_s3(config: &Path) {
    cmd(config)
        .args(["auth", "s3", "login"])
        .args(["--endpoint", "http://127.0.0.1:1"])
        .args(["--access-key-id", "AK", "--secret-access-key", "SK"])
        .assert()
        .success();
}

#[test]
fn a_bucket_that_cant_be_written_to_is_refused_at_configuration_time() {
    // The whole point of checking here: a target is set once and used
    // many times, so a wrong one must not survive to cost a session
    // pick and a derivation later.
    let config = tempfile::tempdir().unwrap();
    unreachable_s3(config.path());

    cmd(config.path())
        .args(["target", "s3://my-bucket/traces"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "can't write to s3://my-bucket/traces",
        ))
        .stderr(predicate::str::contains("--no-verify"))
        // object_store's retry epilogue is not an answer to anything.
        .stderr(predicate::str::contains("max_retries").not());

    assert!(
        !config.path().join("config.json").exists(),
        "a target that failed verification must not be stored"
    );
}

#[test]
fn no_verify_stores_an_unchecked_target() {
    let config = tempfile::tempdir().unwrap();
    unreachable_s3(config.path());

    cmd(config.path())
        .args(["target", "s3://my-bucket/traces", "--no-verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(not verified)"));

    let raw = std::fs::read_to_string(config.path().join("config.json")).unwrap();
    assert!(raw.contains("s3://my-bucket/traces"), "{raw}");
}

#[test]
fn designating_a_folder_creates_it_and_leaves_no_probe_behind() {
    // Verification is a real write, so it proves the folder is usable
    // and makes it exist — and cleans up after itself.
    let config = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let folder = parent.path().join("brand/new/traces");

    cmd(config.path())
        .args(["target", &folder.to_string_lossy()])
        .assert()
        .success();

    assert!(folder.is_dir(), "the folder should exist after designation");
    let leftovers: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(leftovers.is_empty(), "probe not cleaned up: {leftovers:?}");
}

#[test]
fn an_unwritable_folder_is_refused_at_configuration_time() {
    let config = tempfile::tempdir().unwrap();
    // A file, not a directory: nothing can be written underneath it.
    let blocker = tempfile::tempdir().unwrap();
    let occupied = blocker.path().join("a-file");
    std::fs::write(&occupied, "not a directory").unwrap();

    cmd(config.path())
        .args(["target", &occupied.join("traces").to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("can't write to"));
}

#[test]
fn target_with_no_argument_explains_the_options() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No share target"))
        .stdout(predicate::str::contains("In effect now: pathbase"))
        .stdout(predicate::str::contains("path target ~/"));
}

#[test]
fn target_clear_falls_back_to_pathbase() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target", "s3://my-bucket/traces", "--no-verify"])
        .assert()
        .success();
    cmd(config.path())
        .args(["target", "--clear"])
        .assert()
        .success()
        .stdout(predicate::str::contains("uploads to Pathbase"));
    cmd(config.path())
        .args(["target"])
        .assert()
        .success()
        .stdout(predicate::str::contains("In effect now: pathbase"));
}

#[test]
fn the_env_var_overrides_the_stored_default() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target", "s3://stored/x", "--no-verify"])
        .assert()
        .success();
    cmd(config.path())
        .args(["target"])
        .env("TOOLPATH_SHARE_TARGET", "s3://from-env/y")
        .assert()
        .success()
        // The stored value is still reported as stored…
        .stdout(predicate::str::contains("s3://stored/x"))
        // …but the env var is what would actually be used.
        .stdout(predicate::str::contains(
            "In effect now: s3://from-env/y (TOOLPATH_SHARE_TARGET)",
        ));
}

#[test]
fn a_scheme_less_target_is_a_folder_not_a_bucket() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target", &folder.path().to_string_lossy()])
        .assert()
        .success();
    let raw = std::fs::read_to_string(config.path().join("config.json")).unwrap();
    assert!(
        raw.contains("file://"),
        "a bare path must mean a folder: {raw}"
    );
    assert!(!raw.contains("s3://"), "{raw}");
}

#[test]
fn an_unsupported_scheme_is_rejected() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target", "gs://bucket/x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("s3://"));
}

// ── path auth s3 ────────────────────────────────────────────────────

#[test]
fn auth_s3_login_stores_status_shows_and_logout_clears() {
    let config = tempfile::tempdir().unwrap();

    cmd(config.path())
        .args([
            "auth",
            "s3",
            "login",
            "--region",
            "eu-west-1",
            "--access-key-id",
            "AKIAEXAMPLE",
            "--secret-access-key",
            "supersecretvalue",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("S3 settings saved"))
        // Credentials alone don't redirect `path share` — say so.
        .stdout(predicate::str::contains("path target"));

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
        .stdout(predicate::str::contains("****alue"));

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

#[test]
fn s3_credentials_without_a_default_refuse_to_publish_anonymously() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["auth", "s3", "login", "--access-key-id", "AKIAEXAMPLE"])
        .assert()
        .success();

    // Not logged into Pathbase, S3 credentials present, no default:
    // silently uploading to the anonymous public endpoint would be the
    // worst possible guess.
    cmd(config.path())
        .args(["p", "export", "object", "--input", doc.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path target"));
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

    let docs = config.path().join("documents");
    let ids: Vec<String> = std::fs::read_dir(&docs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "expected one cached doc, got {ids:?}");
    assert!(ids[0].starts_with("file-"), "unexpected cache id: {ids:?}");
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
fn export_object_uses_the_configured_default_target() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["target", &format!("{}/traces", folder.path().display())])
        .assert()
        .success();

    cmd(config.path())
        .args(["p", "export", "object", "--input", doc.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "/traces/2026-01-01-hello-doc.json",
        ));

    assert!(
        folder
            .path()
            .join("traces/2026-01-01-hello-doc.json")
            .is_file()
    );
}

#[test]
fn export_object_with_a_pathbase_default_says_so() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());

    cmd(config.path())
        .args(["target", "pathbase"])
        .assert()
        .success();

    cmd(config.path())
        .args(["p", "export", "object", "--input", doc.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("p export pathbase"));
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

// ── path share ──────────────────────────────────────────────────────

#[test]
fn a_shared_object_keeps_its_name_when_the_session_is_re_shared() {
    // Re-sharing must overwrite its own object, not leave a trail of
    // near-duplicates — the reason every part of the name is a pure
    // function of the document.
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
fn a_bare_relative_target_is_rejected_rather_than_creating_a_folder() {
    // The trap: a bucket name typed from memory becomes ./my-bucket
    // under the cwd, and the share reports success.
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
        "a rejected target must not leave a directory behind"
    );
}

#[test]
fn target_rejects_a_bare_relative_value_too() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["target", "my-bucket/traces"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("s3://my-bucket/traces"));
}

#[test]
fn share_rejects_pathbase_flags_alongside_an_object_target() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["share", "--to", "s3://b/p", "--public"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--to pathbase"));
}

#[test]
fn share_rejects_an_unparseable_target_before_doing_any_work() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["share", "--to", "gs://bucket/x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("s3://"));
}
