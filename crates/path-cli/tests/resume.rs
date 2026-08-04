//! Integration tests for `path resume`.
//!
//! Tests dispatch through `path_cli::cmd_resume::run_with_strategy`
//! with a `RecordingExec` strategy so the would-be `execvp` becomes a
//! captured `(binary, args, cwd)` tuple. Each test isolates `$HOME`,
//! `$TOOLPATH_CONFIG_DIR`, and `$PATH` via RAII guards under a shared
//! lock.

#![cfg(not(target_os = "emscripten"))]

use path_cli::cmd_resume::{FixedPicker, RecordingExec, ResumeArgs, run_bare, run_with_strategy};
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
        input: Some(cache_id.to_string()),
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(Harness::Claude),
        ..Default::default()
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

// ── Bare mode: cross-harness session picker ─────────────────────────

#[test]
fn bare_resume_picks_session_derives_projects_and_execs() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    write_claude_session(
        &home.home_dir(),
        "-test-project",
        "bare-session-one",
        "Add a feature",
    );

    let args = ResumeArgs {
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(Harness::Claude),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    let picker = FixedPicker::select(0);
    run_bare(&args, &recorder, &picker).unwrap();

    let cap = recorder.captured();
    assert_eq!(cap.binary, "claude");
    assert_eq!(cap.args[0], "-r");
    assert!(!cap.args[1].is_empty(), "session id should be non-empty");
    assert_eq!(cap.cwd, std::fs::canonicalize(cwd.path()).unwrap());

    // The projection wrote a JSONL beyond the fixture's own — i.e. one
    // outside the fixture's project directory.
    let projects = home.home_dir().join(".claude/projects");
    let projected: Vec<_> = files_with_ext(&projects, "jsonl")
        .into_iter()
        .filter(|p| !p.parent().unwrap().ends_with("-test-project"))
        .collect();
    assert!(
        !projected.is_empty(),
        "no projected JSONL written outside the fixture project"
    );
}

#[test]
fn bare_resume_no_sessions_bails_with_status_table() {
    let _env = env_lock();
    let _home = ScopedHome::new();

    let args = ResumeArgs::default();
    let recorder = RecordingExec::default();
    let err = run_bare(&args, &recorder, &FixedPicker::select(0)).unwrap_err();
    assert!(
        err.to_string().contains("no resumable sessions"),
        "actual: {err}"
    );
    assert!(recorder.captured().binary.is_empty(), "must not exec");
}

#[test]
fn bare_resume_from_filters_the_picker() {
    let _env = env_lock();
    let home = ScopedHome::new();

    write_claude_session(&home.home_dir(), "-test-project", "claude-sess-one", "hi");
    write_codex_session(
        &home.home_dir(),
        "00000000-0000-0000-0000-0000000000aa",
        "/work/proj",
    );

    let args = ResumeArgs {
        from: Some(Harness::Codex),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    let picker = FixedPicker::no_match();
    run_bare(&args, &recorder, &picker).unwrap();

    let offered = picker.offered();
    assert!(!offered.is_empty(), "codex fixture should be offered");
    assert!(
        offered.iter().all(|l| l.starts_with("codex\t")),
        "every offered line must be codex: {offered:?}"
    );
    assert!(
        offered.iter().all(|l| !l.starts_with("claude\t")),
        "claude rows must be filtered out: {offered:?}"
    );
}

#[test]
fn bare_resume_from_with_no_matching_sessions_mentions_filter() {
    let _env = env_lock();
    let home = ScopedHome::new();

    // A claude session exists, but the --from filter names codex —
    // the error must name the filter instead of showing the generic
    // all-harness status table.
    write_claude_session(&home.home_dir(), "-test-project", "claude-only-sess", "hi");

    let args = ResumeArgs {
        from: Some(Harness::Codex),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    let err = run_bare(&args, &recorder, &FixedPicker::select(0)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "no codex sessions found; drop --from to see sessions from other harnesses"
    );
    assert!(recorder.captured().binary.is_empty(), "must not exec");
}

#[test]
fn bare_resume_cwd_flag_ranks_matching_sessions_first() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let cwd = tempfile::tempdir().unwrap();
    let canonical_cwd = std::fs::canonicalize(cwd.path()).unwrap();

    // The matching session gets a strictly OLDER embedded timestamp
    // (codex `last_activity` is the max embedded JSONL timestamp, not
    // file mtime). Recency ranking alone would therefore put the
    // NON-matching session first — so `offered()[0]` below passes only
    // if the cwd match genuinely outranks recency. A broken
    // `matches_cwd` (always false) deterministically fails this test.
    write_codex_session_at(
        &home.home_dir(),
        "00000000-0000-0000-0000-0000000000bb",
        canonical_cwd.to_str().unwrap(),
        "2026-05-07T00:00",
    );
    write_codex_session_at(
        &home.home_dir(),
        "00000000-0000-0000-0000-0000000000cc",
        "/somewhere/else",
        "2026-05-07T01:00",
    );

    let args = ResumeArgs {
        cwd: Some(cwd.path().to_path_buf()),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    let picker = FixedPicker::no_match();
    run_bare(&args, &recorder, &picker).unwrap();

    let offered = picker.offered();
    assert_eq!(offered.len(), 2, "both codex fixtures should be offered");
    assert!(
        offered[0].contains("00000000-0000-0000-0000-0000000000bb"),
        "cwd-matching session must rank first: {offered:?}"
    );
}

#[test]
fn bare_resume_writes_cache_and_manifest_by_default() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    write_claude_session(
        &home.home_dir(),
        "-test-project",
        "bare-cache-session",
        "prompt",
    );

    let args = ResumeArgs {
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(Harness::Claude),
        ..Default::default()
    };
    run_bare(&args, &RecordingExec::default(), &FixedPicker::select(0)).unwrap();

    let config_dir = std::path::PathBuf::from(std::env::var_os("TOOLPATH_CONFIG_DIR").unwrap());
    let cached: Vec<_> = files_with_ext(&config_dir.join("documents"), "json")
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("claude-"))
        })
        .collect();
    assert!(!cached.is_empty(), "no claude-*.json written to the cache");

    let manifest = std::fs::read_to_string(config_dir.join("manifest.json"))
        .expect("manifest.json should exist after a bare resume");
    assert!(
        manifest.contains("bare-cache-session"),
        "manifest must record the derived session: {manifest}"
    );
}

#[test]
fn bare_resume_no_cache_skips_cache_write() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");
    let cwd = tempfile::tempdir().unwrap();

    write_claude_session(
        &home.home_dir(),
        "-test-project",
        "bare-nocache-session",
        "prompt",
    );

    let args = ResumeArgs {
        cwd: Some(cwd.path().to_path_buf()),
        harness: Some(Harness::Claude),
        no_cache: true,
        ..Default::default()
    };
    run_bare(&args, &RecordingExec::default(), &FixedPicker::select(0)).unwrap();

    let config_dir = std::path::PathBuf::from(std::env::var_os("TOOLPATH_CONFIG_DIR").unwrap());
    assert!(
        files_with_ext(&config_dir.join("documents"), "json").is_empty(),
        "--no-cache must not write cache docs"
    );
    assert!(
        !config_dir.join("manifest.json").exists(),
        "--no-cache must not record the manifest"
    );
}

#[test]
fn bare_resume_fresh_cache_fast_path_uses_cached_doc() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");

    write_claude_session(
        &home.home_dir(),
        "-test-project",
        "fast-path-session",
        "prompt",
    );

    // Run 1: derives and writes cache + manifest. `--project` pins the
    // picker to the fixture so run 2 can't accidentally pick the
    // session run 1 projects under $HOME/.claude/projects.
    let cwd1 = tempfile::tempdir().unwrap();
    let args1 = ResumeArgs {
        cwd: Some(cwd1.path().to_path_buf()),
        harness: Some(Harness::Claude),
        project: Some(std::path::PathBuf::from("/test/project")),
        ..Default::default()
    };
    run_bare(&args1, &RecordingExec::default(), &FixedPicker::select(0)).unwrap();

    // Overwrite the cached doc with a sentinel graph. fresh_cache_id
    // stats the SOURCE session file (untouched), so the overwritten
    // doc still rides the fast path.
    let config_dir = std::path::PathBuf::from(std::env::var_os("TOOLPATH_CONFIG_DIR").unwrap());
    let cached = files_with_ext(&config_dir.join("documents"), "json")
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("claude-"))
        })
        .expect("run 1 should have cached the derived doc");
    let mut sentinel = make_convo_path("agent:claude-code", "claude-code://sentinel-session");
    sentinel.steps[0]
        .change
        .get_mut("claude-code://sentinel-session")
        .unwrap()
        .structural
        .as_mut()
        .unwrap()
        .extra
        .insert(
            "text".to_string(),
            serde_json::json!("SENTINEL-BARE-RESUME-FAST-PATH"),
        );
    let sentinel_graph = toolpath::v1::Graph::from_path(sentinel);
    std::fs::write(&cached, sentinel_graph.to_json().unwrap()).unwrap();

    // Run 2 into a fresh cwd: the fast path must project the sentinel
    // doc, not a re-derivation of the fixture.
    let cwd2 = tempfile::tempdir().unwrap();
    let args2 = ResumeArgs {
        cwd: Some(cwd2.path().to_path_buf()),
        harness: Some(Harness::Claude),
        project: Some(std::path::PathBuf::from("/test/project")),
        ..Default::default()
    };
    run_bare(&args2, &RecordingExec::default(), &FixedPicker::select(0)).unwrap();

    let projects = home.home_dir().join(".claude/projects");
    let has_sentinel = files_with_ext(&projects, "jsonl").iter().any(|p| {
        std::fs::read_to_string(p)
            .map(|s| s.contains("SENTINEL-BARE-RESUME-FAST-PATH"))
            .unwrap_or(false)
    });
    assert!(
        has_sentinel,
        "run 2 must project the cached sentinel doc via the freshness fast path"
    );
}

#[test]
fn bare_resume_no_match_returns_ok_without_exec() {
    let _env = env_lock();
    let home = ScopedHome::new();

    write_claude_session(&home.home_dir(), "-test-project", "nomatch-session", "hi");

    let args = ResumeArgs::default();
    let recorder = RecordingExec::default();
    run_bare(&args, &recorder, &FixedPicker::no_match()).unwrap();
    assert!(
        recorder.captured().binary.is_empty(),
        "NoMatch must not exec anything"
    );
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

// ── Recency-first picker (top-N hydration + tail row) ───────────────

#[test]
fn bare_resume_recent_view_mixes_all_harnesses_in_one_call() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");

    write_claude_session(&home.home_dir(), "-mix-proj", "mix-claude", "claude prompt");
    write_codex_session(
        &home.home_dir(),
        "00000000-0000-0000-0000-0000000000aa",
        "/mix/proj",
    );

    let args = ResumeArgs {
        harness: Some(Harness::Claude),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    let picker = FixedPicker::select(0);
    run_bare(&args, &recorder, &picker).unwrap();

    let calls = picker.offered_calls();
    assert_eq!(
        calls.len(),
        1,
        "one recency round, no second sweep: {calls:?}"
    );
    let view = &calls[0];
    assert!(
        view.iter().any(|l| l.starts_with("claude\t")),
        "recency view must include the claude session: {view:?}"
    );
    assert!(
        view.iter().any(|l| l.starts_with("codex\t")),
        "recency view must include the codex session: {view:?}"
    );
    assert!(
        view.iter().all(|l| !l.contains("older sessions")),
        "small history must not offer a tail row: {view:?}"
    );
    assert_eq!(recorder.captured().binary, "claude");
}

#[test]
fn bare_resume_tail_row_loads_everything() {
    let _env = env_lock();
    let home = ScopedHome::new();
    let _path = ScopedPath::with_binary("claude");

    // 101 codex sessions: one more than RECENT_LIMIT, so the recency
    // view shows 100 rows plus a "1 older sessions" tail row.
    for i in 0..101u32 {
        let id = format!("00000000-0000-0000-0000-0000000{:05}", i);
        let stamp = format!("2026-05-07T{:02}:{:02}", i / 60, i % 60);
        write_codex_session_at(&home.home_dir(), &id, "/tail/proj", &stamp);
    }

    let args = ResumeArgs {
        harness: Some(Harness::Claude),
        ..Default::default()
    };
    let recorder = RecordingExec::default();
    // Round 1: 100 rows + tail at index 100 -> pick the tail.
    // Round 2: the full sweep -> pick the top row.
    let picker = FixedPicker::sequence(vec![
        path_cli::cmd_resume::PickChoice::Index(100),
        path_cli::cmd_resume::PickChoice::Index(0),
    ]);
    run_bare(&args, &recorder, &picker).unwrap();

    let calls = picker.offered_calls();
    assert_eq!(
        calls.len(),
        2,
        "tail row must trigger the full sweep: {calls:?}"
    );
    assert_eq!(calls[0].len(), 101, "100 rows + tail row");
    let tail = calls[0].last().unwrap();
    assert!(
        tail.contains("1 older sessions"),
        "tail row must count the unhydrated sessions: {tail}"
    );
    assert_eq!(calls[1].len(), 101, "full sweep hydrates all 101 sessions");
    assert!(
        calls[1].iter().all(|l| !l.contains("older sessions")),
        "full sweep offers no tail row"
    );
    assert_eq!(recorder.captured().binary, "claude");
}
