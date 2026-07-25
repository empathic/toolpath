//! Integration tests for `path resume`.
//!
//! Tests dispatch through `path_cli::cmd_resume::run_with_strategy`
//! with a `RecordingExec` strategy so the would-be `execvp` becomes a
//! captured `(binary, args, cwd)` tuple. Each test isolates `$HOME`,
//! `$TOOLPATH_CONFIG_DIR`, and `$PATH` via RAII guards under a shared
//! lock.

#![cfg(not(target_os = "emscripten"))]

use path_cli::cmd_resume::{
    PersistBackend, RecordingExec, ResumeArgs, Transport, run_with_strategy,
};
use path_cli::harness::Harness;

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
        args_explicit(doc_file, cwd.path(), Harness::Claude),
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
        args_explicit(doc_file, cwd.path(), Harness::Gemini),
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
        args_explicit(doc_file, cwd.path(), Harness::Codex),
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
        args_explicit(doc_file, cwd.path(), Harness::Copilot),
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
        args_explicit(doc_file, cwd.path(), Harness::Opencode),
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
    run_with_strategy(args_explicit(doc_file, cwd.path(), Harness::Pi), &recorder).unwrap();

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
        harness: Some(Harness::Claude),
        no_cache: false,
        force: false,
        url: None,
        remote: None,
        tmux: false,
        persist: None,
        via: Transport::Ssh,
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
        args_explicit(doc_file, cwd.path(), Harness::Claude),
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
        args_explicit(doc_file, cwd.path(), Harness::Claude),
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
        args_explicit(doc_file, cwd.path(), Harness::Claude),
        &recorder,
    )
    .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("isn't on PATH"), "actual: {s}");
    assert!(s.contains("claude"), "actual: {s}");
}

// ── Remote resume over SSH ──────────────────────────────────────────

/// With `--remote <ssh url>`, resume should be dispatched to the remote
/// host over SSH rather than exec'ing a local harness: the session is
/// projected locally and the JSONL shipped into the remote's Claude
/// layout (no `path` on the remote), and the final recorded invocation
/// must be `ssh -t` targeting the remote host and launching
/// `claude -r <id>` directly (id computed host-side from the doc).
#[test]
fn remote_flag_dispatches_resume_over_ssh() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-remote-int");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://dev@example.com:2222/home/dev/project".to_string());

    let recorder = RecordingExec::default();
    run_with_strategy(args, &recorder).unwrap();

    // Ship: the locally-projected JSONL is written over SFTP into the
    // remote's Claude projects layout — typed transport calls, no shell
    // strings and no `path` on the remote.
    let writes = recorder.writes();
    assert_eq!(writes.len(), 1, "exactly one file written");
    let (dest, body) = &writes[0];
    assert!(
        dest.contains(".claude/projects/") && dest.ends_with("resume-remote-int.jsonl"),
        "file should land in the remote Claude layout, got {dest}"
    );
    assert!(
        body.contains("\"sessionId\":\"resume-remote-int\""),
        "written bytes should carry the projected JSONL"
    );

    // Launch: interactive ssh -t running the harness directly.
    let cap = recorder.captured();
    assert_eq!(
        cap.binary, "ssh",
        "remote resume should exec ssh, not the local harness (got {})",
        cap.binary
    );
    assert!(
        cap.args.iter().any(|a| a.contains("example.com")),
        "ssh argv should target the remote host, got {:?}",
        cap.args
    );
    assert!(
        cap.args
            .iter()
            .any(|a| a.contains("claude -r resume-remote-int")),
        "ssh should launch `claude -r <id>` on the remote, got {:?}",
        cap.args
    );
}

/// With `--persist dtach` pinned explicitly (skipping the probe/picker),
/// the launch command must be wrapped in `dtach -A /tmp/path-dtach-<id>`
/// so the remote session survives an SSH disconnect.
#[test]
fn remote_resume_persist_dtach_records_launch_and_ships() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-persist-dtach");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://h".to_string());
    args.persist = Some(PersistBackend::Dtach);

    let rec = RecordingExec::with_available(["dtach"]);
    run_with_strategy(args, &rec).unwrap();

    let cap = rec.captured();
    assert_eq!(cap.binary, "ssh");
    assert!(
        cap.args
            .iter()
            .any(|a| a.contains("dtach -A /tmp/path-dtach-")),
        "{:?}",
        cap.args
    );
}

/// `--via et` (a reserved transport) must fail on the host BEFORE any
/// remote side effect — nothing probed, nothing shipped, nothing
/// launched — so a doomed transport never leaves a half-staged session.
#[test]
fn remote_resume_via_et_errors_before_any_remote_touch() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-via-et");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://h".to_string());
    args.via = Transport::Et;

    let rec = RecordingExec::with_available(["tmux"]);
    let err = run_with_strategy(args, &rec).unwrap_err();

    assert!(
        err.to_string().contains("et is not yet supported"),
        "actual: {err}"
    );
    assert!(rec.homes().is_empty(), "must not probe the remote");
    assert!(rec.writes().is_empty(), "must not ship anything");
    assert!(rec.captured().binary.is_empty(), "must not launch");
}

/// With no `--persist`, `run_remote` probes the remote for available
/// backends and auto-selects the preferred one (tmux over zellij), then
/// wraps the launch in it — the session survives disconnects without the
/// user naming a backend.
#[test]
fn remote_resume_auto_selects_preferred_backend_when_persist_omitted() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-auto-persist");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://h".to_string());
    args.persist = None; // no explicit backend → probe + auto-select

    let rec = RecordingExec::with_available(["zellij", "tmux"]);
    run_with_strategy(args, &rec).unwrap();

    // The session file still ships, and the launch wraps in tmux (preferred).
    assert_eq!(rec.writes().len(), 1, "session file should ship");
    let cap = rec.captured();
    assert_eq!(cap.binary, "ssh");
    assert!(
        cap.args.iter().any(|a| a.contains("tmux new-session")),
        "auto-selected launch should wrap in tmux, got {:?}",
        cap.args
    );
}

/// With no `--persist` and no persistence backend installed on the
/// remote, `run_remote` falls back to a plain launch (`claude -r`) —
/// still ships and launches, just without a detachable wrapper.
#[test]
fn remote_resume_falls_back_to_plain_when_no_backend_available() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-plain-fallback");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://h".to_string());
    args.persist = None;

    let rec = RecordingExec::with_available([]); // nothing installed
    run_with_strategy(args, &rec).unwrap();

    assert_eq!(rec.writes().len(), 1, "session file should still ship");
    let cap = rec.captured();
    assert!(
        cap.args.iter().any(|a| a.contains("claude -r")),
        "should still launch claude, got {:?}",
        cap.args
    );
    assert!(
        !cap.args.iter().any(|a| a.contains("tmux new-session")
            || a.contains("zellij")
            || a.contains("dtach")
            || a.contains("abduco")),
        "no persistence wrapper when none available, got {:?}",
        cap.args
    );
}

/// A `--cwd` through a symlink (e.g. macOS `/tmp` → `/private/tmp`) must
/// key the shipped project dir on the *canonical* path — otherwise
/// `claude -r`, which uses the physical cwd, looks in a dir the session
/// was never shipped to. Regression for the live-verified macOS bug.
#[test]
fn remote_resume_ships_to_canonical_cwd_when_symlinked() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-symlink-cwd");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.remote = Some("ssh://h".to_string());
    args.cwd = Some(std::path::PathBuf::from("/tmp/work")); // logical
    args.persist = Some(PersistBackend::Plain);

    // Remote reports the physical path (symlink resolved).
    let rec = RecordingExec::with_realpath("/private/tmp/work");
    run_with_strategy(args, &rec).unwrap();

    // Session file shipped under the CANONICAL project dir, not the logical one.
    let (dest, _) = &rec.writes()[0];
    assert!(
        dest.contains("/.claude/projects/-private-tmp-work/"),
        "should ship to canonical project dir, got {dest}"
    );
    assert!(
        !dest.contains("/projects/-tmp-work/"),
        "must not ship to the logical (symlinked) dir, got {dest}"
    );
    // And the launch cd's into the canonical path too.
    let cap = rec.captured();
    assert!(
        cap.args.iter().any(|a| a.contains("cd /private/tmp/work")),
        "launch should cd into the canonical cwd, got {:?}",
        cap.args
    );
}

/// `--remote` without `--harness` must fail fast on the host with a
/// clear message: the remote resume runs over a non-interactive SSH
/// session where the harness picker has no TTY, and the host can't run
/// the picker either (it never resolves the doc in v0).
#[test]
fn remote_without_harness_errors_before_dispatch() {
    let _env = env_lock();
    let _home = ScopedHome::new();
    let _path = ScopedPath::with_binaries(&["ssh", "claude"]);
    let cwd = tempfile::tempdir().unwrap();

    let path = make_convo_path("agent:claude-code", "claude-code://resume-remote-nohar");
    let doc_file = write_path_to_temp(cwd.path(), path);

    let mut args = args_explicit(doc_file, cwd.path(), Harness::Claude);
    args.harness = None;
    args.remote = Some("ssh://dev@example.com:2222".to_string());

    let recorder = RecordingExec::default();
    let err = run_with_strategy(args, &recorder).unwrap_err();
    let s = err.to_string();
    assert!(
        s.contains("--harness"),
        "error should mention --harness: {s}"
    );
    assert!(
        recorder.captured().binary.is_empty(),
        "must not dispatch ssh when --harness is missing"
    );
}
