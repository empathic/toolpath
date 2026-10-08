//! Hermetic goldens for cross-harness transformations.
//!
//! Each case takes a REAL captured session from `test-fixtures/<harness>/convo.jsonl`, places it
//! where the source harness's adapter looks for it inside a fresh temp HOME, runs the `path`
//! binary with an EMPTY environment (only HOME, XDG_*, and the harness store dirs set, all inside
//! that temp dir), and compares the resulting JSONL byte-for-byte against `goldens/<case>/output.jsonl`.
//!
//! Regenerate on purpose (writes `goldens/**` and `goldens/manifest.json`, nothing else):
//!
//! ```text
//! GOLDENS_UPDATE=1 cargo test -p path-cli --test goldens
//! ```
//!
//! `scripts/goldens.sh` wraps both modes; see `goldens/DEMO.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PATH_BIN: &str = env!("CARGO_BIN_EXE_path");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn goldens_dir() -> PathBuf {
    repo_root().join("goldens")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn updating() -> bool {
    std::env::var_os("GOLDENS_UPDATE").is_some()
}

/// How a case reaches the target format: derive from the source harness's store, then project.
struct Case {
    name: &'static str,
    /// Fixture, relative to `test-fixtures/`.
    fixture: &'static str,
    source: Source,
    /// Target step; `{ir}` and `{out}` are replaced with file paths.
    target_args: &'static [&'static str],
}

enum Source {
    Codex,
    Claude,
}

const CASES: &[Case] = &[
    Case {
        name: "codex-to-claude",
        fixture: "codex/convo.jsonl",
        source: Source::Codex,
        target_args: &["p", "project", "claude", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "claude-to-codex",
        fixture: "claude/convo.jsonl",
        source: Source::Claude,
        target_args: &["p", "export", "codex", "-i", "{ir}", "-o", "{out}"],
    },
];

struct Run {
    input: Vec<u8>,
    output: Vec<u8>,
    command_line: String,
}

/// An empty environment pointing every store at `home`. Nothing is inherited from the caller.
fn hermetic(home: &Path) -> Command {
    let mut c = Command::new(PATH_BIN);
    // `p export codex -o` records the CALLER's cwd as the session cwd (observed 2026-10-08), so the
    // working directory is part of the input: pin it to `/`, which exists everywhere and names no user.
    c.current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("COPILOT_HOME", home.join(".copilot"))
        .env("TOOLPATH_CONFIG_DIR", home.join(".toolpath"));
    c
}

fn exec(cmd: &mut Command, what: &str) -> Vec<u8> {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: spawn: {e}"));
    assert!(
        out.status.success(),
        "{what} failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn first_json(input: &[u8], pick: impl Fn(&Value) -> Option<Value>) -> Value {
    String::from_utf8_lossy(input)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| pick(&v))
        .expect("fixture lacks the session identity this case needs")
}

fn run_case(case: &Case) -> Run {
    let input = fs::read(repo_root().join("test-fixtures").join(case.fixture)).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();

    // Place the fixture where the source adapter discovers it, and build the derive step.
    let derive_args: Vec<String> = match case.source {
        Source::Codex => {
            let meta = first_json(&input, |v| {
                (v["type"] == "session_meta").then(|| v["payload"].clone())
            });
            let id = meta["id"].as_str().unwrap();
            let ts = meta["timestamp"].as_str().unwrap();
            let (date, time) = ts.split_once('T').unwrap();
            let time = time.split('.').next().unwrap().replace(':', "-");
            let dir = home.join(".codex/sessions").join(date.replace('-', "/"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("rollout-{date}T{time}-{id}.jsonl")),
                &input,
            )
            .unwrap();
            vec!["p".into(), "derive".into(), "codex".into(), "--all".into()]
        }
        Source::Claude => {
            let cwd = first_json(&input, |v| v.get("cwd").cloned());
            let cwd = cwd.as_str().unwrap().to_string();
            let sid = first_json(&input, |v| v.get("sessionId").cloned());
            // Claude's project dir slug: `/`, `_`, `.` all become `-`.
            let slug = cwd.replace(['/', '_', '.'], "-");
            let dir = home.join(".claude/projects").join(slug);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{}.jsonl", sid.as_str().unwrap())), &input).unwrap();
            vec![
                "p".into(),
                "derive".into(),
                "claude".into(),
                "--project".into(),
                cwd,
                "--all".into(),
            ]
        }
    };

    let ir = home.join("ir.json");
    let out = home.join("out.jsonl");
    let ir_bytes = exec(hermetic(&home).args(&derive_args), "derive");
    fs::write(&ir, ir_bytes).unwrap();

    let target: Vec<String> = case
        .target_args
        .iter()
        .map(|a| match *a {
            "{ir}" => ir.to_string_lossy().into_owned(),
            "{out}" => out.to_string_lossy().into_owned(),
            other => other.to_string(),
        })
        .collect();
    exec(hermetic(&home).args(&target), "project/export");
    let output = fs::read(&out).unwrap();

    // Hermeticity: the output may mention neither the temp HOME nor the real one.
    let text = String::from_utf8_lossy(&output);
    assert!(
        !text.contains(home.to_str().unwrap()),
        "{}: output leaks the temp HOME",
        case.name
    );
    if let Ok(real) = std::env::var("HOME")
        && real.len() > 1
    {
        assert!(
            !text.contains(&real),
            "{}: output leaks the real HOME {real}",
            case.name
        );
    }

    let placeholder = |args: &[String]| args.join(" ");
    let command_line = format!(
        "cd / && env -i HOME=<tmp> XDG_*=<tmp> CLAUDE_CONFIG_DIR=<tmp> CODEX_HOME=<tmp> path {} && path {}",
        placeholder(&derive_args).replace(&home.to_string_lossy().into_owned(), "<tmp>"),
        case.target_args.join(" ")
    );
    Run {
        input,
        output,
        command_line,
    }
}

/// Readable line diff: counts, then the first few differing lines (truncated).
fn diff(golden: &str, actual: &str) -> String {
    let (g, a): (Vec<_>, Vec<_>) = (golden.lines().collect(), actual.lines().collect());
    let mut out = format!("golden: {} lines, actual: {} lines\n", g.len(), a.len());
    // Show a window of chars around the first difference, so long JSON lines stay readable.
    let window = |s: &str, at: usize| {
        let chars: Vec<char> = s.chars().collect();
        let (lo, hi) = (at.saturating_sub(100), (at + 140).min(chars.len()));
        format!(
            "{}{}{}",
            if lo > 0 { "…" } else { "" },
            chars[lo..hi].iter().collect::<String>(),
            if hi < chars.len() { "…" } else { "" }
        )
    };
    let mut shown = 0;
    for i in 0..g.len().max(a.len()) {
        let (gl, al) = (g.get(i), a.get(i));
        if gl != al {
            let (gs, as_) = (
                gl.copied().unwrap_or("<absent>"),
                al.copied().unwrap_or("<absent>"),
            );
            let at = gs
                .chars()
                .zip(as_.chars())
                .take_while(|(x, y)| x == y)
                .count();
            out += &format!(
                "@@ line {}, first difference at char {}\n-{}\n+{}\n",
                i + 1,
                at,
                window(gs, at),
                window(as_, at)
            );
            shown += 1;
            if shown == 5 {
                out += "… (further differences omitted)\n";
                break;
            }
        }
    }
    out
}

fn check_bytes(label: &str, golden_path: &Path, actual: &[u8]) {
    let golden = fs::read(golden_path).unwrap_or_else(|e| {
        panic!(
            "{label}: missing golden {}: {e} (run with GOLDENS_UPDATE=1)",
            golden_path.display()
        )
    });
    if golden != actual {
        panic!(
            "{label}: output differs from {}\n{}\nIf the change is intended: GOLDENS_UPDATE=1 cargo test -p path-cli --test goldens",
            golden_path.display(),
            diff(
                &String::from_utf8_lossy(&golden),
                &String::from_utf8_lossy(actual)
            )
        );
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(repo_root())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Known defect, pinned as EXPECTED CURRENT BEHAVIOUR.
//
// Report: ~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29 diagnosis).
// `toolpath-claude/src/project.rs::project_event` writes a foreign event's raw `event_type`
// verbatim as the Claude entry's `type`, so codex -> claude output carries codex's own wire
// vocabulary (session_meta, task_started, agent_message, token_count, ...) that real Claude Code
// does not define. Not a regression: unchanged since the projector's first commit; no other test
// fails because toolpath-claude's reader is lenient about `type`.
//
// This golden lists, with counts, every top-level `type` in the codex -> claude output that is
// outside Claude's documented vocabulary (docs/agents/formats/claude-code/entry-types.md). When
// the projector is fixed the list becomes empty, this test fails, and regenerating the golden is
// the deliberate acknowledgement that the defect is gone.
// ---------------------------------------------------------------------------------------------
const CLAUDE_TYPES: &[&str] = &[
    "user",
    "assistant",
    "system",
    "attachment",
    "file-history-snapshot",
    "permission-mode",
    "queue-operation",
    "last-prompt",
    "summary",
    "compact_boundary",
    "progress",
    "ai-title",
    "custom-title",
    "agent-name",
    "mode",
    "atis-latch",
    "pr-link",
    "frame-link",
    "relocated",
    "worktree-state",
];

fn illegal_types(output: &[u8]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in String::from_utf8_lossy(output).lines() {
        let v: Value = serde_json::from_str(line).expect("output line is JSON");
        let t = v["type"].as_str().unwrap_or("<no type>");
        if !CLAUDE_TYPES.contains(&t) {
            *counts.entry(t.to_string()).or_default() += 1;
        }
    }
    counts.iter().map(|(t, n)| format!("{t}\t{n}\n")).collect()
}

#[test]
fn goldens() {
    let dir = goldens_dir();
    let mut manifest = Vec::new();
    let mut codex_to_claude: Option<Vec<u8>> = None;

    for case in CASES {
        let run = run_case(case);
        let golden = dir.join(case.name).join("output.jsonl");
        let input_golden = dir.join(case.name).join("input.jsonl");
        if updating() {
            write(&golden, &run.output);
            write(&input_golden, &run.input);
        } else {
            check_bytes(&format!("{} output", case.name), &golden, &run.output);
            check_bytes(
                &format!("{} input (fixture drifted)", case.name),
                &input_golden,
                &run.input,
            );
        }
        if case.name == "codex-to-claude" {
            codex_to_claude = Some(run.output.clone());
        }
        manifest.push(json!({
            "case": case.name,
            "fixture": format!("test-fixtures/{}", case.fixture),
            "input_sha256": sha256_hex(&run.input),
            "output_sha256": sha256_hex(&run.output),
            "command": run.command_line,
        }));
    }

    // Defect golden.
    let defect = illegal_types(&codex_to_claude.unwrap());
    let defect_path = dir.join("known-defect/codex-to-claude-illegal-types.tsv");
    if updating() {
        write(&defect_path, defect.as_bytes());
    } else {
        check_bytes(
            "known defect (codex-to-claude illegal entry types)",
            &defect_path,
            defect.as_bytes(),
        );
    }

    let manifest_path = dir.join("manifest.json");
    if updating() {
        let doc = json!({
            "toolpath_rev": git(&["rev-parse", "HEAD"]),
            "toolpath_dirty": !git(&["status", "--porcelain", "-uno", "--", "crates", "test-fixtures"]).is_empty(),
            "path_version": env!("CARGO_PKG_VERSION"),
            "cases": manifest,
            "known_defect": {
                "file": "known-defect/codex-to-claude-illegal-types.tsv",
                "sha256": sha256_hex(defect.as_bytes()),
                "report": "~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29)",
            },
        });
        write(
            &manifest_path,
            (serde_json::to_string_pretty(&doc).unwrap() + "\n").as_bytes(),
        );
    } else {
        // The committed manifest must agree with the committed goldens and the fresh run.
        let doc: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("goldens/manifest.json"))
                .unwrap();
        for (got, want) in manifest.iter().zip(doc["cases"].as_array().unwrap()) {
            for k in ["case", "input_sha256", "output_sha256", "command"] {
                assert_eq!(
                    got[k], want[k],
                    "manifest {} field {k} is stale (run with GOLDENS_UPDATE=1)",
                    got["case"]
                );
            }
        }
        assert_eq!(
            doc["known_defect"]["sha256"],
            sha256_hex(defect.as_bytes()),
            "manifest known_defect sha256 is stale"
        );
    }
}
