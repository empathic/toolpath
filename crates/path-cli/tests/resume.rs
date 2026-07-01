//! Integration tests for `path resume`.
//!
//! Tests dispatch through `path_cli::cmd_resume::run_with_strategy`
//! with a `RecordingExec` strategy so the would-be `execvp` becomes a
//! captured `(binary, args, cwd)` tuple. Each test isolates `$HOME`,
//! `$TOOLPATH_CONFIG_DIR`, and `$PATH` via RAII guards under a shared
//! lock.

#![cfg(not(target_os = "emscripten"))]

use path_cli::cmd_resume::{HarnessArg, RecordingExec, ResumeArgs, run_with_strategy};

mod support;
use support::*;

// ── Per-harness positive cases ──────────────────────────────────────

#[test]
fn file_input_explicit_claude_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-claude-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Claude),
        &recorder,
    )
    .unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");
    assert!(!cap.args[1].is_empty(), "session id should be non-empty");
    assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());

    // Side effect: a JSONL was written under HOME/.claude/projects.
    let projects = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".claude/projects"))
        .unwrap();
    assert!(projects.exists(), "claude projects dir not created");
    assert!(
        dir_contains_file_with_ext(&projects, "jsonl"),
        "no JSONL written under claude projects"
    );
}

#[test]
fn file_input_explicit_gemini_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("gemini");
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:gemini-cli", "gemini-cli://resume-gemini-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Gemini),
        &recorder,
    )
    .unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "gemini");
    assert_eq!(cap.args[0], "--resume");
    assert!(!cap.args[1].is_empty());

    let tmp_root = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".gemini/tmp"))
        .unwrap();
    assert!(tmp_root.exists(), "gemini tmp dir not created");
}

#[test]
fn file_input_explicit_codex_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("codex");
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:codex", "codex://resume-codex-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Codex),
        &recorder,
    )
    .unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "codex");
    assert_eq!(cap.args[0], "resume");
    assert!(!cap.args[1].is_empty());

    let sessions = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".codex/sessions"))
        .unwrap();
    assert!(sessions.exists(), "codex sessions dir not created");
}

#[test]
fn file_input_explicit_copilot_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("copilot");
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:copilot", "copilot://resume-copilot-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Copilot),
        &recorder,
    )
    .unwrap();

    // Resume argv is `copilot --resume <fresh-id>`.
    let cap = recorder.captured();
    assert_eq!(cap.binary, "copilot");
    assert_eq!(cap.args[0], "--resume");
    assert!(!cap.args[1].is_empty());

    // A session-state/<id>/events.jsonl was projected under the temp ~/.copilot.
    let state = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".copilot/session-state"))
        .unwrap();
    assert!(state.exists(), "copilot session-state dir not created");
    let has_events = std::fs::read_dir(&state)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.path().join("events.jsonl").is_file());
    assert!(has_events, "no session-state/<id>/events.jsonl written");
}

#[test]
fn file_input_explicit_opencode_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("opencode");
    let cwd = tempfile::tempdir().unwrap();

    // Pre-create the opencode db with the canonical schema. (Schema DDL
    // copied from cmd_export's existing opencode test until/unless
    // toolpath-opencode exposes a public bootstrap helper.)
    let resolver = toolpath_opencode::PathResolver::new();
    let db_path = resolver.db_path().unwrap();
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text NOT NULL,
              name text, time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            "#,
        )
        .unwrap();
    }

    let path = make_convo_path("agent:opencode", "opencode://ses_resume-opencode-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Opencode),
        &recorder,
    )
    .unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "opencode");
    assert_eq!(cap.args[0], "--session");
    assert!(!cap.args[1].is_empty());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
        .unwrap();
    assert_eq!(session_count, 1, "opencode session row not inserted");
}

#[test]
fn file_input_explicit_pi_projects_and_records_exec() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("pi");
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:pi", "pi://resume-pi-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Pi),
        &recorder,
    )
    .unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "pi");
    assert_eq!(cap.args[0], "--session");
    assert!(!cap.args[1].is_empty());

    let sessions = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".pi/agent/sessions"))
        .unwrap();
    assert!(sessions.exists(), "pi sessions dir not created");
}

// ── Cache-id input ──────────────────────────────────────────────────

#[test]
fn cache_id_input_loads_and_projects() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    // Seed a cache entry by writing the graph to
    // <TOOLPATH_CONFIG_DIR>/documents/<id>.json directly.
    let cache_id = "claude-resume-cache-test";
    let documents = std::path::PathBuf::from(std::env::var_os("TOOLPATH_CONFIG_DIR").unwrap())
        .join("documents");
    std::fs::create_dir_all(&documents).unwrap();
    let graph = toolpath::v1::Graph::from_path(make_convo_path(
        "agent:claude-code",
        "claude-code://resume-cache-int",
    ));
    std::fs::write(
        documents.join(format!("{cache_id}.json")),
        graph.to_json().unwrap(),
    )
    .unwrap();

    let resume_args = ResumeArgs {
        input: cache_id.to_string(),
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(HarnessArg::Claude),
        no_cache: false,
        force: false,
        url: None,
    };

    let recorder = RecordingExec::default();
    run_with_strategy(resume_args, &recorder).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");
}

// ── Rejection cases ─────────────────────────────────────────────────

#[test]
fn multi_path_graph_returns_clear_error() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    let p1 = make_convo_path("agent:claude-code", "claude-code://multi-1");
    let mut p2 = make_convo_path("agent:claude-code", "claude-code://multi-2");
    p2.path.id = "p2".into();

    let graph = toolpath::v1::Graph {
        graph: toolpath::v1::GraphIdentity { id: "g1".into() },
        paths: vec![
            toolpath::v1::PathOrRef::Path(Box::new(p1)),
            toolpath::v1::PathOrRef::Path(Box::new(p2)),
        ],
        meta: None,
    };
    let doc_file = cwd.path().join("multi.json");
    std::fs::write(&doc_file, graph.to_json().unwrap()).unwrap();

    let recorder = RecordingExec::default();
    let err = run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Claude),
        &recorder,
    )
    .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("single `Path`"), "actual: {s}");
    assert!(s.contains("2 paths"), "actual: {s}");
}

#[test]
fn agentless_path_returns_clear_error() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    // human:* actor — should be rejected by ensure_path_with_agent.
    let path = make_convo_path("human:alex", "claude-code://noop");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    let err = run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Claude),
        &recorder,
    )
    .unwrap_err();
    assert!(err.to_string().contains("no agent session"));
}

#[test]
fn explicit_harness_not_on_path_errors() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::empty();
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://no-binary");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let recorder = RecordingExec::default();
    let err = run_with_strategy(
        args_explicit(doc_file, cwd.path(), HarnessArg::Claude),
        &recorder,
    )
    .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("isn't on PATH"), "actual: {s}");
    assert!(s.contains("claude"), "actual: {s}");
}
