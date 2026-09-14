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

/// A minimal single-step agent document with graph id `id`, written to
/// `dir/doc.json`. Two documents with different ids and the same
/// basename are how collision tests are built.
fn write_doc_with_id(dir: &Path, id: &str) -> std::path::PathBuf {
    let body = serde_json::json!({
        "graph": { "id": id },
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

fn write_doc(dir: &Path) -> std::path::PathBuf {
    write_doc_with_id(dir, "g1")
}

fn folder_names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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

    // Legible name: date and topic lead, the document's graph ID trails.
    // The fixture is a 2026-01-01 session whose first prompt is "hello".
    assert!(
        uri.ends_with("/2026-01-01-hello--g1.json"),
        "unexpected location: {uri}"
    );
    assert!(folder.path().join("2026-01-01-hello--g1.json").is_file());

    cmd(config.path())
        .args(["p", "import", "object", &format!("file://{uri}")])
        .assert()
        .success();

    let ids: Vec<String> = std::fs::read_dir(config.path().join("documents"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        ids,
        vec!["object-g1.json".to_string()],
        "unexpected cache id: {ids:?}"
    );
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
    assert_eq!(objects, vec!["2026-01-01-hello--g1.json".to_string()]);
}

#[test]
fn two_documents_with_the_same_basename_land_on_two_keys() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let doc_a = write_doc_with_id(a.path(), "path-claude-code-aaaa");
    let doc_b = write_doc_with_id(b.path(), "path-claude-code-bbbb");

    for doc in [&doc_a, &doc_b] {
        cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    assert_eq!(
        folder_names(folder.path()),
        vec![
            "2026-01-01-hello--path-claude-code-aaaa.json".to_string(),
            "2026-01-01-hello--path-claude-code-bbbb.json".to_string(),
        ]
    );
}

#[test]
fn the_same_document_from_a_cache_id_and_a_file_lands_on_one_key() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc_with_id(work.path(), "path-claude-code-aaaa");
    // The same bytes under a cache ID that has nothing to do with the
    // file's basename.
    let documents = config.path().join("documents");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::copy(&doc, documents.join("claude-path-claude-code-aaaa.json")).unwrap();

    for input in [doc.to_str().unwrap(), "claude-path-claude-code-aaaa"] {
        cmd(config.path())
            .args(["p", "export", "object"])
            .args(["--input", input])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    assert_eq!(
        folder_names(folder.path()),
        vec!["2026-01-01-hello--path-claude-code-aaaa.json".to_string()]
    );
}

#[test]
fn a_non_document_is_refused_before_anything_is_written() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let junk = work.path().join("id_rsa");
    std::fs::write(&junk, "PRIVATE KEY MATERIAL\nnot json at all\n").unwrap();

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", junk.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a toolpath document"));
    assert!(folder_names(folder.path()).is_empty());
}

#[test]
fn a_schema_invalid_document_is_refused_unless_forced() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    // Parses as a Graph (the Rust struct only requires `actor` to be a
    // string), but the schema's actor pattern (`type:name`) rejects it.
    let bad = work.path().join("bad.json");
    std::fs::write(
        &bad,
        r#"{"graph":{"id":"g-bad"},"paths":[{"path":{"id":"p","head":"s"},"steps":[{"step":{"id":"s","actor":"not-a-valid-actor","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#,
    )
    .unwrap();

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", bad.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid toolpath document"));
    assert!(folder_names(folder.path()).is_empty());

    cmd(config.path())
        .args(["p", "export", "object", "--force"])
        .args(["--input", bad.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading anyway"));
    assert_eq!(folder_names(folder.path()).len(), 1);
}

#[test]
fn a_document_whose_graph_id_has_no_usable_characters_is_refused() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc_with_id(work.path(), "!!!");

    cmd(config.path())
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no usable characters"));
    assert!(folder_names(folder.path()).is_empty());
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
    assert!(folder.path().join("2026-01-01-hello--g1.json").is_file());
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
        .stdout(predicate::str::contains("credentials:       stored by"));

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
fn auth_s3_status_reports_env_keys_as_the_environment() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .env("AWS_REGION", "us-west-2")
        .assert()
        .success()
        .stdout(predicate::str::contains("AWS_ACCESS_KEY_ID (environment)"))
        .stdout(predicate::str::contains("stored by").not());
}

#[test]
fn auth_s3_status_prints_the_key_id_for_a_profile_and_skips_the_login_advice() {
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
        .stdout(predicate::str::contains("access key id:     AKIAPROFILE"))
        .stdout(predicate::str::contains(
            "region:            us-east-1 (default)",
        ))
        .stdout(predicate::str::contains("Run `path auth s3 login`").not());
}

#[test]
fn auth_s3_status_advises_login_only_when_nothing_resolves() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["auth", "s3", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EC2/ECS/EKS credential chain"))
        .stdout(predicate::str::contains("Run `path auth s3 login`"));
}

#[test]
fn auth_s3_whoami_runs_sts_with_the_resolved_credentials() {
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("env.log");
    let aws = bin.join("aws");
    std::fs::write(
        &aws,
        format!(
            "#!/bin/sh\necho \"$AWS_ACCESS_KEY_ID\" >> {}\nif [ \"$1\" = \"sts\" ]; then echo '{{\"Account\":\"123456789012\",\"Arn\":\"arn:aws:iam::123456789012:user/alex\",\"UserId\":\"AIDAEXAMPLE\"}}'; exit 0; fi\nexit 1\n",
            log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&aws, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .args(["auth", "s3", "whoami"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "arn:aws:iam::123456789012:user/alex",
        ))
        .stdout(predicate::str::contains("account:     123456789012"))
        .stdout(predicate::str::contains(
            "credentials: AWS_ACCESS_KEY_ID (environment)",
        ));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "AKIAENVENVENVENV1234"
    );
}

#[test]
fn auth_s3_whoami_without_the_aws_cli_says_so() {
    let config = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    cmd(config.path())
        .env("PATH", empty.path())
        .env("AWS_ACCESS_KEY_ID", "AKIAENVENVENVENV1234")
        .env("AWS_SECRET_ACCESS_KEY", "s3cret")
        .args(["auth", "s3", "whoami"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`aws` isn't on PATH"));
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
        .stdout(predicate::str::contains(
            "credentials:       profile `default`",
        ));
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

/// A fake `aws` on PATH that logs every invocation to `log` and reports
/// an expired SSO session, plus an `~/.aws/config` declaring an SSO
/// profile so resolution has to go through the CLI.
fn expired_sso_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("aws-calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> {}\necho 'Error loading SSO Token: Token for https://x.awsapps.com/start does not exist' >&2\nexit 255\n",
        log.display()
    );
    let aws = bin.join("aws");
    std::fs::write(&aws, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&aws, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = dir.path().join("aws-config");
    std::fs::write(
        &config,
        "[profile sso-team]\nsso_start_url = https://x.awsapps.com/start\nsso_region = us-east-1\nsso_account_id = 123456789012\nsso_role_name = Dev\nregion = us-east-1\n",
    )
    .unwrap();
    (dir, bin, log)
}

#[test]
fn a_folder_export_never_spawns_the_aws_cli() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    let (fixture, bin, log) = expired_sso_fixture();

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_CONFIG_FILE", fixture.path().join("aws-config"))
        .env("AWS_PROFILE", "sso-team")
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", &folder.path().to_string_lossy()])
        .assert()
        .success();

    assert!(
        !log.exists(),
        "the AWS CLI was spawned for a folder export: {:?}",
        std::fs::read_to_string(&log)
    );
    assert_eq!(folder_names(folder.path()).len(), 1);
}

#[test]
fn an_expired_sso_session_on_s3_fails_with_the_login_command_not_imds() {
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path());
    let (fixture, bin, log) = expired_sso_fixture();

    cmd(config.path())
        .env("PATH", &bin)
        .env("AWS_CONFIG_FILE", fixture.path().join("aws-config"))
        .env("AWS_PROFILE", "sso-team")
        .args(["p", "export", "object"])
        .args(["--input", doc.to_str().unwrap()])
        .args(["--to", "s3://audit-bucket/traces"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("aws sso login --profile sso-team"))
        .stderr(predicate::str::contains("169.254.169.254").not());

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(calls.contains("configure export-credentials"), "{calls}");
    assert!(
        !calls.contains("sso login"),
        "no terminal, so no login must be attempted: {calls}"
    );
}

// ── p list object ───────────────────────────────────────────────────

fn folder_with_two_docs(config: &Path) -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    for id in ["path-claude-code-aaaa", "path-claude-code-bbbb"] {
        let work = tempfile::tempdir().unwrap();
        let doc = write_doc_with_id(work.path(), id);
        cmd(config)
            .args(["p", "export", "object"])
            .args(["--input", doc.to_str().unwrap()])
            .args(["--to", &folder.path().to_string_lossy()])
            .assert()
            .success();
    }
    folder
}

#[test]
fn list_object_tsv_is_one_line_per_document_with_the_id_first() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    let out = cmd(config.path())
        .args([
            "p",
            "list",
            "object",
            &folder.path().to_string_lossy(),
            "--format",
            "tsv",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let mut rows: Vec<Vec<&str>> = stdout.lines().map(|l| l.split('\t').collect()).collect();
    rows.sort();
    assert_eq!(rows.len(), 2, "{stdout}");
    assert_eq!(rows[0][0], "path-claude-code-aaaa");
    assert_eq!(rows[0][1], "2026-01-01");
    assert_eq!(rows[0][2], "hello");
    assert!(
        rows[0][5].ends_with("2026-01-01-hello--path-claude-code-aaaa.json"),
        "{stdout}"
    );
}

#[test]
fn list_object_json_carries_the_parsed_name_parts() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    let out = cmd(config.path())
        .args([
            "p",
            "list",
            "object",
            &folder.path().to_string_lossy(),
            "--format",
            "json",
        ])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["source"], "object");
    let objects = v["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 2);
    let ids: Vec<&str> = objects.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"path-claude-code-aaaa"), "{ids:?}");
    assert_eq!(objects[0]["date"], "2026-01-01");
    assert_eq!(objects[0]["topic"], "hello");
    assert!(objects[0]["size"].as_u64().unwrap() > 0);
    assert!(objects[0]["uri"].as_str().unwrap().ends_with(".json"));
}

#[test]
fn list_object_on_an_empty_destination_exits_zero() {
    let config = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();

    cmd(config.path())
        .args([
            "p",
            "list",
            "object",
            &folder.path().to_string_lossy(),
            "--format",
            "tsv",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    cmd(config.path())
        .args([
            "p",
            "list",
            "object",
            &folder.path().to_string_lossy(),
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"objects\": []"));
    cmd(config.path())
        .args([
            "p",
            "list",
            "object",
            &folder.path().to_string_lossy(),
            "--format",
            "pretty",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no documents in"));
}

// ── p import object <destination> ───────────────────────────────────

#[test]
fn import_object_with_a_destination_imports_every_document_under_it() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    cmd(config.path())
        .args(["p", "import", "object", &folder.path().to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains("Imported").count(2));

    let mut ids = folder_names(&config.path().join("documents"));
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "object-path-claude-code-aaaa.json".to_string(),
            "object-path-claude-code-bbbb.json".to_string()
        ]
    );
}

#[test]
fn import_object_with_a_destination_skips_bad_objects_and_exits_nonzero() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());
    std::fs::write(folder.path().join("garbage.json"), "not json").unwrap();

    cmd(config.path())
        .args(["p", "import", "object", &folder.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipping"))
        .stderr(predicate::str::contains("garbage.json"))
        .stderr(predicate::str::contains(
            "1 object(s) could not be imported",
        ));

    assert_eq!(folder_names(&config.path().join("documents")).len(), 2);
}

// ── path resume with object storage ─────────────────────────────────

#[test]
fn resume_a_destination_without_a_terminal_points_at_the_lister() {
    let config = tempfile::tempdir().unwrap();
    let folder = folder_with_two_docs(config.path());

    cmd(config.path())
        .args([
            "resume",
            &folder.path().to_string_lossy(),
            "--harness",
            "claude",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path p list object"))
        .stderr(predicate::str::contains("fzf").not());
}

#[test]
fn resume_help_lists_object_storage_inputs() {
    let config = tempfile::tempdir().unwrap();
    cmd(config.path())
        .args(["resume", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("s3://"))
        .stdout(predicate::str::contains("s3a://"))
        .stdout(predicate::str::contains("folder"));
}
