//! Opt-in PTY smoke tests for the native picker.
//!
//! These drive the real `path` binary inside a pseudo-terminal so the
//! whole stack runs: TTY detection, raw mode, the ratatui event loop,
//! and the exit-code contract. They're `#[ignore]` because they spawn
//! real processes with real timing — run them explicitly:
//!
//! ```sh
//! cargo test -p path-cli --test picker_pty -- --ignored
//! ```

#![cfg(unix)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// A `$HOME` with one Claude session the picker can list and import.
/// Mirrors the `claude_home_fixture` in integration.rs.
fn claude_home_fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let project_slug = project
        .to_string_lossy()
        .replace([std::path::MAIN_SEPARATOR, '_', '.'], "-");
    let project_dir = temp.path().join(".claude/projects").join(&project_slug);
    std::fs::create_dir_all(&project_dir).unwrap();
    let session_file = project_dir.join("session-pty.jsonl");
    std::fs::write(
        &session_file,
        format!(
            r#"{{"type":"user","uuid":"u-1","timestamp":"2024-01-01T00:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":"pty smoke prompt"}}}}
{{"type":"assistant","uuid":"a-1","timestamp":"2024-01-01T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}
"#,
            cwd = project.display()
        ),
    )
    .unwrap();
    (temp, session_file)
}

struct PtyRun {
    exit_code: u32,
    output: String,
}

/// Spawn the `path` binary in a fresh PTY, wait `settle` for the
/// picker to come up, send `keys`, and collect exit code + everything
/// the PTY produced.
fn run_in_pty(args: &[&str], home: &std::path::Path, cfg: &std::path::Path, keys: &[u8]) -> PtyRun {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_path"));
    cmd.args(args);
    cmd.env("HOME", home);
    cmd.env("TOOLPATH_CONFIG_DIR", cfg);
    cmd.env("TERM", "xterm-256color");
    cmd.cwd(home);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn in pty");
    drop(pair.slave);

    // Drain the PTY continuously so the child never blocks on a full
    // output buffer.
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Give the binary time to start, list sessions, and enter the
    // picker loop; raw-mode input is buffered by the PTY anyway, so
    // early keystrokes would still land — the settle just makes the
    // test deterministic-ish about *what* consumes them.
    std::thread::sleep(Duration::from_millis(1500));
    let mut writer = pair.master.take_writer().expect("pty writer");
    writer.write_all(keys).expect("send keys");
    writer.flush().expect("flush keys");

    // Wait for exit with a hard deadline so a wedged picker fails the
    // test instead of hanging the suite.
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("picker did not exit within 30s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    drop(pair.master);
    let _ = reader_thread.join();
    let mut bytes = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        bytes.extend_from_slice(&chunk);
    }
    PtyRun {
        exit_code: status.exit_code(),
        output: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// Enter on the import picker accepts the highlighted first row: the
/// session derives into the cache and the CLI exits 0.
#[test]
#[ignore = "spawns a real PTY; run with --ignored"]
fn pty_smoke_import_picker_accept_first_row() {
    let (home, _session) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    let run = run_in_pty(&["p", "import", "claude"], home.path(), cfg.path(), b"\r");
    assert_eq!(
        run.exit_code, 0,
        "import should exit 0; pty output:\n{}",
        run.output
    );
    // The derive shapes the cache id (`claude-path-claude-code-<short
    // session>`), so assert on the stable parts: a claude-* doc landed
    // in the cache and the import summary named its cache id.
    let docs_dir = cfg.path().join("documents");
    let claude_docs: Vec<_> = std::fs::read_dir(&docs_dir)
        .unwrap_or_else(|e| {
            panic!(
                "read {}: {e}; pty output:\n{}",
                docs_dir.display(),
                run.output
            )
        })
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("claude-") && n.ends_with(".json"))
        .collect();
    assert!(
        !claude_docs.is_empty(),
        "expected a claude-* cache doc in {}; pty output:\n{}",
        docs_dir.display(),
        run.output
    );
    assert!(
        run.output.contains("Imported") && run.output.contains("claude-"),
        "import summary with cache id missing from output:\n{}",
        run.output
    );
}

/// Esc is a deliberate cancel: `path share` propagates it as exit 130
/// (the same contract the external fzf backend has).
#[test]
#[ignore = "spawns a real PTY; run with --ignored"]
fn pty_smoke_esc_exits_130() {
    let (home, _session) = claude_home_fixture();
    let cfg = tempfile::tempdir().unwrap();
    // --anon skips the auth preflight, so no network is touched before
    // the picker comes up; Esc exits before any upload could happen.
    let run = run_in_pty(&["share", "--anon"], home.path(), cfg.path(), b"\x1b");
    assert_eq!(
        run.exit_code, 130,
        "esc should exit 130; pty output:\n{}",
        run.output
    );
}
