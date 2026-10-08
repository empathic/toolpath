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

use std::collections::{BTreeMap, BTreeSet};
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
    /// Fixture, relative to the repo root.
    fixture: &'static str,
    source: Source,
    /// Target step; `{ir}` and `{out}` are replaced with file paths.
    target_args: &'static [&'static str],
}

enum Source {
    Codex,
    Claude,
    Copilot,
    Pi,
}

const CASES: &[Case] = &[
    // Real captured fixtures for the two harnesses `p project` / `p export` can both read and write.
    Case {
        name: "copilot-to-claude",
        fixture: "test-fixtures/copilot/convo.jsonl",
        source: Source::Copilot,
        target_args: &["p", "project", "claude", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "pi-to-claude",
        fixture: "test-fixtures/pi/convo.jsonl",
        source: Source::Pi,
        target_args: &["p", "project", "claude", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "claude-to-pi",
        fixture: "test-fixtures/claude/convo.jsonl",
        source: Source::Claude,
        target_args: &["p", "export", "pi", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "claude-to-copilot",
        fixture: "test-fixtures/claude/convo.jsonl",
        source: Source::Claude,
        target_args: &["p", "export", "copilot", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "codex-to-claude",
        fixture: "test-fixtures/codex/convo.jsonl",
        source: Source::Codex,
        target_args: &["p", "project", "claude", "-i", "{ir}", "-o", "{out}"],
    },
    Case {
        name: "claude-to-codex",
        fixture: "test-fixtures/claude/convo.jsonl",
        source: Source::Claude,
        target_args: &["p", "export", "codex", "-i", "{ir}", "-o", "{out}"],
    },
    // Captured once by Bobby with scripts/capture-claude-session.sh (a real `claude -p` run in a
    // temp HOME); skipped while goldens/claude-session/input.jsonl is absent.
    Case {
        name: "claude-session-to-codex",
        fixture: "goldens/claude-session/input.jsonl",
        source: Source::Claude,
        target_args: &["p", "export", "codex", "-i", "{ir}", "-o", "{out}"],
    },
    // Synthetic (hand-written, not captured) but it carries a compaction boundary. It derives only
    // because the file is placed as `<sessionId>.jsonl`: the adapter matches the file stem to the
    // in-file sessionId, and a mismatched name gives "no documents produced" (goldens-baseline).
    Case {
        name: "claude-compacted-to-codex",
        fixture: "crates/toolpath-claude/tests/fixtures/compacted_session.jsonl",
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
    let input = fs::read(repo_root().join(case.fixture)).unwrap();
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
        Source::Copilot => {
            let sid = first_json(&input, |v| v.get("data")?.get("sessionId").cloned());
            let dir = home
                .join(".copilot/session-state")
                .join(sid.as_str().unwrap());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("events.jsonl"), &input).unwrap();
            vec![
                "p".into(),
                "derive".into(),
                "copilot".into(),
                "--all".into(),
            ]
        }
        Source::Pi => {
            let head = first_json(&input, |v| (v["type"] == "session").then(|| v.clone()));
            let cwd = head["cwd"].as_str().unwrap().to_string();
            // Pi's project dir: `--` + cwd without the leading `/`, `/` -> `-`, + `--`.
            let enc = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
            let ts = head["timestamp"].as_str().unwrap().replace([':', '.'], "-");
            let dir = home.join(".pi/agent/sessions").join(enc);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(format!("{ts}_{}.jsonl", head["id"].as_str().unwrap())),
                &input,
            )
            .unwrap();
            vec![
                "p".into(),
                "derive".into(),
                "pi".into(),
                "--project".into(),
                cwd,
                "--all".into(),
            ]
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
    let mut output = fs::read(&out).unwrap();
    if nondeterministic(case) {
        output = canonicalize(&output);
    }

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

/// Cases whose raw output differs between identical runs (observed 2026-10-08): `p export copilot`
/// mints a random session id and emits object keys in unstable order. Their golden is the canonical
/// form (keys sorted, session id masked), recorded as such in the manifest; every other case is
/// compared as raw bytes.
fn nondeterministic(case: &Case) -> bool {
    case.name == "claude-to-copilot"
}

fn canonicalize(output: &[u8]) -> Vec<u8> {
    fn sorted(v: Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut kv: Vec<_> = m.into_iter().collect();
                kv.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(kv.into_iter().map(|(k, v)| (k, sorted(v))).collect())
            }
            Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
            other => other,
        }
    }
    let mut text = String::from_utf8_lossy(output).into_owned();
    let id = first_json(output, |v| v.get("data")?.get("sessionId").cloned());
    text = text.replace(id.as_str().unwrap(), "<session-id>");
    text.lines()
        .map(|l| serde_json::to_string(&sorted(serde_json::from_str(l).unwrap())).unwrap() + "\n")
        .collect::<String>()
        .into_bytes()
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
    let mut claude_to_codex: Option<(Vec<u8>, Vec<u8>)> = None;

    for case in CASES {
        // A case whose input is captured by hand (scripts/capture-claude-session.sh) is skipped
        // until that input is committed.
        if !repo_root().join(case.fixture).exists() && case.fixture.starts_with("goldens/") {
            eprintln!("skipping {}: {} not captured yet", case.name, case.fixture);
            continue;
        }
        let run = run_case(case);
        let golden = dir.join(case.name).join("output.jsonl");
        // A captured input already lives under goldens/; every other fixture is copied beside its output.
        let copy_input = !case.fixture.starts_with("goldens/");
        let input_golden = dir.join(case.name).join("input.jsonl");
        if updating() {
            write(&golden, &run.output);
            if copy_input {
                write(&input_golden, &run.input);
            }
        } else {
            check_bytes(&format!("{} output", case.name), &golden, &run.output);
            if copy_input {
                check_bytes(
                    &format!("{} input (fixture drifted)", case.name),
                    &input_golden,
                    &run.input,
                );
            }
        }
        if case.name == "codex-to-claude" {
            codex_to_claude = Some(run.output.clone());
        }
        if case.name == "claude-to-codex" {
            claude_to_codex = Some((run.input.clone(), run.output.clone()));
        }
        manifest.push(json!({
            "case": case.name,
            "fixture": case.fixture,
            "input_sha256": sha256_hex(&run.input),
            "output_sha256": sha256_hex(&run.output),
            "command": run.command_line,
            "output_form": if nondeterministic(case) { "canonical (sorted keys, session id masked)" } else { "raw bytes" },
        }));
    }

    // Defect goldens.
    let (c2c_input, c2c_output) = claude_to_codex.unwrap();
    let defects = [
        (
            "known-defect/codex-to-claude-illegal-types.tsv",
            illegal_types(&codex_to_claude.unwrap()),
            "~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29)",
        ),
        (
            "known-defect/claude-to-codex-caller-cwd.tsv",
            caller_cwd(&c2c_input, &c2c_output),
            "~/.lobby/ops/REPORT-demo-goldens-toolpath-2026-10-08.md (found 2026-10-08)",
        ),
    ];
    let mut defect_docs = Vec::new();
    for (file, body, report) in &defects {
        let path = dir.join(file);
        if updating() {
            write(&path, body.as_bytes());
        } else {
            check_bytes(&format!("known defect {file}"), &path, body.as_bytes());
        }
        defect_docs.push(json!({
            "file": file,
            "sha256": sha256_hex(body.as_bytes()),
            "report": report,
        }));
    }

    // Pins: the harness and everything it uses. In check mode ANY moved pin fails, by name.
    let pins = compute_pins();
    let manifest_path = dir.join("manifest.json");
    if updating() {
        let doc = json!({
            "pins": pins,
            // Informational, not pinned: HEAD moves with every commit, rustc comes from the dev shell.
            "informational": {
                "toolpath_rev": git(&["rev-parse", "HEAD"]),
                "toolpath_dirty": !git(&["status", "--porcelain", "-uno", "--", "crates", "test-fixtures"]).is_empty(),
                "rustc": Command::new("rustc").arg("--version").output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
            },
            "cases": manifest,
            "known_defects": defect_docs,
        });
        write(
            &manifest_path,
            (serde_json::to_string_pretty(&doc).unwrap() + "\n").as_bytes(),
        );
    } else {
        let doc: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("goldens/manifest.json"))
                .unwrap();
        let mut moved = Vec::new();
        diff_json("pins", &doc["pins"], &pins, &mut moved);
        check_nix_pin(&doc["pins"]["nix_pinned_binary"], &mut moved);
        let want_cases = doc["cases"].as_array().unwrap();
        if want_cases.len() != manifest.len() {
            moved.push(format!(
                "cases: manifest lists {}, ran {} (a captured input was added or removed)",
                want_cases.len(),
                manifest.len()
            ));
        }
        for (got, want) in manifest.iter().zip(want_cases) {
            for k in ["case", "input_sha256", "output_sha256", "command"] {
                if got[k] != want[k] {
                    moved.push(format!(
                        "cases.{}.{k}: manifest {} actual {}",
                        got["case"], want[k], got[k]
                    ));
                }
            }
        }
        for (got, want) in defect_docs
            .iter()
            .zip(doc["known_defects"].as_array().unwrap())
        {
            if got["sha256"] != want["sha256"] {
                moved.push(format!("known_defects.{}.sha256", got["file"]));
            }
        }
        assert!(
            moved.is_empty(),
            "PIN MOVED ({} item(s)); the manifest no longer describes this tree:\n  {}\nIf intended: GOLDENS_UPDATE=1 cargo test -p path-cli --test goldens",
            moved.len(),
            moved.join("\n  ")
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Known defect 2, pinned as EXPECTED CURRENT BEHAVIOUR (found 2026-10-08, see the demo report).
// `path p export codex -o` writes the CALLER's working directory into every codex `cwd` field
// instead of the source session's cwd. The harness runs from `/`, so the wrong value is `/`; a
// fix makes `written_cwd` equal `source_cwd`, this golden fails, and regenerating it is the
// deliberate acknowledgement.
// ---------------------------------------------------------------------------------------------
fn caller_cwd(input: &[u8], output: &[u8]) -> String {
    let source = first_json(input, |v| v.get("cwd").cloned());
    let mut written = BTreeSet::new();
    for line in String::from_utf8_lossy(output).lines() {
        let v: Value = serde_json::from_str(line).expect("output line is JSON");
        if let Some(c) = v["payload"]["cwd"].as_str() {
            written.insert(c.to_string());
        }
    }
    let mut out = format!("source_cwd\t{}\n", source.as_str().unwrap());
    for w in written {
        out += &format!("written_cwd\t{w}\n");
    }
    out
}

fn sha_file(rel: &str) -> String {
    sha256_hex(&fs::read(repo_root().join(rel)).unwrap_or_else(|e| panic!("pin {rel}: {e}")))
}

/// Everything the goldens depend on besides the fixtures: the harness itself, the dependency
/// lock, the toolchain pin, the flake lock, and the `path` version under test.
fn compute_pins() -> Value {
    let toolchain = fs::read_to_string(repo_root().join("rust-toolchain.toml")).unwrap();
    let channel = toolchain
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel"))
        .map(|r| {
            r.trim_start_matches([' ', '='])
                .trim_matches('"')
                .to_string()
        })
        .unwrap_or_default();
    json!({
        "harness": {
            "crates/path-cli/tests/goldens.rs": sha_file("crates/path-cli/tests/goldens.rs"),
            "scripts/goldens.sh": sha_file("scripts/goldens.sh"),
            "scripts/capture-claude-session.sh": sha_file("scripts/capture-claude-session.sh"),
        },
        "cargo_lock_sha256": sha_file("Cargo.lock"),
        "flake_lock_sha256": sha_file("flake.lock"),
        "rust_toolchain": { "channel": channel, "file_sha256": sha_file("rust-toolchain.toml") },
        "path_version": env!("CARGO_PKG_VERSION"),
        "nix_pinned_binary": nix_pinned_binary(),
    })
}

/// The nix-store `path` binary on PATH (what goldens-baseline/ uses), with its narHash; null if none.
fn nix_pinned_binary() -> Value {
    let found = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .find_map(|d| {
            let real = Path::new(d).join("path").canonicalize().ok()?;
            let s = real.to_string_lossy().into_owned();
            (s.starts_with("/nix/store/") && s.contains("toolpath-path-")).then_some(s)
        });
    let Some(bin) = found else { return Value::Null };
    // /nix/store/<hash>-<name>/bin/path -> /nix/store/<hash>-<name>
    let root: String = bin.split('/').take(4).collect::<Vec<_>>().join("/");
    match nar_hash(&root) {
        Some(h) => json!({ "store_path": root, "nar_hash": h }),
        None => Value::Null,
    }
}

fn nar_hash(store_path: &str) -> Option<String> {
    let out = Command::new("nix")
        .args(["path-info", "--json", "--json-format", "1", store_path])
        .output()
        .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v[store_path]["narHash"].as_str().map(str::to_string)
}

/// A recorded nix store path is verified when it exists here; elsewhere the pin cannot be checked.
fn check_nix_pin(recorded: &Value, moved: &mut Vec<String>) {
    let Some(store) = recorded["store_path"].as_str() else {
        return;
    };
    if !Path::new(store).exists() {
        eprintln!("nix pin {store}: not present on this machine, narHash not verified");
        return;
    }
    let actual = nar_hash(store);
    if actual.as_deref() != recorded["nar_hash"].as_str() {
        moved.push(format!(
            "pins.nix_pinned_binary.nar_hash: manifest {} actual {:?}",
            recorded["nar_hash"], actual
        ));
    }
}

/// Collect the leaf paths where `want` (the manifest) and `got` (this tree) disagree.
/// The nix pin is checked separately.
fn diff_json(prefix: &str, want: &Value, got: &Value, moved: &mut Vec<String>) {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for k in w.keys().chain(g.keys().filter(|k| !w.contains_key(*k))) {
                if k == "nix_pinned_binary" {
                    continue;
                }
                diff_json(
                    &format!("{prefix}.{k}"),
                    w.get(k).unwrap_or(&Value::Null),
                    g.get(k).unwrap_or(&Value::Null),
                    moved,
                );
            }
        }
        (w, g) if w != g => moved.push(format!("{prefix}: manifest {w} actual {g}")),
        _ => {}
    }
}
