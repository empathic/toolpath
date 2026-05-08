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
    index.add_path(std::path::Path::new("main.rs")).unwrap();
    index.write().unwrap();
    let tree1 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    let oid1 = repo
        .commit(Some("HEAD"), &sig, &sig, "initial commit", &tree1, &[])
        .unwrap();
    let commit1 = repo.find_commit(oid1).unwrap();

    // Commit 2
    std::fs::write(dir.path().join("main.rs"), "fn main() { fixed() }").unwrap();
    index.add_path(std::path::Path::new("main.rs")).unwrap();
    index.write().unwrap();
    let tree2 = repo.find_tree(index.write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fix the bug", &tree2, &[&commit1])
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
        .stdout(predicate::str::contains("\"graph\":"))
        .stdout(predicate::str::contains("\"paths\":"))
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
    let path = &json["paths"][0];

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
    let step = &json["paths"][0]["steps"][0];

    // The step should have a change for main.rs with a raw diff
    let change = &step["change"]["main.rs"];
    let raw = change["raw"].as_str().unwrap();
    assert!(
        raw.contains("-fn main() {}"),
        "diff should show old content"
    );
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
    let step = &json["paths"][0]["steps"][0];

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
    let base = &json["paths"][0]["path"]["base"];

    // base.uri should be a file:// URL pointing to the repo
    let uri = base["uri"].as_str().unwrap();
    assert!(
        uri.starts_with("file://"),
        "Expected file:// URI, got {}",
        uri
    );

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
    let input = std::fs::read_to_string(examples_dir().join("path-01-pr.path.json")).unwrap();

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
        .arg(examples_dir().join("path-01-pr.path.json"))
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
        .arg(examples_dir().join("path-01-pr.path.json"))
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
        .arg(examples_dir().join("path-01-pr.path.json"))
        .arg(examples_dir().join("path-02-local-session.path.json"))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"graph\":"))
        .stdout(predicate::str::contains("\"paths\":"));
}

// ── .path.jsonl input ────────────────────────────────────────────────

#[test]
fn validate_accepts_path_jsonl() {
    cmd()
        .arg("validate")
        .arg("--input")
        .arg(examples_dir().join("path-02-local-session.path.jsonl"))
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid: Graph"));
}

#[test]
fn validate_rejects_truncated_jsonl() {
    let mut f = tempfile::Builder::new()
        .suffix(".path.jsonl")
        .tempfile()
        .unwrap();
    // No PathOpen, just garbage.
    use std::io::Write;
    writeln!(f, r#"{{"Step":"garbage"}}"#).unwrap();
    f.flush().unwrap();

    cmd()
        .arg("validate")
        .arg("--input")
        .arg(f.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid"));
}

#[test]
fn render_md_accepts_path_jsonl() {
    cmd()
        .arg("render")
        .arg("md")
        .arg("--input")
        .arg(examples_dir().join("path-03-signed-pr.path.jsonl"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn query_dead_ends_accepts_path_jsonl() {
    cmd()
        .arg("query")
        .arg("dead-ends")
        .arg("--input")
        .arg(examples_dir().join("path-04-exploration.path.jsonl"))
        .assert()
        .success();
}

#[test]
fn merge_accepts_path_jsonl() {
    cmd()
        .arg("merge")
        .arg(examples_dir().join("path-01-pr.path.jsonl"))
        .arg(examples_dir().join("path-02-local-session.path.jsonl"))
        .assert()
        .success()
        .stdout(predicate::str::contains("\"graph\":"))
        .stdout(predicate::str::contains("\"paths\":"));
}

// ── Auth ─────────────────────────────────────────────────────────────

#[test]
fn auth_help_lists_subcommands() {
    cmd()
        .arg("auth")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("login"))
        .stdout(predicate::str::contains("logout"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("whoami"));
}

#[test]
fn auth_login_against_unreachable_url_errors() {
    // Port 1 is privileged and not bound to anything — connection refused.
    cmd()
        .arg("auth")
        .arg("login")
        .arg("--url")
        .arg("http://127.0.0.1:1")
        .arg("--code")
        .arg("BCDFGHJK")
        .assert()
        .failure()
        .stderr(predicate::str::contains("127.0.0.1"));
}

// ── Import / export / cache ─────────────────────────────────────────

#[test]
fn import_help_lists_sources_including_pathbase() {
    cmd()
        .arg("import")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("git"))
        .stdout(predicate::str::contains("github"))
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("pathbase"));
}

#[test]
fn export_help_lists_claude_and_pathbase() {
    cmd()
        .arg("export")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("pathbase"));
}

#[test]
fn import_git_no_cache_emits_stdout_json() {
    let (dir, branch) = git_fixture();

    cmd()
        .arg("import")
        .arg("git")
        .arg("--no-cache")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"graph\":"))
        .stdout(predicate::str::contains("\"paths\":"))
        .stdout(predicate::str::contains("\"steps\""));
}

#[test]
fn import_git_writes_cache_and_prints_path() {
    let (dir, branch) = git_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .arg("import")
        .arg("git")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .assert()
        .success()
        .stdout(predicate::str::contains(".json"))
        .stderr(predicate::str::contains("Imported"));
}

#[test]
fn import_git_errors_on_existing_cache_without_force() {
    let (dir, branch) = git_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--force", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();
}

#[test]
fn cache_ls_on_empty_directory_prints_hint() {
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["cache", "ls"])
        .assert()
        .success()
        .stderr(predicate::str::contains("No cached"));
}

#[test]
fn cache_ls_after_import_lists_entry() {
    let (dir, branch) = git_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["cache", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git-"));
}

#[test]
fn export_pathbase_repo_flag_requires_login() {
    // `export pathbase` without --repo falls through to the anonymous
    // endpoint; --repo is the explicitly-authenticated path, so it must
    // refuse without credentials.
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "export",
            "pathbase",
            "--repo",
            "alex/pathstash",
            "--url",
            "http://127.0.0.1:1",
            "--input",
        ])
        .arg(examples_dir().join("path-01-pr.path.json"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not logged in"));
}

#[test]
fn import_pathbase_rejects_legacy_trace_id() {
    // The old `/traces/<id>` shape is gone; passing a bare token that
    // isn't a `<owner>/<repo>/<slug>` triple should fail at parse time
    // with a clear message rather than blowing up downstream.
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "pathbase", "trc_nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<owner>/<repo>/<slug>"));
}

#[test]
fn import_git_no_cache_honors_global_pretty() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("--pretty")
        .arg("import")
        .arg("git")
        .arg("--no-cache")
        .arg("--repo")
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Pretty JSON always has multi-line indentation; compact JSON never does.
    assert!(
        stdout.contains("\n  "),
        "expected pretty-printed JSON, got: {stdout}"
    );
}

#[test]
fn import_git_two_repos_on_same_branch_have_distinct_cache_ids() {
    let (dir_a, branch) = git_fixture();
    let (dir_b, _) = git_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir_a.path())
        .assert()
        .success();

    // Second import from a different repo on the same branch must NOT
    // trigger the "cache entry already exists" collision.
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir_b.path())
        .assert()
        .success();

    let ls = cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["cache", "ls"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(ls.stdout).unwrap();
    let git_entries = stdout.lines().filter(|l| l.starts_with("git-")).count();
    assert_eq!(
        git_entries, 2,
        "expected two distinct git- cache entries, got:\n{stdout}"
    );
}

// ── Deprecation aliases ─────────────────────────────────────────────

#[test]
fn derive_alias_still_works_with_warning() {
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
        .stdout(predicate::str::contains("\"graph\":"))
        .stdout(predicate::str::contains("\"paths\":"))
        .stderr(predicate::str::contains("deprecated"));
}

#[test]
fn share_help_lists_unified_picker_flags() {
    cmd()
        .args(["share", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--harness"))
        .stdout(predicate::str::contains("--session"))
        .stdout(predicate::str::contains("--project"))
        .stdout(predicate::str::contains("--anon"));
}

#[test]
fn share_explicit_args_uploads_via_anon() {
    use std::io::Write;
    use std::net::TcpListener;

    // Stand up a one-shot mock that returns a valid AnonUploadResponse.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Drain the request just enough to keep the OS happy.
        use std::io::Read;
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"abc-123","path":"/anon/x/y/abc-123","share_url":"https://example.test/anon/abc-123"}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    // Build a claude fixture so the explicit-args path has something to derive.
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    // toolpath-claude maps '/', '_', and '.' to '-' when sanitizing project
    // paths into directory slugs — mirror that here so the fixture lands
    // where the resolver looks for it.
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-abc.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("HOME", temp.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--no-cache", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "https://example.test/anon/abc-123",
        ))
        .stderr(predicate::str::contains("Uploaded"));

    server.join().unwrap();
}

/// Helper for the cache tests. Spawns a one-shot mock anon-upload server
/// on a free port and returns (port, server-thread-handle, fixture-temp,
/// project-path, $HOME-path).
fn share_anon_fixture() -> (
    u16,
    std::thread::JoinHandle<()>,
    tempfile::TempDir,
    PathBuf,
    PathBuf,
) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"abc","path":"/anon/x/y/abc","share_url":"https://example.test/anon/abc"}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    // toolpath-claude maps '/', '_', and '.' to '-' when sanitizing project
    // paths into directory slugs — mirror that here so the fixture lands
    // where the resolver looks for it.
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-abc.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    let home = temp.path().to_path_buf();
    (port, server, temp, project, home)
}

/// Spawn a one-shot mock anon-upload server on a free port. Returns the
/// port and the join handle. Used by tests that need multiple sequential
/// uploads (the default fixture builds the claude session too, which we
/// don't want to redo between runs).
fn one_shot_anon_server() -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = r#"{"id":"abc","path":"/anon/x/y/abc","share_url":"https://example.test/anon/abc"}"#;
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    (port, server)
}

/// `path share` re-run after a conversation has grown should overwrite
/// the cache file with the fresh derive — otherwise the cache and the
/// uploaded body would disagree (upload uses the in-memory fresh body,
/// cache file would be stale). Lock that contract in.
#[test]
fn share_rewrites_cache_when_session_has_grown() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    let session_file = project_dir.join("session-grow.jsonl");
    let cwd_str = project.display().to_string();
    let initial = format!(
        r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd_str}","message":{{"role":"user","content":"first"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"reply-1"}}}}
"#
    );
    std::fs::write(&session_file, &initial).unwrap();

    let cfg = tempfile::tempdir().unwrap();
    let home = temp.path();

    // First share: cache picks up the 2-turn conversation.
    let (port1, server1) = one_shot_anon_server();
    cmd()
        .env("HOME", home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share", "--harness", "claude", "--session", "session-grow", "--project"])
        .arg(&project)
        .args(["--anon", "--url"])
        .arg(format!("http://127.0.0.1:{port1}"))
        .assert()
        .success();
    server1.join().unwrap();

    let docs = cfg.path().join("documents");
    let cache_files: Vec<_> = std::fs::read_dir(&docs)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(cache_files.len(), 1, "expected one cache entry after first share");
    let cache_path = cache_files[0].path();
    let cache_v1 = std::fs::read_to_string(&cache_path).unwrap();
    assert!(cache_v1.contains("reply-1"), "v1 cache must contain reply-1");
    assert!(!cache_v1.contains("reply-2"), "v1 cache must not contain reply-2 yet");

    // Conversation continues: append two more turns to the session JSONL.
    let mut grown = initial.clone();
    grown.push_str(&format!(
        r#"{{"type":"user","uuid":"u-2","timestamp":"2024-01-02T00:00:00Z","cwd":"{cwd_str}","message":{{"role":"user","content":"second"}}}}
{{"type":"assistant","uuid":"a-2","timestamp":"2024-01-02T00:00:01Z","message":{{"role":"assistant","content":"reply-2"}}}}
"#
    ));
    std::fs::write(&session_file, &grown).unwrap();

    // Second share: must overwrite the cache file with the grown derive,
    // not silently keep the v1 contents while uploading v2.
    let (port2, server2) = one_shot_anon_server();
    cmd()
        .env("HOME", home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share", "--harness", "claude", "--session", "session-grow", "--project"])
        .arg(&project)
        .args(["--anon", "--url"])
        .arg(format!("http://127.0.0.1:{port2}"))
        .assert()
        .success();
    server2.join().unwrap();

    let cache_v2 = std::fs::read_to_string(&cache_path).unwrap();
    assert!(
        cache_v2.contains("reply-2"),
        "v2 cache should contain the new turn, got: {cache_v2}"
    );
    assert_ne!(
        cache_v1, cache_v2,
        "cache file must be rewritten when the session has grown"
    );
}

#[test]
fn share_writes_cache_by_default() {
    let (port, server, _temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success();

    let docs = cfg.path().join("documents");
    let entries: Vec<_> = std::fs::read_dir(&docs)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one cache entry, got {entries:?}"
    );
    let name = entries[0].file_name().to_string_lossy().into_owned();
    assert!(
        name.starts_with("claude-"),
        "expected claude-* cache id, got {name}"
    );

    server.join().unwrap();
}

#[test]
fn share_no_cache_skips_write() {
    let (port, server, _temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--anon", "--no-cache", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success();

    let docs = cfg.path().join("documents");
    if docs.exists() {
        let entries: Vec<_> = std::fs::read_dir(&docs)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "expected no cache entries with --no-cache, got {entries:?}"
        );
    }

    server.join().unwrap();
}

#[test]
fn share_logged_out_anon_default() {
    // No --anon flag and no credentials file => share() falls through to the
    // anonymous endpoint and emits a "not logged in — uploading anonymously"
    // notice on stderr. This covers the logged-out branch in
    // cmd_export::run_pathbase_inner that the explicit --anon tests skip.
    let (port, server, _temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .args(["--no-cache", "--url"])
        .arg(format!("http://127.0.0.1:{port}"))
        .assert()
        .success()
        .stderr(predicate::str::contains("not logged in"))
        .stderr(predicate::str::contains("uploading anonymously"));

    server.join().unwrap();
}

#[test]
fn share_filters_by_project_with_no_matches_errors() {
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let nonexistent = home.path().join("never");

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share", "--project"])
        .arg(&nonexistent)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "No agent sessions found in project",
        ));
}

#[test]
fn share_no_harness_non_tty_prints_recipe() {
    // Build a minimal claude fixture in a tempdir, point HOME at it, so
    // gather_sessions returns a non-empty Vec. Without this, an environment
    // with no agent harnesses configured (e.g. CI) would hit bail_no_sessions
    // before the fzf-unavailable recipe path. We want the recipe path here.
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let claude_dir = temp.path().join(".claude");
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = claude_dir.join("projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-recipe.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("HOME", temp.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["share"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path import"))
        .stderr(predicate::str::contains("path export pathbase"));
}
