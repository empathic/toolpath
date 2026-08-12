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
        .args(["p", "validate"])
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
        .args(["p", "validate"])
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
        .args(["p", "derive"])
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
        .args(["p", "derive"])
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
        .args(["p", "derive"])
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
        .args(["p", "derive"])
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
        .args(["p", "derive"])
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
        .args(["p", "derive"])
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
        .args(["p", "validate"])
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
        .args(["p", "render"])
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
        .arg("--input")
        .arg(examples_dir().join("path-01-pr.path.json"))
        .arg("map(select(.dead_end))")
        .assert()
        .success()
        .stdout(predicate::str::contains("step-002a"));
}

#[test]
fn query_ancestors() {
    cmd()
        .args(["p", "query", "ancestors"])
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
        .args(["p", "merge"])
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
        .args(["p", "validate"])
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
        .args(["p", "validate"])
        .arg("--input")
        .arg(f.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid"));
}

#[test]
fn render_md_accepts_path_jsonl() {
    cmd()
        .args(["p", "render"])
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
        .arg("--input")
        .arg(examples_dir().join("path-04-exploration.path.jsonl"))
        .arg("map(select(.dead_end))")
        .assert()
        .success();
}

#[test]
fn merge_accepts_path_jsonl() {
    cmd()
        .args(["p", "merge"])
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
        .args(["p", "import"])
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("git"))
        .stdout(predicate::str::contains("github"))
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("pathbase"));
}

/// Lay a minimal Copilot session out under `<home>/.copilot/session-state/<id>/`
/// and return (temp_home, session_id). The resolver honors `COPILOT_HOME`.
fn copilot_home_fixture() -> (tempfile::TempDir, String) {
    let home = tempfile::tempdir().unwrap();
    let id = "demo-sess-01";
    let dir = home.path().join("session-state").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let body = [
        r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"copilotVersion":"1.0.66","producer":"copilot-agent","context":{"cwd":"/tmp/demo","gitRoot":"/tmp/demo","repository":"acme/demo","branch":"main"}}}"#,
        r#"{"type":"user.message","timestamp":"2026-06-30T10:00:01.000Z","data":{"content":"hello copilot"}}"#,
        r#"{"type":"assistant.turn_start","data":{}}"#,
        r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"content":"hi there","model":"claude-haiku-4.5"}}"#,
        r#"{"type":"assistant.turn_end","data":{}}"#,
    ]
    .join("\n");
    std::fs::write(dir.join("events.jsonl"), body).unwrap();
    (home, id.to_string())
}

#[test]
fn import_help_lists_copilot() {
    cmd()
        .args(["p", "import", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("copilot"));
}

#[test]
fn import_copilot_writes_cache() {
    let (home, id) = copilot_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .args(["p", "import", "copilot", "--session", &id])
        .env("COPILOT_HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .assert()
        .success()
        // The cache file path is printed to stdout; the summary to stderr.
        .stdout(predicate::str::contains("copilot-path-copilot-"))
        .stderr(predicate::str::contains("Imported"));
    // And the cache file actually landed.
    assert!(
        cfg.path()
            .join("documents/copilot-path-copilot-demo-ses.json")
            .exists()
    );
}

#[test]
fn list_copilot_tsv_shows_session() {
    let (home, _id) = copilot_home_fixture();
    cmd()
        .args(["p", "list", "copilot", "--format", "tsv"])
        .env("COPILOT_HOME", home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-sess-01"))
        .stdout(predicate::str::contains("hello copilot"));
}

#[test]
fn show_copilot_renders_markdown() {
    let (home, id) = copilot_home_fixture();
    cmd()
        .args(["show", "copilot", "--session", &id])
        .env("COPILOT_HOME", home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Copilot session"))
        .stdout(predicate::str::contains("hello copilot"));
}

#[test]
fn export_help_lists_copilot() {
    cmd()
        .args(["p", "export", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("copilot"));
}

#[test]
fn export_copilot_to_output_file() {
    // import → doc → `p export copilot --output` emits an events.jsonl.
    let (home, id) = copilot_home_fixture();
    let tmp = tempfile::tempdir().unwrap();
    let doc = tmp.path().join("doc.json");
    let stdout = cmd()
        .args(["p", "import", "copilot", "--session", &id, "--no-cache"])
        .env("COPILOT_HOME", home.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    std::fs::write(&doc, stdout).unwrap();

    let events = tmp.path().join("events.jsonl");
    cmd()
        .args(["p", "export", "copilot", "--input"])
        .arg(&doc)
        .arg("--output")
        .arg(&events)
        .assert()
        .success()
        .stderr(predicate::str::contains("events to"));

    let jsonl = std::fs::read_to_string(&events).unwrap();
    assert!(
        jsonl.lines().next().unwrap().contains("\"session.start\""),
        "first projected line should be session.start"
    );
    assert!(jsonl.contains("hello copilot"), "user prompt round-trips");
}

#[test]
fn export_help_lists_claude_and_pathbase() {
    cmd()
        .args(["p", "export"])
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
        .args(["p", "import"])
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
        .args(["p", "import"])
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
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "git", "--force", "--branch"])
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
        .args(["p", "cache", "ls"])
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
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git-"));
}

/// A `$HOME` with one Claude session. Returns (home-tempdir, session file).
fn claude_home_fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    // toolpath-claude maps '/', '_', and '.' to '-' when sanitizing project
    // paths into directory slugs — mirror that here so the fixture lands
    // where the resolver looks for it.
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = temp.path().join(".claude/projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    let session_file = project_dir.join("session-abc.jsonl");
    std::fs::write(
        &session_file,
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"hi"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();
    (temp, session_file)
}

#[test]
fn cache_sync_ingests_reskips_and_updates() {
    let (home, session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let sync = || {
        let mut c = cmd();
        c.env("HOME", home.path())
            .env("TOOLPATH_CONFIG_DIR", cfg.path())
            .args(["p", "cache", "sync", "claude"]);
        c
    };

    // First run derives the session into the cache and records it.
    sync()
        .assert()
        .success()
        .stderr(predicate::str::contains("1 new, 0 updated, 0 unchanged"));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("manifest.json")).unwrap())
            .unwrap();
    let record = &manifest["claude"]["session-abc"];
    let cache_id = record["cache_id"].as_str().unwrap();
    assert!(
        cfg.path()
            .join(format!("documents/{cache_id}.json"))
            .exists()
    );

    // Nothing changed: the second run derives nothing.
    sync()
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 1 unchanged"));

    // The session grows a turn; the third run re-derives it.
    let mut body = std::fs::read_to_string(&session_file).unwrap();
    body.push_str(
        r#"{"type":"user","uuid":"u-2","timestamp":"2024-01-01T00:05:00Z","cwd":"/x","message":{"role":"user","content":"more"}}"#,
    );
    body.push('\n');
    std::fs::write(&session_file, body).unwrap();
    sync()
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 1 updated, 0 unchanged"));
}

#[test]
fn cache_sync_default_run_with_no_sessions_reports_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("HOME", home.path())
        // opencode resolves through $XDG_DATA_HOME before $HOME — drop it
        // so the sandboxed run can't see the developer's real database.
        .env_remove("XDG_DATA_HOME")
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync"])
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to sync"));
    assert!(!cfg.path().join("manifest.json").exists());
}

#[test]
fn import_records_manifest_so_sync_skips() {
    let (home, _session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "p",
            "import",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .assert()
        .success();

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("manifest.json")).unwrap())
            .unwrap();
    let record = &manifest["claude"]["session-abc"];
    assert!(record["modified"].is_string(), "import must stamp mtime");
    assert!(record["size"].is_u64(), "import must stamp size");

    // Sync sees the import's record and derives nothing.
    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "claude"])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 1 unchanged"));
}

#[test]
fn bulk_import_records_every_session() {
    let (home, session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");
    // A second session alongside the fixture's, so --all has a batch.
    std::fs::write(
        session_file.parent().unwrap().join("deadbeef-second.jsonl"),
        format!(
            r#"{{"type":"user","uuid":"u-9","timestamp":"2024-02-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"second"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "claude", "--all", "--project"])
        .arg(&project)
        .assert()
        .success();

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["claude"].as_object().unwrap().len(),
        2,
        "--all must record every session it writes"
    );

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "claude"])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 2 unchanged"));
}

#[test]
fn git_import_lands_in_manifest_and_sync_leaves_it_alone() {
    let (dir, branch) = git_fixture();
    let cfg = tempfile::tempdir().unwrap();

    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir.path())
        .assert()
        .success();

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("manifest.json")).unwrap())
            .unwrap();
    let git_records = manifest["git"].as_object().unwrap();
    assert_eq!(git_records.len(), 1);
    let record = git_records.values().next().unwrap();
    assert!(
        record["path"].is_string(),
        "git records carry the repo path"
    );

    // Git artifacts are recorded, not discovered: sync reports zeros
    // and must not fail or re-derive.
    let home = tempfile::tempdir().unwrap();
    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "git"])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 0 unchanged"));
}

#[test]
fn share_records_manifest_so_sync_skips() {
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
    server.join().unwrap();

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.path().join("manifest.json")).unwrap())
            .unwrap();
    assert!(manifest["claude"]["session-abc"]["modified"].is_string());

    cmd()
        .env("HOME", &home)
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "claude"])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 1 unchanged"));
}

#[test]
fn cache_sync_project_under_limits_ingestion() {
    let (home, _session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "p",
            "cache",
            "sync",
            "claude",
            "--project-under",
            "/nowhere",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 new, 0 updated, 0 unchanged"));

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "claude", "--project-under"])
        .arg(&project)
        .assert()
        .success()
        .stderr(predicate::str::contains("1 new, 0 updated, 0 unchanged"));
}

#[test]
fn share_uploads_cached_doc_when_source_unchanged() {
    let (home, session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");
    let share = |port: u16| {
        let mut c = cmd();
        c.env("HOME", home.path())
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
            .arg(format!("http://127.0.0.1:{port}"));
        c
    };

    // First share derives + records.
    let (port, server) = one_shot_anon_server();
    share(port)
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading without re-deriving").not());
    server.join().unwrap();

    // Unchanged source: the second share must not re-derive.
    let (port, server) = one_shot_anon_server();
    share(port)
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading without re-deriving"));
    server.join().unwrap();

    // The session grows: the fast path steps aside and share re-derives.
    let mut body = std::fs::read_to_string(&session_file).unwrap();
    body.push_str(
        r#"{"type":"user","uuid":"u-2","timestamp":"2024-01-01T00:05:00Z","cwd":"/x","message":{"role":"user","content":"more"}}"#,
    );
    body.push('\n');
    std::fs::write(&session_file, body).unwrap();
    let (port, server) = one_shot_anon_server();
    share(port)
        .assert()
        .success()
        .stderr(predicate::str::contains("uploading without re-deriving").not())
        .stderr(predicate::str::contains("Cached claude session"));
    server.join().unwrap();
}

#[test]
fn import_ingests_thinking_maximally() {
    let (home, session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");
    // Append an assistant turn with a thinking block.
    let mut body = std::fs::read_to_string(&session_file).unwrap();
    body.push_str(
        r#"{"type":"assistant","uuid":"a-2","timestamp":"2024-01-01T00:00:02Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret reasoning"},{"type":"text","text":"done"}]}}"#,
    );
    body.push('\n');
    std::fs::write(&session_file, body).unwrap();

    let out = cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "p",
            "import",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(out.status.success());
    let doc_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let doc = std::fs::read_to_string(&doc_path).unwrap();
    assert!(
        doc.contains("secret reasoning"),
        "the cache holds the maximal derivation, thinking included"
    );
}

#[test]
fn import_after_query_sync_is_a_noop_not_an_error() {
    let (home, _session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");

    // A bare query auto-syncs the session into the cache…
    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["query", "--source", "claude", "length"])
        .assert()
        .success();

    // …and the documented explicit import must not die on the exists-check.
    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args([
            "p",
            "import",
            "claude",
            "--session",
            "session-abc",
            "--project",
        ])
        .arg(&project)
        .assert()
        .success()
        .stderr(predicate::str::contains("already up to date"));
}

#[cfg(unix)]
#[test]
fn bulk_import_skips_unreadable_sessions() {
    use std::os::unix::fs::PermissionsExt;
    let (home, session_file) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let project = home.path().join("proj");
    // A second session that cannot be read.
    let bad = session_file.parent().unwrap().join("deadbeef-bad.jsonl");
    std::fs::write(&bad, "x").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();

    cmd()
        .env("HOME", home.path())
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "claude", "--all", "--project"])
        .arg(&project)
        .assert()
        .success()
        .stderr(predicate::str::contains("Warning: skipping session"))
        .stderr(predicate::str::contains("Imported"));
}

#[test]
fn cache_sync_rejects_unknown_type() {
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "sync", "frobnicate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
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
            "p",
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
    // isn't an `<owner>/<repo>/<uuid>` triple should fail at parse time
    // with a clear message rather than blowing up downstream.
    let cfg = tempfile::tempdir().unwrap();
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "pathbase", "trc_nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<owner>/<repo>/<uuid>"));
}

#[test]
fn import_git_no_cache_honors_global_pretty() {
    let (dir, branch) = git_fixture();

    let output = cmd()
        .arg("--pretty")
        .args(["p", "import"])
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
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir_a.path())
        .assert()
        .success();

    // Second import from a different repo on the same branch must NOT
    // trigger the "cache entry already exists" collision.
    cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "import", "git", "--branch"])
        .arg(&branch)
        .arg("--repo")
        .arg(dir_b.path())
        .assert()
        .success();

    let ls = cmd()
        .env("TOOLPATH_CONFIG_DIR", cfg.path())
        .args(["p", "cache", "ls"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(ls.stdout).unwrap();
    let git_entries = stdout.lines().filter(|l| l.starts_with("git-")).count();
    assert_eq!(
        git_entries, 2,
        "expected two distinct git- cache entries, got:\n{stdout}"
    );
}

// ── `path p derive` regression guard ────────────────────────────────

#[test]
fn p_derive_is_first_class_and_warns_no_one() {
    // `path p derive` is the canonical home of the stdout-JSON derive
    // surface — it must produce the document on stdout AND keep stderr
    // clean. Regression guard: if we ever start printing a deprecation
    // notice through this path, this test breaks loudly.
    let (dir, branch) = git_fixture();
    cmd()
        .args(["p", "derive", "git", "--repo"])
        .arg(dir.path())
        .arg("--branch")
        .arg(&branch)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"graph\":"))
        .stderr(predicate::str::is_empty());
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
        let body = r#"{"id":"fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537","repo_id":"00000000-0000-0000-0000-000000000002","toolpath_id":"tp-1","document":{"graph":{"id":"g"},"paths":[]},"path_count":0,"url":"https://example.test/anon/abc-123","visibility":"unlisted","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#;
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
        let body = r#"{"id":"fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537","repo_id":"00000000-0000-0000-0000-000000000002","toolpath_id":"tp-1","document":{"graph":{"id":"g"},"paths":[]},"path_count":0,"url":"https://example.test/anon/abc","visibility":"unlisted","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#;
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
        let body = r#"{"id":"fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537","repo_id":"00000000-0000-0000-0000-000000000002","toolpath_id":"tp-1","document":{"graph":{"id":"g"},"paths":[]},"path_count":0,"url":"https://example.test/anon/abc","visibility":"unlisted","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#;
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
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-grow",
            "--project",
        ])
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
    assert_eq!(
        cache_files.len(),
        1,
        "expected one cache entry after first share"
    );
    let cache_path = cache_files[0].path();
    let cache_v1 = std::fs::read_to_string(&cache_path).unwrap();
    assert!(
        cache_v1.contains("reply-1"),
        "v1 cache must contain reply-1"
    );
    assert!(
        !cache_v1.contains("reply-2"),
        "v1 cache must not contain reply-2 yet"
    );

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
        .args([
            "share",
            "--harness",
            "claude",
            "--session",
            "session-grow",
            "--project",
        ])
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

// ── share: configured repo mappings (`~/.toolpath/config.toml`, `.toolpath.toml`) ──

/// Build the claude session fixture used by the configured-repo share
/// tests: a project dir plus a matching `~/.claude/projects/<slug>`
/// session. Returns (fixture-temp, project-path).
fn claude_session_fixture() -> (tempfile::TempDir, PathBuf) {
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
    (temp, project)
}

/// A configured repo requires an authed upload: hitting a `[[project]]`
/// rule while not logged in must error with a login hint instead of
/// silently falling through to the anonymous endpoint.
#[test]
fn share_configured_repo_requires_login() {
    let (temp, project) = claude_session_fixture();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        format!(
            "[[project]]\ndir = {:?}\nrepo = \"team/sessions\"\n",
            project.display().to_string()
        ),
    )
    .unwrap();

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
        .args(["--no-cache", "--url", "http://127.0.0.1:1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("team/sessions"))
        .stderr(predicate::str::contains("path auth login"));
}

/// A repo-tracked `.toolpath.toml` at the project root feeds the same
/// mapping (and the error provenance names the file).
#[test]
fn share_tracked_toolpath_toml_configures_repo() {
    let (temp, project) = claude_session_fixture();
    std::fs::write(
        project.join(".toolpath.toml"),
        "[share]\nrepo = \"team/sessions\"\n",
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
        .args(["--no-cache", "--url", "http://127.0.0.1:1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("team/sessions"))
        .stderr(predicate::str::contains(".toolpath.toml"))
        .stderr(predicate::str::contains("path auth login"));
}

/// Explicit `--anon` opts out of the configured mapping: the upload goes
/// to the anonymous endpoint with no "Sharing to" line.
#[test]
fn share_anon_flag_ignores_configured_repo() {
    let (port, server, temp, project, home) = share_anon_fixture();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        format!(
            "[[project]]\ndir = {:?}\nrepo = \"team/sessions\"\n",
            project.display().to_string()
        ),
    )
    .unwrap();

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
        .success()
        .stderr(predicate::str::contains("Sharing to").not());

    server.join().unwrap();
    drop(temp);
}

/// Mock Pathbase for the authed configured-repo test: answers the
/// `GET /api/v1/u/me` credentials probe and then a graph POST, capturing
/// each request's start line for assertions.
fn authed_upload_server() -> (u16, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut starts = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut start = String::new();
            reader.read_line(&mut start).unwrap();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).ok();
            }
            // Progenitor strictly validates response shapes, so both
            // bodies carry every required field.
            let (status, body) = if start.starts_with("GET") {
                (
                    "200 OK",
                    r#"{"id":"fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537","username":"alex","email":null,"display_name":null,"bio":null,"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#,
                )
            } else {
                (
                    "201 Created",
                    r#"{"id":"fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537","repo_id":"00000000-0000-0000-0000-000000000002","toolpath_id":"tp-1","document":{"graph":{"id":"g"},"paths":[]},"path_count":0,"url":"https://example.test/u/team/repos/sessions/graphs/fe94b6f9","visibility":"unlisted","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z"}"#,
                )
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
                body.len()
            );
            let mut stream = reader.into_inner();
            let _ = stream.write_all(resp.as_bytes());
            starts.push(start.trim_end().to_string());
        }
        starts
    });
    (port, handle)
}

/// Logged in with a `[[project]]` rule covering the session's project:
/// the upload must go to the configured repo's graphs endpoint, with the
/// provenance line on stderr and the share URL on stdout.
#[test]
fn share_configured_repo_uploads_when_authed() {
    let (port, server) = authed_upload_server();
    let (temp, project) = claude_session_fixture();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        format!(
            "[[project]]\ndir = {:?}\nrepo = \"team/sessions\"\n",
            project.display().to_string()
        ),
    )
    .unwrap();
    std::fs::write(
        cfg.path().join("credentials.json"),
        format!(
            r#"{{"url":"http://127.0.0.1:{port}","token":"tok","user":{{"id":"u-1","username":"alex"}}}}"#
        ),
    )
    .unwrap();

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
        .args(["--no-cache"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Sharing to team/sessions"))
        .stdout(predicate::str::contains(
            "https://example.test/u/team/repos/sessions/graphs/fe94b6f9",
        ));

    let starts = server.join().unwrap();
    assert!(
        starts[0].starts_with("GET /api/v1/u/me"),
        "first request should be the auth probe: {starts:?}"
    );
    assert!(
        starts[1].starts_with("POST /api/v1/u/team/repos/sessions/graphs"),
        "upload must target the configured repo: {starts:?}"
    );
}
