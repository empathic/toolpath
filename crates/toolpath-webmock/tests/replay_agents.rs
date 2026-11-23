//! UNRUN (written where a port cannot be bound; compile-checked only). One replay test per agent:
//! the agent's CLI, started by `run::run` against this crate's server in replay mode, answers the
//! recorded probe from the committed cassettes with no miss.
//!
//! A cell (`<WEBMOCK_CELLS>/<agent>/{home,project,sid}`) is built by the resume matrix:
//! `path p derive` the golden, `path resume --harness <agent>` into a temp HOME (see
//! `cassettes/<agent>/provenance.json`). Until that step is a library call, the cells are handed in
//! by `WEBMOCK_CELLS`.
//!
//! Skipping is loud and never green by accident: a test with no cell or no CLI on PATH prints
//! `SKIPPED <agent>: <why>` and the summary test `replay_coverage` fails unless every agent
//! named in `WEBMOCK_REQUIRE` (comma list; CI sets all) actually ran.

use std::path::PathBuf;
use std::sync::Mutex;

use toolpath_webmock::run::{self, Agent, Opts, RunMode};

static RAN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

const PROBE: &str = "Reply with exactly the first five words of the very first user message in this conversation (not this one), then the word END. No other text.";

/// The probe command per agent, as recorded in `cassettes/<agent>/provenance.json`.
fn probe_args(agent: Agent) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match agent {
        Agent::Claude => s(&[
            "-p",
            PROBE,
            "-r",
            "{sid}",
            "--model",
            "claude-haiku-4-5-20251001",
            "--output-format",
            "json",
            "--allowedTools",
            "Bash(ls)",
        ]),
        Agent::Copilot => s(&[
            "-p",
            PROBE,
            "--resume={sid}",
            "-s",
            "--no-color",
            "--no-custom-instructions",
            "--available-tools=",
        ]),
        Agent::Pi => s(&[
            "-p",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
            "--no-prompt-templates",
            "--model",
            "openai-codex/gpt-5.6-terra",
            "--session",
            "{sid}",
            PROBE,
        ]),
        Agent::Opencode => s(&[
            "run",
            "--pure",
            "--format",
            "json",
            "--model",
            "opencode/big-pickle",
            "--session",
            "{sid}",
            PROBE,
        ]),
        Agent::Gemini => s(&[
            "--resume",
            "{sid}",
            "--skip-trust",
            "--approval-mode",
            "plan",
            "-p",
            PROBE,
        ]),
        a => panic!("no probe for {}", a.name()),
    }
}

fn replay(agent: Agent, in_place: bool) {
    let name = agent.name();
    let cell = match std::env::var_os("WEBMOCK_CELLS").map(PathBuf::from) {
        Some(cells) => cells.join(name),
        None => {
            // build the cell ourselves (kept for the life of the process)
            let dir = std::env::temp_dir().join(format!("webmock-cells-{}", std::process::id()));
            match build_cell(agent, &dir) {
                Ok(c) => c,
                Err(why) => {
                    eprintln!("SKIPPED {name}: no WEBMOCK_CELLS and cannot build a cell: {why}");
                    return;
                }
            }
        }
    };
    if !cell.join("sid").is_file() {
        eprintln!("SKIPPED {name}: no cell at {}", cell.display());
        return;
    }
    if std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .all(|d| !d.join(agent.binary()).is_file())
    {
        eprintln!("SKIPPED {name}: `{}` not on PATH", agent.binary());
        return;
    }
    let opts = Opts {
        agent,
        mode: RunMode::Replay,
        cell,
        cassettes: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cassettes"),
        in_place,
        recorded_as: None,
        masks: Vec::new(),
        upstream: None,
        github_upstream: None,
        pass_env: Vec::new(),
        bin: None,
        keep: false,
        args: probe_args(agent),
    };
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let out = rt.block_on(run::run(opts)).expect("the run starts");
    assert_eq!(out.rc, Some(0), "{name}: {:?}", out.lines);
    assert_eq!(out.tally.miss, 0, "{name} missed: {:?}", out.lines);
    assert!(
        out.tally.exact + out.tally.loose > 0,
        "{name}: nothing was served"
    );
    RAN.lock().unwrap().push(agent.name());
}

#[test]
fn replay_copilot() {
    replay(Agent::Copilot, false);
}
#[test]
fn replay_claude() {
    replay(Agent::Claude, false);
}
#[test]
fn replay_pi() {
    replay(Agent::Pi, true);
}
#[test]
fn replay_opencode() {
    replay(Agent::Opencode, true);
}
#[test]
fn replay_gemini() {
    replay(Agent::Gemini, true);
}

/// Run last (name sorts after `replay_<agent>`; with `--test-threads=1`): fails when an agent
/// named in `WEBMOCK_REQUIRE` was skipped.
#[test]
fn replay_zz_coverage() {
    let Some(req) = std::env::var_os("WEBMOCK_REQUIRE") else {
        return;
    };
    let ran = RAN.lock().unwrap().clone();
    let missing: Vec<String> = req
        .to_string_lossy()
        .split(',')
        .filter(|a| !a.is_empty() && !ran.contains(a))
        .map(str::to_string)
        .collect();
    assert!(missing.is_empty(), "required but not replayed: {missing:?}");
}

/// Build `<out>/<agent>/{home,project,sid}` from the codex golden's document with the `path`
/// binary named by `WEBMOCK_PATH_BIN` (what the resume matrix did by hand). Returns why not.
fn build_cell(agent: Agent, out: &std::path::Path) -> Result<PathBuf, String> {
    let bin = std::env::var_os("WEBMOCK_PATH_BIN").ok_or("WEBMOCK_PATH_BIN not set")?;
    let snap = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../toolpath-goldens/snapshots/document@codex.snap");
    let text = std::fs::read_to_string(&snap).map_err(|e| format!("{}: {e}", snap.display()))?;
    // the document is the body after the insta header's closing `---`
    let body = text
        .split("\n---\n")
        .nth(1)
        .ok_or("no insta header in the snapshot")?;
    let cell = out.join(agent.name());
    let (home, project) = (cell.join("home"), cell.join("project"));
    std::fs::create_dir_all(&home)
        .and_then(|_| std::fs::create_dir_all(&project))
        .map_err(|e| e.to_string())?;
    let doc = cell.join("doc.json");
    std::fs::write(&doc, body).map_err(|e| e.to_string())?;
    if agent == Agent::Opencode {
        // `p export opencode` writes into an opencode db that must already exist: a listing
        // creates it (no model call, no login; the provider URL points at a dead loopback port).
        let cfg = home.join(".config/opencode");
        for d in [".config/opencode", ".local/share", ".local/state", ".cache"] {
            std::fs::create_dir_all(home.join(d)).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            cfg.join("opencode.json"),
            r#"{"autoupdate":false,"share":"disabled","provider":{"opencode":{"options":{"baseURL":"http://127.0.0.1:9/zen/v1"}}}}"#,
        )
        .map_err(|e| e.to_string())?;
        // Its exit status is not the point (a fresh store may exit non-zero); the db is.
        let _ = std::process::Command::new("opencode")
            .args(["session", "list"])
            .current_dir(&project)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("TMPDIR", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .output()
            .map_err(|e| e.to_string())?;
        if !home
            .join(".local/share/opencode/opencode-stable.db")
            .is_file()
        {
            return Err("`opencode session list` created no db".into());
        }
    }
    let mut cmd = std::process::Command::new(bin);
    if agent == Agent::Opencode {
        cmd.env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"));
    }
    let o = cmd
        .args(["p", "export", agent.name(), "-i"])
        .arg(&doc)
        .arg("-p")
        .arg(&project)
        .env("HOME", &home)
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .output()
        .map_err(|e| e.to_string())?;
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    if !o.status.success() {
        return Err(format!("path p export {}: {all}", agent.name()));
    }
    // the id follows the resume hint: `--resume X`, `-r X` or `--session X`
    let sid = all
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| matches!(w[0], "--resume" | "-r" | "--session"))
        .map(|w| w[1].to_string())
        .next_back()
        .ok_or_else(|| format!("no session id in: {all}"))?;
    std::fs::write(cell.join("sid"), sid).map_err(|e| e.to_string())?;
    Ok(cell)
}
