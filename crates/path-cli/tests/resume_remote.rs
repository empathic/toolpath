//! Integration tests for `path resume --remote`.
//!
//! Tests dispatch through `path_cli::cmd_resume::remote::run_remote` with a
//! fake `ssh` shim on the injected search path (see
//! [`support::SshShim`]) and a `RecordingExec` for the attach step. The
//! shim records argv and stdin per invocation and replies with scripted
//! stdout, so every remote interaction is asserted without a network.

#![cfg(not(target_os = "emscripten"))]
#![cfg(unix)]

use path_cli::cmd_resume::RecordingExec;
use path_cli::cmd_resume::remote::{mint_remote_id, run_remote, tmux_session_name};
use path_cli::harness::Harness;

mod support;
use support::*;

/// A tempdir standing in for the local home. `run_remote` takes the
/// home and the local cwd as parameters, so no environment scoping is
/// needed.
struct TestHome {
    dir: tempfile::TempDir,
}

impl TestHome {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn home_dir(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }
}

const REMOTE: &str = "exedev@testhost";
const REMOTE_HOME: &str = "/home/exedev";
const REMOTE_CLAUDE: &str = "/usr/local/bin/claude";
const PROJECT: &str = "/data/proj";

/// The five preflight fact lines a healthy remote reports.
fn preflight_ok(pwd: &str, session: &str) -> String {
    format!(
        "TP_HOME={REMOTE_HOME}\nTP_CLAUDE={REMOTE_CLAUDE}\nTP_TMUX=ok\nTP_PWD={pwd}\nTP_SESSION={session}\n"
    )
}

/// A doc file plus the sandbox it lives in.
fn claude_doc(dir: &std::path::Path) -> std::path::PathBuf {
    let path = make_convo_path("agent:claude-code", "claude-code://remote-int-session");
    write_path_to_temp(dir, path)
}

/// The JSONL `run_remote` ships: the projected conversation with the
/// minted id and the remote project directory applied.
fn expected_shipped_jsonl(doc_file: &std::path::Path, remote_id: &str, project: &str) -> String {
    use toolpath_convo::ConversationProjector;
    let json = std::fs::read_to_string(doc_file).unwrap();
    let graph = toolpath::v1::Graph::from_json(&json).unwrap();
    let path = graph.single_path().unwrap();
    let view = toolpath_convo::extract_conversation(path);
    let mut conv = toolpath_claude::ClaudeProjector.project(&view).unwrap();
    conv.set_session_id_and_cwd(remote_id, project);
    let mut lines: Vec<String> = conv
        .preamble
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    lines.extend(
        conv.entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap()),
    );
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn source_session_tmux_name() -> String {
    tmux_session_name("remote-int-session")
}

// ── Mint stability across input serializations ──────────────────────

#[test]
fn same_document_mints_the_same_id_across_serializations() {
    let docs = tempfile::tempdir().unwrap();
    let compact_file = claude_doc(docs.path());
    let compact = std::fs::read_to_string(&compact_file).unwrap();
    let value: serde_json::Value = serde_json::from_str(&compact).unwrap();
    let pretty_file = docs.path().join("pretty.json");
    std::fs::write(&pretty_file, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let mut ship_commands = Vec::new();
    for doc in [&compact_file, &pretty_file] {
        let home = TestHome::new();
        let shim = SshShim::new();
        shim.respond(0, &preflight_ok(PROJECT, "none"));
        let recorder = RecordingExec::default();
        run_remote(
            &args_remote(doc.to_str().unwrap(), REMOTE, Some(PROJECT), false),
            &recorder,
            Some(&home.home_dir()),
            &home.home_dir(),
            &shim.search_path(),
            true,
        )
        .unwrap();
        ship_commands.push(shim.argv(1)[1].clone());
    }
    assert_eq!(
        ship_commands[0], ship_commands[1],
        "the minted id must depend on the parsed document, not the input bytes"
    );
}

// ── Fresh push: preflight, ship, launch, attach ─────────────────────

#[test]
fn fresh_push_ships_launches_and_attaches() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();
    shim.respond(0, &preflight_ok(PROJECT, "none"));

    let recorder = RecordingExec::default();
    run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false),
        &recorder,
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap();

    assert_eq!(shim.calls(), 3, "preflight, ship, launch");

    // Preflight: one batched call, remote first, script second.
    let preflight = shim.argv(0);
    assert_eq!(preflight[0], REMOTE);
    assert_eq!(preflight.len(), 2);
    assert!(preflight[1].contains("pwd -P"));
    assert!(preflight[1].contains("has-session"));

    // Ship: the exact command, and byte-identical JSONL on stdin.
    let raw_json = std::fs::read_to_string(&doc_file).unwrap();
    let graph = toolpath::v1::Graph::from_json(&raw_json).unwrap();
    let canonical = serde_json::to_string(&serde_json::to_value(&graph).unwrap()).unwrap();
    let remote_id = mint_remote_id(&canonical);
    let resolver = toolpath_claude::PathResolver::new().with_home(REMOTE_HOME);
    let slug_dir = resolver.project_dir(PROJECT).unwrap();
    let target = resolver.conversation_file(PROJECT, &remote_id).unwrap();
    let ship = shim.argv(1);
    assert_eq!(ship[0], REMOTE);
    assert_eq!(
        ship[1],
        format!(
            "umask 077; mkdir -p '{}' && cat > '{}'",
            slug_dir.display(),
            target.display()
        )
    );
    let expected = expected_shipped_jsonl(&doc_file, &remote_id, PROJECT);
    assert_eq!(
        String::from_utf8(shim.stdin_bytes(1)).unwrap(),
        expected,
        "shipped JSONL must be byte-identical to the rewritten projection"
    );

    // Launch: detached tmux session running claude on the minted id.
    let launch = shim.argv(2);
    assert_eq!(launch[0], REMOTE);
    let name = source_session_tmux_name();
    assert_eq!(
        launch[1],
        format!(
            "tmux new-session -d -s '{name}' -c '{PROJECT}' \
             'env LANG=C.UTF-8 '\\''{REMOTE_CLAUDE}'\\'' -r '\\''{remote_id}'\\'''"
        )
    );

    // Attach: exec'd with a pty through the strategy.
    let cap = recorder.captured();
    assert_eq!(cap.binary, shim.ssh_path().to_string_lossy());
    assert_eq!(
        cap.args,
        vec![
            "-t".to_string(),
            REMOTE.to_string(),
            format!("tmux attach-session -d -t '={name}'"),
        ]
    );
}

// ── Reattach shortcut ───────────────────────────────────────────────

#[test]
fn live_session_reattaches_without_ship_or_launch() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();
    shim.respond(0, &preflight_ok(PROJECT, "live"));

    let recorder = RecordingExec::default();
    run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false),
        &recorder,
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap();

    assert_eq!(shim.calls(), 1, "preflight only; no ship, no launch");
    let cap = recorder.captured();
    assert_eq!(cap.binary, shim.ssh_path().to_string_lossy());
    assert!(cap.args[2].contains("attach-session"));
}

// ── Dry run ─────────────────────────────────────────────────────────

#[test]
fn dry_run_runs_preflight_and_nothing_else() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();
    shim.respond(0, &preflight_ok(PROJECT, "none"));

    let recorder = RecordingExec::default();
    run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), true),
        &recorder,
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap();

    assert_eq!(shim.calls(), 1, "preflight only");
    assert!(
        recorder.captured().binary.is_empty(),
        "dry run must not attach"
    );
}

// ── Preflight failures ──────────────────────────────────────────────

fn run_expecting_err(shim: &SshShim, cwd: Option<&str>) -> anyhow::Error {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let recorder = RecordingExec::default();
    run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, cwd, false),
        &recorder,
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err()
}

#[test]
fn preflight_missing_claude_errors_with_probe_list() {
    let shim = SshShim::new();
    shim.respond(
        0,
        &format!(
            "TP_HOME={REMOTE_HOME}\nTP_CLAUDE=\nTP_TMUX=ok\nTP_PWD={PROJECT}\nTP_SESSION=none\n"
        ),
    );
    let err = run_expecting_err(&shim, Some(PROJECT));
    let s = err.to_string();
    assert!(s.contains("claude not found"), "actual: {s}");
    assert!(s.contains(".npm-global"), "actual: {s}");
    assert_eq!(shim.calls(), 1);
}

#[test]
fn preflight_missing_tmux_errors() {
    let shim = SshShim::new();
    shim.respond(
        0,
        &format!(
            "TP_HOME={REMOTE_HOME}\nTP_CLAUDE={REMOTE_CLAUDE}\nTP_TMUX=missing\nTP_PWD={PROJECT}\nTP_SESSION=none\n"
        ),
    );
    let err = run_expecting_err(&shim, Some(PROJECT));
    assert!(err.to_string().contains("tmux not found"), "actual: {err}");
    assert_eq!(shim.calls(), 1);
}

#[test]
fn preflight_missing_project_dir_errors() {
    let shim = SshShim::new();
    shim.respond(0, &preflight_ok("", "none"));
    let err = run_expecting_err(&shim, Some(PROJECT));
    let s = err.to_string();
    assert!(s.contains("does not exist"), "actual: {s}");
    assert!(s.contains(PROJECT), "actual: {s}");
    assert_eq!(shim.calls(), 1);
}

#[test]
fn preflight_symlinked_project_dir_names_the_physical_path() {
    let shim = SshShim::new();
    shim.respond(0, &preflight_ok("/data/real-proj", "none"));
    let err = run_expecting_err(&shim, Some("/data/proj"));
    let s = err.to_string();
    assert!(s.contains("physical"), "actual: {s}");
    assert!(s.contains("-C /data/real-proj"), "actual: {s}");
    assert_eq!(shim.calls(), 1, "veto only; no ship after the mismatch");
}

#[test]
fn preflight_banner_output_is_rejected_verbatim() {
    let banner = "Please complete registration with `ssh exe.dev` first.";
    let shim = SshShim::new();
    shim.respond(0, banner);
    let err = run_expecting_err(&shim, Some(PROJECT));
    let s = format!("{err:#}");
    assert!(s.contains("banner"), "actual: {s}");
    assert!(s.contains(banner), "verbatim output missing: {s}");
    assert_eq!(shim.calls(), 1, "nothing shipped after a banner reply");
}

#[test]
fn unreachable_remote_errors_with_ssh_status() {
    let shim = SshShim::new();
    shim.exit_with(0, 255);
    let err = run_expecting_err(&shim, Some(PROJECT));
    let s = err.to_string();
    assert!(s.contains("ssh to"), "actual: {s}");
    assert_eq!(shim.calls(), 1);
}

// ── Early argument errors: zero remote touches ──────────────────────

#[test]
fn non_tty_stdin_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();

    let recorder = RecordingExec::default();
    let err = run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false),
        &recorder,
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        false,
    )
    .unwrap_err();

    assert!(err.to_string().contains("TTY"), "actual: {err}");
    assert_eq!(shim.calls(), 0, "no remote touches on a non-TTY error");
}

#[test]
fn option_shaped_remote_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();

    let err = run_remote(
        &args_remote(
            doc_file.to_str().unwrap(),
            "-oProxyCommand=evil",
            Some(PROJECT),
            false,
        ),
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(err.to_string().contains("ssh destination"), "actual: {err}");
    assert_eq!(shim.calls(), 0);
}

#[test]
fn non_claude_harness_flag_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();

    let mut args = args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false);
    args.harness = Some(Harness::Codex);
    let err = run_remote(
        &args,
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("supports claude only"),
        "actual: {err}"
    );
    assert_eq!(shim.calls(), 0);
}

#[test]
fn non_claude_source_document_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let path = make_convo_path("agent:gemini-cli", "gemini-cli://remote-int-gemini");
    let doc_file = write_path_to_temp(docs.path(), path);
    let shim = SshShim::new();

    let err = run_remote(
        &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false),
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("supports claude only"),
        "actual: {err}"
    );
    assert_eq!(shim.calls(), 0);
}

#[test]
fn bad_input_errors_before_any_remote_work() {
    let home = TestHome::new();
    let shim = SshShim::new();

    let err = run_remote(
        &args_remote("definitely-not-a-cache-id", REMOTE, Some(PROJECT), false),
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("couldn't resolve"),
        "actual: {err}"
    );
    assert_eq!(shim.calls(), 0);
}

#[test]
fn relative_cwd_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();

    let err = run_remote(
        &args_remote(
            doc_file.to_str().unwrap(),
            REMOTE,
            Some("relative/dir"),
            false,
        ),
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(err.to_string().contains("absolute"), "actual: {err}");
    assert_eq!(shim.calls(), 0);
}

#[test]
fn dotdot_cwd_errors_before_any_remote_work() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());
    let shim = SshShim::new();

    let err = run_remote(
        &args_remote(
            doc_file.to_str().unwrap(),
            REMOTE,
            Some("/data/../etc"),
            false,
        ),
        &RecordingExec::default(),
        Some(&home.home_dir()),
        &home.home_dir(),
        &shim.search_path(),
        true,
    )
    .unwrap_err();

    assert!(err.to_string().contains(".."), "actual: {err}");
    assert_eq!(shim.calls(), 0);
}

// ── Idempotence across invocations ──────────────────────────────────

#[test]
fn re_running_the_same_push_mints_the_same_id_and_target() {
    let home = TestHome::new();
    let docs = tempfile::tempdir().unwrap();
    let doc_file = claude_doc(docs.path());

    let mut ship_cmds = Vec::new();
    for _ in 0..2 {
        let shim = SshShim::new();
        shim.respond(0, &preflight_ok(PROJECT, "none"));
        run_remote(
            &args_remote(doc_file.to_str().unwrap(), REMOTE, Some(PROJECT), false),
            &RecordingExec::default(),
            Some(&home.home_dir()),
            &home.home_dir(),
            &shim.search_path(),
            true,
        )
        .unwrap();
        ship_cmds.push(shim.argv(1)[1].clone());
    }
    assert_eq!(
        ship_cmds[0], ship_cmds[1],
        "unchanged content must target the same remote file"
    );
}
