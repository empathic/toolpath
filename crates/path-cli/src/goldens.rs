//! `path goldens`: capture, check and diff hermetic goldens for cross-harness transformations.
//!
//! A *golden set* is one input session (a fixture, or a real session captured with
//! `capture-claude`) plus its output in every target format the CLI can project to. `capture`
//! runs the transformations under an EMPTY environment (temp HOME/XDG/store dirs, cwd `/`),
//! writes `goldens/<name>/` and a manifest entry that pins everything the result depends on;
//! `check` re-runs and fails on any output difference or moved pin. The goldens test calls
//! [`check`] from this module, so the tool and the test cannot drift.
//!
//! Layout under the root: `goldens/manifest.json`, `goldens/<name>/input.jsonl`,
//! `goldens/<name>/to-<target>.jsonl`, `goldens/known-defect/*.tsv`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Files that make up this harness; their sha256 is pinned in every manifest entry.
/// What a producer's code is made of: a rev change touching none of these is not drift.
const PRODUCER_CODE: &[&str] = &[
    "crates",
    "Cargo.toml",
    "Cargo.lock",
    "flake.lock",
    "rust-toolchain.toml",
    "test-fixtures",
    "scripts",
];

const HARNESS_FILES: &[&str] = &[
    "crates/path-cli/src/goldens.rs",
    "scripts/goldens.sh",
    // The spec the goldens follow (rule: `check` cites it by sha and fails without it).
    SPEC_FILE,
];

const SPEC_FILE: &str = "docs/GOLDENS.md";

/// Fixtures `init` captures: (set name, harness, fixture path relative to the root).
const STANDARD_SETS: &[(&str, Harness, &str)] = &[
    ("codex", Harness::Codex, "test-fixtures/codex/convo.jsonl"),
    (
        "claude",
        Harness::Claude,
        "test-fixtures/claude/convo.jsonl",
    ),
    (
        "copilot",
        Harness::Copilot,
        "test-fixtures/copilot/convo.jsonl",
    ),
    ("pi", Harness::Pi, "test-fixtures/pi/convo.jsonl"),
    // Synthetic, with a compaction boundary.
    (
        "claude-compacted",
        Harness::Claude,
        "crates/toolpath-claude/tests/fixtures/compacted_session.jsonl",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Codex,
    Claude,
    Copilot,
    Pi,
}

impl Harness {
    fn name(self) -> &'static str {
        match self {
            Harness::Codex => "codex",
            Harness::Claude => "claude",
            Harness::Copilot => "copilot",
            Harness::Pi => "pi",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        Self::from_str(s, true).map_err(|e| anyhow!("unknown harness {s:?}: {e}"))
    }
    /// Every target format the CLI can project to, except the source's own.
    fn targets(self) -> Vec<&'static str> {
        ["claude", "codex", "pi", "copilot"]
            .into_iter()
            .filter(|t| *t != self.name())
            .collect()
    }
}

#[derive(Args, Debug)]
pub struct GoldensArgs {
    /// Repository root holding `goldens/` (default: nearest ancestor of the cwd that has one)
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: GoldensCommand,
}

#[derive(Subcommand, Debug)]
pub enum GoldensCommand {
    /// List the golden sets in the manifest
    List,
    /// Round trips IR -> adapter -> IR for every set and adapter: key paths lost and gained, text
    /// and tool-use survival (check pins today's losses; a new loss fails)
    Roundtrip {
        /// Only this set (default: all)
        name: Option<String>,
    },
    /// What information is shed when a transcript becomes a Toolpath document and is projected out:
    /// per stage events and bytes in/out, key paths lost, renames, dropped fields with one example
    Shed {
        /// Only this set (default: all)
        name: Option<String>,
        /// Write goldens/shed/<set>.json (listed in the manifest) instead of only printing
        #[arg(long)]
        json: bool,
    },
    /// Capture the standard sets from the repo's own fixtures
    Init,
    /// Capture a golden set from a fixture, hermetically, to every target; with --all re-capture
    /// every set in the manifest
    Capture(CaptureArgs),
    /// Re-run and compare to the goldens and the pins; exit 1 on any difference
    Check {
        /// Check only this set (default: all)
        name: Option<String>,
        /// Report a moved producer (the running `path` binary, or repo code differing from the pin)
        /// as a warning row instead of a problem
        #[arg(long)]
        allow_producer_drift: bool,
    },
    /// Show how a fresh run differs from a set's goldens
    Diff { name: String },
    /// Capture one REAL headless session of an installed agent hermetically (credentials copied or
    /// forwarded into a temp HOME only), leak-check it, then capture it as a golden set
    CaptureLive {
        #[arg(value_enum)]
        harness: LiveHarness,
        #[command(flatten)]
        args: CaptureLiveArgs,
    },
    /// Alias for `capture-live claude`
    CaptureClaude(CaptureLiveArgs),
    /// Print the provenance of an installed agent binary: code signature and upstream match
    /// (needs network for the upstream check; no credentials)
    Provenance {
        #[arg(value_enum)]
        harness: LiveHarness,
        /// Path to the agent executable (default: $<AGENT>_BIN, then PATH, then ~/.local/bin/<agent>)
        #[arg(long)]
        bin: Option<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct CaptureArgs {
    /// Source harness
    #[arg(value_enum, required_unless_present = "all")]
    harness: Option<Harness>,
    /// Fixture file (a session log in the harness's own format)
    #[arg(required_unless_present = "all")]
    fixture: Option<PathBuf>,
    /// Name of the golden set (default: the harness name)
    #[arg(long)]
    name: Option<String>,
    /// Project directory to place the session under (claude, pi; default: the fixture's cwd)
    #[arg(long)]
    project: Option<String>,
    /// Re-capture every set already in the manifest
    #[arg(long, conflicts_with_all = ["harness", "fixture", "name", "project"])]
    all: bool,
}

#[derive(Args, Debug)]
pub struct CaptureLiveArgs {
    /// Path to the agent executable (default: $<AGENT>_BIN, then PATH, then ~/.local/bin/<agent>)
    #[arg(long)]
    bin: Option<PathBuf>,
    /// Golden set name (default: <agent>-session)
    #[arg(long)]
    name: Option<String>,
    /// Model to run (claude defaults to claude-haiku-4-5-20251001; others use their own default)
    #[arg(long)]
    model: Option<String>,
}

pub fn run(args: GoldensArgs) -> Result<()> {
    let root = match args.root {
        Some(r) => r,
        None => find_root()?,
    };
    let mut g = Goldens::new(root, std::env::current_exe()?);
    match args.command {
        GoldensCommand::List => g.list(),
        GoldensCommand::Roundtrip { name } => g.roundtrip_report(name.as_deref()),
        GoldensCommand::Shed { name, json } => g.shed(name.as_deref(), json),
        GoldensCommand::Init => {
            for (name, harness, fixture) in STANDARD_SETS {
                g.capture(*harness, &g.root.join(fixture), name, None, None)?;
            }
            Ok(())
        }
        GoldensCommand::Capture(a) if a.all => g.capture_all(),
        GoldensCommand::Capture(a) => {
            let harness = a.harness.expect("clap requires it");
            let fixture = a.fixture.expect("clap requires it");
            let name = a.name.unwrap_or_else(|| harness.name().to_string());
            g.capture(harness, &abs(&fixture)?, &name, a.project.as_deref(), None)
        }
        GoldensCommand::Check {
            name,
            allow_producer_drift,
        } => {
            g.allow_producer_drift = allow_producer_drift;
            let report = g.check(name.as_deref())?;
            print!("{}", report.render());
            if report.problems.is_empty() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        GoldensCommand::Diff { name } => {
            let report = g.check_with(Some(&name), 20)?;
            print!("{}", report.render());
            if report.problems.is_empty() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        GoldensCommand::CaptureLive { harness, args } => g.capture_live(harness, args),
        GoldensCommand::CaptureClaude(args) => g.capture_live(LiveHarness::Claude, args),
        GoldensCommand::Provenance { harness, bin } => {
            let drv = driver(harness)?;
            let bin = resolve_agent(drv.exe, bin)?;
            let version = agent_version(&bin);
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "name": drv.exe,
                    "version": version,
                    "binary_path": bin.to_string_lossy(),
                    "binary_sha256": hash_file(&bin).unwrap_or_default(),
                    "provenance": provenance(drv.exe, &bin, &version),
                }))?
            );
            Ok(())
        }
    }
}

fn abs(p: &Path) -> Result<PathBuf> {
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    })
}

fn find_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    for d in cwd.ancestors() {
        if d.join("goldens/manifest.json").exists() || d.join("rust-toolchain.toml").exists() {
            return Ok(d.to_path_buf());
        }
    }
    Ok(cwd)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

// ---------------------------------------------------------------------------------------------
// The tool
// ---------------------------------------------------------------------------------------------

/// `exe` is the `path` binary the transformations run through (the current exe from the CLI, the
/// built binary from a test).
pub struct Goldens {
    pub root: PathBuf,
    exe: PathBuf,
    /// Downgrade producer drift (binary sha, repo rev) from a problem to a warning row.
    pub allow_producer_drift: bool,
    /// Binaries under this directory are reproducible builds whose sha is compared strictly.
    pub store_root: PathBuf,
}

/// Result of a check: one line per passing target, one entry per problem (already formatted).
#[derive(Debug, Default)]
pub struct Report {
    pub ok: Vec<String>,
    /// Information only (never a failure), e.g. a local agent binary that moved since capture.
    pub info: Vec<String>,
    /// Would be problems, but were allowed (`--allow-producer-drift`).
    pub warnings: Vec<String>,
    pub problems: Vec<String>,
}

impl Report {
    pub fn render(&self) -> String {
        let mut s = String::new();
        for l in &self.ok {
            s += &format!("ok    {l}\n");
        }
        for i in &self.info {
            s += &format!("info  {i}\n");
        }
        for w in &self.warnings {
            s += &format!("warn  {w}\n");
        }
        for p in &self.problems {
            s += &format!("FAIL  {p}\n");
        }
        s += &format!("{} ok, {} problem(s)\n", self.ok.len(), self.problems.len());
        s
    }
}

/// Library entry point for the goldens test.
pub fn check(root: &Path, exe: &Path) -> Result<Report> {
    Goldens::new(root.to_path_buf(), exe.to_path_buf()).check(None)
}

/// What one run of a set produced, per target.
struct TargetRun {
    target: &'static str,
    command: String,
    /// Ok(bytes) or Err(first line of the failure).
    result: std::result::Result<Vec<u8>, String>,
}

struct SetRun {
    source_cwd: Option<String>,
    /// The Toolpath document derived from the input (IR1).
    ir: Vec<u8>,
    targets: Vec<TargetRun>,
}

impl Goldens {
    pub fn new(root: PathBuf, exe: PathBuf) -> Self {
        Goldens {
            root,
            exe,
            allow_producer_drift: false,
            store_root: PathBuf::from("/nix/store"),
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("goldens")
    }
    fn manifest_path(&self) -> PathBuf {
        self.dir().join("manifest.json")
    }
    fn read_manifest(&self) -> Result<Value> {
        match fs::read(self.manifest_path()) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(_) => Ok(json!({ "goldens": [], "known_defects": [] })),
        }
    }
    fn write_manifest(&self, m: &Value) -> Result<()> {
        write(
            &self.manifest_path(),
            (serde_json::to_string_pretty(m)? + "\n").as_bytes(),
        )
    }

    // -- hermetic execution -----------------------------------------------------------------

    /// An empty environment pointing every store at `home`; nothing is inherited. `p export codex -o`
    /// records the CALLER's cwd as the session cwd (see the known defects), so the working directory
    /// is part of the input: pinned to `/`.
    fn hermetic(&self, home: &Path) -> Command {
        let mut c = Command::new(&self.exe);
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

    fn exec(cmd: &mut Command) -> std::result::Result<Vec<u8>, String> {
        let out = cmd.output().map_err(|e| format!("spawn: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed")
                .to_string())
        }
    }

    /// Place `input` where `harness`'s adapter discovers it inside `home`; returns the derive args
    /// and the session's cwd when it has one.
    fn place(
        harness: Harness,
        input: &[u8],
        home: &Path,
        project: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>)> {
        let pick = |f: &dyn Fn(&Value) -> Option<Value>| -> Result<Value> {
            String::from_utf8_lossy(input)
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find_map(|v| f(&v))
                .ok_or_else(|| {
                    anyhow!(
                        "fixture lacks the session identity {} needs",
                        harness.name()
                    )
                })
        };
        let s = |v: &Value| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("identity is not a string"))
        };
        let args = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        Ok(match harness {
            Harness::Codex => {
                let meta = pick(&|v| (v["type"] == "session_meta").then(|| v["payload"].clone()))?;
                let (id, ts) = (s(&meta["id"])?, s(&meta["timestamp"])?);
                let (date, time) = ts.split_once('T').ok_or_else(|| anyhow!("bad timestamp"))?;
                let time = time.split('.').next().unwrap_or("").replace(':', "-");
                let dir = home.join(".codex/sessions").join(date.replace('-', "/"));
                write(
                    &dir.join(format!("rollout-{date}T{time}-{id}.jsonl")),
                    input,
                )?;
                (
                    args(&["p", "derive", "codex", "--all"]),
                    meta["cwd"].as_str().map(str::to_string),
                )
            }
            Harness::Copilot => {
                let sid = pick(&|v| v.get("data")?.get("sessionId").cloned())?;
                write(
                    &home
                        .join(".copilot/session-state")
                        .join(s(&sid)?)
                        .join("events.jsonl"),
                    input,
                )?;
                let cwd = pick(&|v| v.get("data")?.get("context")?.get("cwd").cloned()).ok();
                (
                    args(&["p", "derive", "copilot", "--all"]),
                    cwd.and_then(|c| c.as_str().map(str::to_string)),
                )
            }
            Harness::Pi => {
                let head = pick(&|v| (v["type"] == "session").then(|| v.clone()))?;
                let cwd = match project {
                    Some(p) => p.to_string(),
                    None => s(&head["cwd"])?,
                };
                // Pi's project dir: `--` + cwd without the leading `/`, `/` -> `-`, + `--`.
                let enc = format!("--{}--", cwd.trim_start_matches('/').replace('/', "-"));
                let ts = s(&head["timestamp"])?.replace([':', '.'], "-");
                let file = format!("{ts}_{}.jsonl", s(&head["id"])?);
                write(&home.join(".pi/agent/sessions").join(enc).join(file), input)?;
                let mut a = args(&["p", "derive", "pi", "--project"]);
                a.extend([cwd.clone(), "--all".into()]);
                (a, Some(cwd))
            }
            Harness::Claude => {
                let cwd = match project {
                    Some(p) => p.to_string(),
                    None => s(&pick(&|v| v.get("cwd").cloned())?)?,
                };
                // The adapter matches the file stem to the in-file sessionId; a mismatched name
                // derives nothing ("no documents produced").
                let sid = s(&pick(&|v| v.get("sessionId").cloned())?)?;
                // Claude's project dir slug: `/`, `_`, `.` all become `-`.
                let slug = cwd.replace(['/', '_', '.'], "-");
                write(
                    &home
                        .join(".claude/projects")
                        .join(slug)
                        .join(format!("{sid}.jsonl")),
                    input,
                )?;
                let mut a = args(&["p", "derive", "claude", "--project"]);
                a.extend([cwd.clone(), "--all".into()]);
                (a, Some(cwd))
            }
        })
    }

    /// Run one set end to end in a fresh temp HOME: derive once, then every target.
    fn run_set(&self, harness: Harness, input: &[u8], project: Option<&str>) -> Result<SetRun> {
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?;
        let (derive_args, source_cwd) = Self::place(harness, input, &home, project)?;
        let ir = home.join("ir.json");
        let ir_bytes = Self::exec(self.hermetic(&home).args(&derive_args))
            .map_err(|e| anyhow!("derive failed: {e}"))?;
        write(&ir, &ir_bytes)?;
        let home_s = home.to_string_lossy().into_owned();
        let derive_cmd = derive_args.join(" ").replace(&home_s, "<tmp>");
        let mut targets = Vec::new();
        for target in harness.targets() {
            let out = home.join(format!("out-{target}.jsonl"));
            let mut a: Vec<String> = if target == "claude" {
                vec!["p".into(), "project".into(), "claude".into()]
            } else {
                vec!["p".into(), "export".into(), target.into()]
            };
            a.extend(["-i".into(), "{ir}".into(), "-o".into(), "{out}".into()]);
            let command = format!(
                "cd / && env -i HOME=<tmp> XDG_*=<tmp> CLAUDE_CONFIG_DIR=<tmp> CODEX_HOME=<tmp> path {derive_cmd} && path {}",
                a.join(" ")
            );
            let real: Vec<String> = a
                .iter()
                .map(|x| match x.as_str() {
                    "{ir}" => ir.to_string_lossy().into_owned(),
                    "{out}" => out.to_string_lossy().into_owned(),
                    o => o.to_string(),
                })
                .collect();
            let result = Self::exec(self.hermetic(&home).args(&real)).and_then(|_| {
                let bytes = fs::read(&out).map_err(|e| format!("read output: {e}"))?;
                // Hermeticity: no output may mention the temp HOME or the real one.
                let text = String::from_utf8_lossy(&bytes);
                if text.contains(&home_s) {
                    return Err("output leaks the temp HOME".into());
                }
                if let Ok(real_home) = std::env::var("HOME")
                    && real_home.len() > 1
                    && text.contains(&real_home)
                {
                    return Err("output leaks the real HOME".into());
                }
                Ok(bytes)
            });
            targets.push(TargetRun {
                target,
                command,
                result,
            });
        }
        Ok(SetRun {
            source_cwd,
            ir: ir_bytes,
            targets,
        })
    }

    // -- capture ----------------------------------------------------------------------------

    pub fn capture(
        &self,
        harness: Harness,
        fixture: &Path,
        name: &str,
        project: Option<&str>,
        agent: Option<Value>,
    ) -> Result<()> {
        let input =
            fs::read(fixture).with_context(|| format!("read fixture {}", fixture.display()))?;
        // Determinism probe: two independent runs. Raw bytes if they agree, else the canonical form
        // (keys sorted, copilot session id masked) if THAT agrees, else refuse.
        let a = self.run_set(harness, &input, project)?;
        let b = self.run_set(harness, &input, project)?;
        let set_dir = self.dir().join(name);
        let ext = fixture
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jsonl");
        let input_copy = set_dir.join(format!("input.{ext}"));
        if fs::canonicalize(fixture).ok() != fs::canonicalize(&input_copy).ok() {
            write(&input_copy, &input)?;
        }
        let rel = |p: &Path| {
            p.strip_prefix(&self.root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        };
        let mut targets = Vec::new();
        for (ta, tb) in a.targets.iter().zip(&b.targets) {
            let file = format!("goldens/{name}/to-{}.jsonl", ta.target);
            let mut entry = json!({ "target": ta.target, "command": ta.command });
            match (&ta.result, &tb.result) {
                (Ok(x), Ok(y)) => {
                    let (form, bytes) = if x == y {
                        ("raw bytes", x.clone())
                    } else if canonicalize(x) == canonicalize(y) {
                        (
                            "canonical (sorted keys, copilot session id masked)",
                            canonicalize(x),
                        )
                    } else {
                        bail!(
                            "{name} -> {}: output differs between identical runs even in canonical form",
                            ta.target
                        )
                    };
                    write(&self.root.join(&file), &bytes)?;
                    entry["status"] = json!("ok");
                    entry["file"] = json!(file);
                    entry["output_sha256"] = json!(sha256_hex(&bytes));
                    entry["output_form"] = json!(form);
                    println!(
                        "captured {name} -> {} ({form}, {} bytes)",
                        ta.target,
                        bytes.len()
                    );
                }
                (Err(e), _) | (_, Err(e)) => {
                    // Not every pair is supported; record that, so support appearing is a visible flip.
                    let stale = self.root.join(&file);
                    if stale.exists() {
                        fs::remove_file(stale)?;
                    }
                    entry["status"] = json!("unsupported");
                    entry["detail"] = json!(e);
                    println!("captured {name} -> {}: unsupported ({e})", ta.target);
                }
            }
            targets.push(entry);
        }
        let mut m = self.read_manifest()?;
        let sets = m["goldens"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("manifest: goldens is not an array"))?;
        // A live capture's leak-check record survives a re-capture of the same set.
        let prev_leak_check = sets
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| e["leak_check"].clone())
            .filter(|v| !v.is_null());
        let mut entry = json!({
            "name": name,
            // The AGENT that produced the input (not the CLI under test).
            "harness": agent.unwrap_or_else(|| self.fixture_agent(harness, &input, fixture)),
            "fixture": rel(fixture),
            "project": project,
            "input_sha256": sha256_hex(&input),
            "pins": self.pins_with_producer(),
            "targets": targets,
        });
        if let Some(v) = prev_leak_check {
            entry["leak_check"] = v;
        }
        match sets.iter_mut().find(|e| e["name"] == name) {
            Some(slot) => *slot = entry,
            None => sets.push(entry),
        }
        sets.sort_by(|x, y| x["name"].as_str().cmp(&y["name"].as_str()));
        m["informational"] = self.informational();
        // Known-defect goldens, derived from the set just captured.
        self.write_defects(&mut m, name, &a)?;
        self.write_roundtrips(&mut m, name, &a)?;
        self.write_manifest(&m)
    }

    fn capture_all(&self) -> Result<()> {
        let m = self.read_manifest()?;
        let sets: Vec<Value> = m["goldens"].as_array().cloned().unwrap_or_default();
        if sets.is_empty() {
            bail!(
                "manifest has no sets; run `path goldens init` or `path goldens capture <harness> <fixture>`"
            );
        }
        for e in sets {
            let harness = Harness::parse(e["harness"]["name"].as_str().unwrap_or(""))?;
            // A live-captured input keeps the pin of the binary that produced it.
            let live = e["harness"]["binary_sha256"]
                .is_string()
                .then(|| e["harness"].clone());
            self.capture(
                harness,
                &self.root.join(e["fixture"].as_str().unwrap_or("")),
                e["name"].as_str().unwrap_or(""),
                e["project"].as_str(),
                live,
            )?;
        }
        Ok(())
    }

    /// The agent behind a committed fixture of unknown origin: no binary is attributed (the one on
    /// PATH did not write it). The version is what the input itself declares, if it does, and the
    /// commit that first added the fixture says when it appeared.
    fn fixture_agent(&self, harness: Harness, input: &[u8], fixture: &Path) -> Value {
        let rel = fixture.strip_prefix(&self.root).unwrap_or(fixture);
        let first_seen = Command::new("git")
            .args(["log", "--diff-filter=A", "--format=%h", "--"])
            .arg(rel)
            .current_dir(&self.root)
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .last()
                    .map(str::to_string)
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into());
        let declared = declared_version(harness, input);
        json!({
            "name": harness.name(),
            "version": declared.clone().unwrap_or_else(|| "unknown".into()),
            "version_source": if declared.is_some() { "declared in the input" } else { "none" },
            "first_seen": first_seen,
        })
    }

    fn write_defects(&self, m: &mut Value, name: &str, run: &SetRun) -> Result<()> {
        for def in DEFECTS.iter().filter(|d| d.set == name) {
            let Some(t) = run.targets.iter().find(|t| t.target == def.target) else {
                continue;
            };
            let Ok(out) = &t.result else { continue };
            let body = (def.compute)(run, out);
            write(&self.root.join(def.file), body.as_bytes())?;
            let doc = json!({
                "file": def.file, "set": def.set, "target": def.target,
                "sha256": sha256_hex(body.as_bytes()), "report": def.report,
            });
            let list = m["known_defects"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("known_defects not an array"))?;
            match list.iter_mut().find(|d| d["file"] == def.file) {
                Some(slot) => *slot = doc,
                None => list.push(doc),
            }
        }
        Ok(())
    }

    // -- pins -------------------------------------------------------------------------------

    fn sha_file(&self, rel: &str) -> Value {
        fs::read(self.root.join(rel))
            .map(|b| json!(sha256_hex(&b)))
            .unwrap_or(Value::Null)
    }

    /// Everything a golden depends on besides the fixture: the harness files, the dependency lock,
    /// the toolchain pin, the flake lock, the `path` version under test, the nix-store binary.
    fn pins(&self) -> Value {
        let toolchain =
            fs::read_to_string(self.root.join("rust-toolchain.toml")).unwrap_or_default();
        let channel = toolchain
            .lines()
            .find_map(|l| l.trim().strip_prefix("channel"))
            .map(|r| {
                r.trim_start_matches([' ', '='])
                    .trim_matches('"')
                    .to_string()
            })
            .unwrap_or_default();
        let harness: serde_json::Map<String, Value> = HARNESS_FILES
            .iter()
            .map(|f| (f.to_string(), self.sha_file(f)))
            .collect();
        json!({
            "harness": harness,
            "cargo_lock_sha256": self.sha_file("Cargo.lock"),
            "flake_lock_sha256": self.sha_file("flake.lock"),
            "rust_toolchain": { "channel": channel, "file_sha256": self.sha_file("rust-toolchain.toml") },
            "path_version": env!("CARGO_PKG_VERSION"),
            "nix_pinned_binary": nix_pinned_binary(),
        })
    }

    fn exe_path(&self) -> PathBuf {
        fs::canonicalize(&self.exe).unwrap_or_else(|_| self.exe.clone())
    }

    /// The tool that ran a capture: the `path` binary (by content) and the checkout it ran in.
    /// `dirty` says the tree differed from `repo_rev`, so the binary sha is what identifies the code.
    fn producer(&self) -> Value {
        let (repo_rev, tree_dirty) = self.git_state();
        let bin = self.exe_path();
        json!({
            "binary_path": bin.to_string_lossy(),
            "binary_sha256": hash_file(&self.exe),
            "repo_rev": repo_rev,
            // A binary outside the nix store is a dev build: not reproducible, so never strict.
            "dirty": tree_dirty || !bin.starts_with(&self.store_root),
        })
    }

    fn pins_with_producer(&self) -> Value {
        let mut pins = self.pins();
        pins["producer"] = self.producer();
        pins
    }

    /// (HEAD, whether the code or fixtures differ from it).
    fn git_state(&self) -> (String, bool) {
        let git = |a: &[&str]| {
            Command::new("git")
                .args(a)
                .current_dir(&self.root)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        (
            git(&["rev-parse", "HEAD"]),
            !git(&[
                "status",
                "--porcelain",
                "-uno",
                "--",
                "crates",
                "test-fixtures",
            ])
            .is_empty(),
        )
    }

    /// Did anything the transformations depend on change between two revs? None: git cannot say.
    fn code_moved(&self, from: &str, to: &str) -> Option<bool> {
        let st = Command::new("git")
            .args(["diff", "--quiet", from, to, "--"])
            .args(PRODUCER_CODE)
            .current_dir(&self.root)
            .status()
            .ok()?;
        match st.code()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// Compare a recorded producer to the one running now. A pin from before producers were
    /// recorded is information; a different binary is a problem (a warning when allowed). A
    /// different repo rev is a problem only if the code between the two revs differs, so a commit
    /// that touches only goldens or docs does not turn every check red.
    fn check_producer(&self, label: &str, pinned: &Value, r: &mut Report) {
        if !pinned.is_object() {
            r.info.push(format!(
                "{label}: unpinned producer (captured before producers were pinned; the next capture pins it)"
            ));
            return;
        }
        let now = self.producer();
        let mut drift = Vec::new();
        // Strict only when both binaries are nix store paths (reproducible); a dev build's sha
        // changes with every cargo build, so it is information.
        let strict = Path::new(pinned["binary_path"].as_str().unwrap_or(""))
            .starts_with(&self.store_root)
            && self.exe_path().starts_with(&self.store_root);
        if pinned["binary_sha256"] != now["binary_sha256"] && !strict {
            r.info.push(format!(
                "{label}: producer: unpinned dev build (binary sha not compared; capture with the nix-built path to pin it)"
            ));
        } else if pinned["binary_sha256"] != now["binary_sha256"] {
            drift.push(format!(
                "{label}: PRODUCER MOVED binary_sha256: pinned {} ({}) running {} ({})",
                pinned["binary_sha256"],
                pinned["binary_path"],
                now["binary_sha256"],
                now["binary_path"]
            ));
        }
        let (was, is) = (
            pinned["repo_rev"].as_str().unwrap_or(""),
            now["repo_rev"].as_str().unwrap_or(""),
        );
        if was != is && !is.is_empty() {
            match self.code_moved(was, is) {
                Some(true) => drift.push(format!(
                    "{label}: PRODUCER MOVED repo_rev: pinned {was} running {is}, and the code differs between them"
                )),
                Some(false) => r.info.push(format!(
                    "{label}: repo_rev moved {was} -> {is}; code and fixtures unchanged"
                )),
                None => drift.push(format!(
                    "{label}: PRODUCER MOVED repo_rev: pinned {was} is not comparable with {is} here"
                )),
            }
        }
        if pinned["dirty"] == json!(true) && strict {
            r.info.push(format!(
                "{label}: captured from a dirty tree; the binary sha is the identity"
            ));
        }
        if self.allow_producer_drift {
            r.warnings.extend(drift);
        } else {
            r.problems.extend(drift);
        }
    }

    /// Recorded for the reader, never pinned: HEAD moves with every commit, rustc is the dev shell's.
    fn informational(&self) -> Value {
        let (toolpath_rev, toolpath_dirty) = self.git_state();
        json!({
            "toolpath_rev": toolpath_rev,
            "toolpath_dirty": toolpath_dirty,
            "rustc": Command::new("rustc").arg("--version").output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
        })
    }

    // -- list / check / diff ----------------------------------------------------------------

    fn list(&self) -> Result<()> {
        let m = self.read_manifest()?;
        let sets = m["goldens"].as_array().cloned().unwrap_or_default();
        if sets.is_empty() {
            println!("no golden sets (run `path goldens init`)");
        }
        for e in sets {
            let ts: Vec<String> = e["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    format!(
                        "{}{}",
                        t["target"].as_str().unwrap_or("?"),
                        if t["status"] == "ok" {
                            ""
                        } else {
                            " (unsupported)"
                        }
                    )
                })
                .collect();
            println!(
                "{:<18} {:<14} {:<62} -> {}",
                e["name"].as_str().unwrap_or(""),
                format!(
                    "{} {}",
                    e["harness"]["name"].as_str().unwrap_or(""),
                    e["harness"]["version"].as_str().unwrap_or("")
                ),
                e["fixture"].as_str().unwrap_or(""),
                ts.join(", ")
            );
        }
        for d in m["known_defects"].as_array().into_iter().flatten() {
            println!(
                "known defect: {} ({} -> {})",
                d["file"].as_str().unwrap_or(""),
                d["set"].as_str().unwrap_or(""),
                d["target"].as_str().unwrap_or("")
            );
        }
        Ok(())
    }

    pub fn check(&self, only: Option<&str>) -> Result<Report> {
        self.check_with(only, 5)
    }

    fn check_with(&self, only: Option<&str>, diff_lines: usize) -> Result<Report> {
        let m = self.read_manifest()?;
        let sets: Vec<Value> = m["goldens"].as_array().cloned().unwrap_or_default();
        let mut r = Report::default();
        if sets.is_empty() {
            r.problems
                .push("manifest lists no golden sets (run `path goldens init`)".into());
            return Ok(r);
        }
        if let Some(n) = only
            && !sets.iter().any(|e| e["name"] == n)
        {
            r.problems.push(format!(
                "no golden set named {n:?} (see `path goldens list`)"
            ));
            return Ok(r);
        }
        // The spec is part of the contract: cite its sha, fail without it.
        match fs::read(self.root.join(SPEC_FILE)) {
            Ok(b) => r
                .info
                .push(format!("spec {SPEC_FILE} sha256 {}", sha256_hex(&b))),
            Err(_) => r.problems.push(format!(
                "{SPEC_FILE} is missing: the goldens are defined by that spec (commit it)"
            )),
        }
        let current_pins = self.pins();
        let mut fresh_runs: Vec<(String, SetRun)> = Vec::new();
        for e in sets.iter().filter(|e| only.is_none_or(|n| e["name"] == n)) {
            let name = e["name"].as_str().unwrap_or("?").to_string();
            let harness = Harness::parse(e["harness"]["name"].as_str().unwrap_or(""))?;
            // The agent that wrote the input: a changed local binary is information, never a failure
            // (the input bytes are what is pinned).
            if let Some(want_sha) = e["harness"]["binary_sha256"].as_str() {
                match agent_binary(harness.name()) {
                    Some(bin) => {
                        let (ver, sha) = (agent_version(&bin), hash_file(&bin).unwrap_or_default());
                        if sha != want_sha {
                            r.info.push(format!(
                                "{name}: local {} is {ver} ({}), the input was captured with {} ({})",
                                harness.name(),
                                bin.display(),
                                e["harness"]["version"].as_str().unwrap_or("?"),
                                e["harness"]["binary_path"].as_str().unwrap_or("?")
                            ));
                        }
                    }
                    None => r.info.push(format!(
                        "{name}: {} is not on PATH here; binary pin not compared",
                        harness.name()
                    )),
                }
            }
            // Pins: every moved item, named.
            let mut moved = Vec::new();
            let mut want_pins = e["pins"].clone();
            if let Some(o) = want_pins.as_object_mut() {
                o.remove("producer");
            }
            diff_json("pins", &want_pins, &current_pins, &mut moved);
            check_nix_pin(&e["pins"]["nix_pinned_binary"], &mut moved);
            for p in moved {
                r.problems.push(format!("{name}: PIN MOVED {p}"));
            }
            self.check_producer(&name, &e["pins"]["producer"], &mut r);
            let prefix = format!("goldens/{name}/");
            if let Some(doc) = m["roundtrip"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["file"].as_str().is_some_and(|f| f.starts_with(&prefix)))
            {
                self.check_producer(&format!("{name} round trips"), &doc["producer"], &mut r);
            }
            // The fixture must still be the one captured.
            let fixture = self.root.join(e["fixture"].as_str().unwrap_or(""));
            let input = match fs::read(&fixture) {
                Ok(b) => b,
                Err(err) => {
                    r.problems.push(format!(
                        "{name}: fixture {} unreadable: {err}",
                        fixture.display()
                    ));
                    continue;
                }
            };
            if Some(sha256_hex(&input).as_str()) != e["input_sha256"].as_str() {
                r.problems.push(format!(
                    "{name}: fixture {} drifted from the captured input_sha256",
                    fixture.display()
                ));
                continue;
            }
            let run = match self.run_set(harness, &input, e["project"].as_str()) {
                Ok(run) => run,
                Err(err) => {
                    r.problems.push(format!("{name}: {err}"));
                    continue;
                }
            };
            for want in e["targets"].as_array().into_iter().flatten() {
                let target = want["target"].as_str().unwrap_or("?");
                let label = format!("{name} -> {target}");
                let Some(got) = run.targets.iter().find(|t| t.target == target) else {
                    r.problems
                        .push(format!("{label}: target no longer produced"));
                    continue;
                };
                match (want["status"].as_str(), &got.result) {
                    (Some("ok"), Ok(bytes)) => {
                        let bytes = if want["output_form"]
                            .as_str()
                            .is_some_and(|f| f.starts_with("canonical"))
                        {
                            canonicalize(bytes)
                        } else {
                            bytes.clone()
                        };
                        let file = self.root.join(want["file"].as_str().unwrap_or(""));
                        match fs::read(&file) {
                            Err(err) => r.problems.push(format!(
                                "{label}: golden {} unreadable: {err}",
                                file.display()
                            )),
                            Ok(golden) => {
                                if Some(sha256_hex(&golden).as_str())
                                    != want["output_sha256"].as_str()
                                {
                                    r.problems.push(format!("{label}: golden file {} does not match the manifest output_sha256 (hand-edited?)", file.display()));
                                } else if golden != bytes {
                                    r.problems.push(format!(
                                        "{label}: output differs from {}\n{}",
                                        file.display(),
                                        diff(
                                            &String::from_utf8_lossy(&golden),
                                            &String::from_utf8_lossy(&bytes),
                                            diff_lines
                                        )
                                    ));
                                } else {
                                    r.ok.push(label);
                                }
                            }
                        }
                    }
                    (Some("ok"), Err(err)) => r
                        .problems
                        .push(format!("{label}: was captured, now fails: {err}")),
                    (Some("unsupported"), Err(_)) => {
                        r.ok.push(format!("{label} (still unsupported)"))
                    }
                    (Some("unsupported"), Ok(_)) => r
                        .problems
                        .push(format!("{label}: was unsupported, now succeeds; recapture")),
                    _ => r
                        .problems
                        .push(format!("{label}: malformed manifest entry")),
                }
            }
            // Round trips: today's losses are pinned as known; only a NEW loss is a problem.
            for row in self.roundtrip_rows(&name, &run) {
                match row {
                    Err(e) => r.problems.push(format!("round trip: {e}")),
                    Ok(fresh) => {
                        let adapter = fresh["adapter"].as_str().unwrap_or("?");
                        let label = format!("{name} -> {adapter} -> IR");
                        let file = row_file(&name, adapter);
                        let pinned = m["roundtrip"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|d| d["file"] == file.as_str());
                        match (fs::read(self.root.join(&file)), pinned) {
                            (Ok(bytes), Some(p)) => {
                                if Some(sha256_hex(&bytes).as_str()) != p["sha256"].as_str() {
                                    r.problems.push(format!("{label}: {file} does not match the manifest sha256 (hand-edited?)"));
                                    continue;
                                }
                                let committed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                                let regress = roundtrip_regressions(&label, &committed, &fresh);
                                if regress.is_empty() {
                                    r.ok.push(if fresh["error"].is_string() {
                                        format!("{label} (cannot round-trip, as pinned)")
                                    } else {
                                        format!("{label} (loses {} key paths, as pinned)", fresh["lost"].as_array().map_or(0, Vec::len))
                                    });
                                } else {
                                    r.problems.extend(regress);
                                }
                            }
                            _ => r.problems.push(format!("{label}: no committed round-trip row {file}; run `path goldens capture --all`")),
                        }
                    }
                }
            }
            fresh_runs.push((name, run));
        }
        // Shed reports are evidence: the files listed in the manifest must be the ones on disk.
        for d in m["shed"].as_array().into_iter().flatten() {
            let file = d["file"].as_str().unwrap_or("?");
            match fs::read(self.root.join(file)) {
                Ok(b) if Some(sha256_hex(&b).as_str()) == d["sha256"].as_str() => {}
                Ok(_) => r.problems.push(format!(
                    "{file} does not match the manifest sha256 (hand-edited?)"
                )),
                Err(e) => r.problems.push(format!("{file} unreadable: {e}")),
            }
        }
        // Known defects: recomputed from fresh output, compared to the pinned file.
        for def in DEFECTS {
            let Some((_, run)) = fresh_runs.iter().find(|(n, _)| n == def.set) else {
                continue;
            };
            let Some(t) = run.targets.iter().find(|t| t.target == def.target) else {
                continue;
            };
            let Ok(out) = &t.result else { continue };
            let body = (def.compute)(run, out);
            match fs::read_to_string(self.root.join(def.file)) {
                Ok(golden) if golden == body => r.ok.push(format!("known defect {} (still present)", def.file)),
                Ok(golden) => r.problems.push(format!(
                    "known defect {} changed: the defect may be FIXED; if so recapture on purpose\n{}",
                    def.file,
                    diff(&golden, &body, diff_lines)
                )),
                Err(err) => r.problems.push(format!("known defect {} unreadable: {err}", def.file)),
            }
            let want = m["known_defects"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["file"] == def.file);
            if want.and_then(|d| d["sha256"].as_str()) != Some(sha256_hex(body.as_bytes()).as_str())
            {
                r.problems.push(format!(
                    "known defect {}: manifest sha256 is stale",
                    def.file
                ));
            }
        }
        Ok(r)
    }
}

// ---------------------------------------------------------------------------------------------
// Known defects, pinned as EXPECTED CURRENT BEHAVIOUR. A fix changes the computed body, `check`
// fails, and recapturing is the deliberate acknowledgement that the defect is gone.
// ---------------------------------------------------------------------------------------------

struct Defect {
    file: &'static str,
    set: &'static str,
    target: &'static str,
    report: &'static str,
    compute: fn(&SetRun, &[u8]) -> String,
}

const DEFECTS: &[Defect] = &[
    // ~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29): `toolpath-claude/src/project.rs::
    // project_event` writes a foreign event's raw `event_type` as the Claude entry's `type`, so codex ->
    // claude output carries codex's wire vocabulary that real Claude Code does not define. Not a
    // regression (unchanged since the projector's first commit); no other test fails because
    // toolpath-claude's reader is lenient about `type`.
    Defect {
        file: "goldens/known-defect/codex-to-claude-illegal-types.tsv",
        set: "codex",
        target: "claude",
        report: "~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29)",
        compute: |_, out| illegal_types(out),
    },
    // Found 2026-10-08: `p export codex -o` writes the CALLER's cwd into every codex `cwd` field instead
    // of the source session's. The harness runs from `/`, so the wrong value is `/`.
    Defect {
        file: "goldens/known-defect/claude-to-codex-caller-cwd.tsv",
        set: "claude",
        target: "codex",
        report: "~/.lobby/ops/REPORT-demo-goldens-toolpath-2026-10-08.md (found 2026-10-08)",
        compute: caller_cwd,
    },
];

/// Top-level `type` values documented for Claude Code (docs/agents/formats/claude-code/entry-types.md).
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
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for line in String::from_utf8_lossy(output).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let t = v["type"].as_str().unwrap_or("<no type>");
        if !CLAUDE_TYPES.contains(&t) {
            *counts.entry(t.to_string()).or_default() += 1;
        }
    }
    counts.iter().map(|(t, n)| format!("{t}\t{n}\n")).collect()
}

fn caller_cwd(run: &SetRun, output: &[u8]) -> String {
    let mut written = BTreeSet::new();
    for line in String::from_utf8_lossy(output).lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line)
            && let Some(c) = v["payload"]["cwd"].as_str()
        {
            written.insert(c.to_string());
        }
    }
    let mut out = format!(
        "source_cwd\t{}\n",
        run.source_cwd.as_deref().unwrap_or("<none>")
    );
    for w in written {
        out += &format!("written_cwd\t{w}\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Canonical form, diff, pin comparison
// ---------------------------------------------------------------------------------------------

/// Canonical form for outputs that differ between identical runs (`p export copilot` mints a random
/// session id and emits keys in unstable order): keys sorted, the copilot session id masked.
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
    let id = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            v.get("data")?
                .get("sessionId")?
                .as_str()
                .map(str::to_string)
        });
    if let Some(id) = id {
        text = text.replace(&id, "<session-id>");
    }
    text.lines()
        .map(|l| match serde_json::from_str::<Value>(l) {
            Ok(v) => serde_json::to_string(&sorted(v)).unwrap_or_default() + "\n",
            Err(_) => l.to_string() + "\n",
        })
        .collect::<String>()
        .into_bytes()
}

/// Readable line diff: counts, then the first `max` differing lines, windowed around the first
/// differing character so long JSON lines stay readable.
fn diff(golden: &str, actual: &str, max: usize) -> String {
    let (g, a): (Vec<_>, Vec<_>) = (golden.lines().collect(), actual.lines().collect());
    let mut out = format!("golden: {} lines, actual: {} lines\n", g.len(), a.len());
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
                "@@ line {}, first difference at char {at}\n-{}\n+{}\n",
                i + 1,
                window(gs, at),
                window(as_, at)
            );
            shown += 1;
            if shown == max {
                out += "… (further differences omitted)\n";
                break;
            }
        }
    }
    out
}

/// Collect the leaf paths where `want` (the manifest) and `got` (this tree) disagree. The nix pin
/// is checked separately.
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

/// The nix-store `path` binary on PATH (what a baseline built from the pinned flake uses), with its
/// narHash; null if there is none.
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
            "pins.nix_pinned_binary.nar_hash: manifest {} actual {actual:?}",
            recorded["nar_hash"]
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// capture-claude: one real `claude -p` session, captured hermetically
// ---------------------------------------------------------------------------------------------

/// `ls` with no flags, so no owner, size or date columns (machine-specific) enter the transcript.
const CAPTURE_PROMPT: &str = "Run the shell command `ls` exactly, with no arguments or flags, to list the file names in the current directory. Then read the file notes.txt and tell me its first line.";
/// Exactly `ls`; `Bash(ls:*)` would also permit `ls -la`.
const CAPTURE_TOOLS: [&str; 3] = ["Read", "Glob", "Bash(ls)"];

/// Which environment variable a credential belongs in.
#[derive(Debug, PartialEq, Eq)]
enum CredentialKind {
    OauthToken,
    ApiKey,
}

impl CredentialKind {
    fn var(&self) -> &'static str {
        match self {
            CredentialKind::OauthToken => "CLAUDE_CODE_OAUTH_TOKEN",
            CredentialKind::ApiKey => "ANTHROPIC_API_KEY",
        }
    }
}

/// Normalise a credential read from the environment or the Keychain: strip whitespace, decode an
/// all-hex even-length value (observed 2026-10-08: `security -w` prints a value holding a stray
/// newline as hex), strip again, and route by `sk-ant-oat` / `sk-ant-api` prefix. Errors never
/// contain the value.
fn normalize_credential(raw: &str) -> Result<(CredentialKind, String)> {
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let mut v = strip(raw);
    if !v.is_empty() && v.len() % 2 == 0 && v.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        let bytes = hex::decode(&v)
            .map_err(|_| anyhow!("credential looks hex-encoded but does not decode"))?;
        v = strip(
            &String::from_utf8(bytes).map_err(|_| anyhow!("hex-decoded credential is not text"))?,
        );
    }
    if v.starts_with("sk-ant-oat") {
        Ok((CredentialKind::OauthToken, v))
    } else if v.starts_with("sk-ant-api") {
        Ok((CredentialKind::ApiKey, v))
    } else {
        bail!(
            "credential does not start with sk-ant-oat or sk-ant-api after normalising (length {})",
            v.len()
        )
    }
}

/// Env var, else the macOS Keychain item. Returns (where it came from, raw value).
fn read_credential() -> Result<(String, String)> {
    for var in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return Ok((format!("${var}"), v));
        }
    }
    if let Ok(out) = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "claude-code-oauth-token",
            "-w",
        ])
        .output()
        && out.status.success()
    {
        let v = String::from_utf8_lossy(&out.stdout).into_owned();
        if !v.trim().is_empty() {
            return Ok(("Keychain item claude-code-oauth-token".into(), v));
        }
    }
    bail!(
        "no credential: export CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY, or add Keychain item 'claude-code-oauth-token'"
    )
}

// ---------------------------------------------------------------------------------------------
// Stub factory (spec rule 10): test inputs are derived from captured bytes, never typed.
//
//     let g = path_cli::goldens::golden("claude");
//     let jsonl = g.slice(0..6);                                  // a prefix of a real session
//     let bad = g.with_field(2, "message.role", json!("robot"));  // one-field mutation of one line
// ---------------------------------------------------------------------------------------------

/// A handle on one golden set's captured input.
pub struct Golden {
    name: String,
    text: String,
}

/// The captured input of golden set `set`, read from the repo's `goldens/manifest.json` (the path
/// the manifest names for the set's fixture). Panics with a readable message if the set is unknown:
/// this is a test helper.
pub fn golden(set: &str) -> Golden {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join("goldens/manifest.json")).expect("goldens/manifest.json"),
    )
    .expect("manifest is JSON");
    let fixture = manifest["goldens"]
        .as_array()
        .and_then(|a| a.iter().find(|e| e["name"] == set))
        .and_then(|e| e["fixture"].as_str())
        .unwrap_or_else(|| panic!("no golden set named {set:?}"))
        .to_string();
    let text = fs::read_to_string(root.join(&fixture)).unwrap_or_else(|e| panic!("{fixture}: {e}"));
    Golden {
        name: set.to_string(),
        text,
    }
}

impl Golden {
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The whole captured input.
    pub fn input(&self) -> &str {
        &self.text
    }
    /// Every line parsed as JSON (non-JSON lines are skipped).
    pub fn events(&self) -> Vec<Value> {
        self.text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    /// Lines `range` of the input, newline-joined (no trailing newline).
    pub fn slice(&self, range: std::ops::Range<usize>) -> String {
        self.text
            .lines()
            .skip(range.start)
            .take(range.len())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// The first string value of key `key` anywhere in the input (e.g. `sessionId`, `cwd`).
    pub fn first_string(&self, key: &str) -> Option<String> {
        fn find(v: &Value, key: &str) -> Option<String> {
            match v {
                Value::Object(m) => m
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| m.values().find_map(|x| find(x, key))),
                Value::Array(a) => a.iter().find_map(|x| find(x, key)),
                _ => None,
            }
        }
        self.events().iter().find_map(|v| find(v, key))
    }
    /// The input with `value` set at dotted `path` on line `line` (one-field mutation).
    pub fn with_field(&self, line: usize, path: &str, value: Value) -> String {
        let mut lines: Vec<String> = self.text.lines().map(str::to_string).collect();
        let mut v: Value = serde_json::from_str(&lines[line]).expect("line is JSON");
        let mut cur = &mut v;
        let parts: Vec<&str> = path.split('.').collect();
        for p in &parts[..parts.len() - 1] {
            cur = &mut cur[*p];
        }
        cur[parts[parts.len() - 1]] = value;
        lines[line] = serde_json::to_string(&v).expect("serialises");
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------------------------
// Round trips: IR1 = derive(input); X = project(IR1); IR2 = derive(X). One row per (set, adapter X).
// ---------------------------------------------------------------------------------------------

/// Leaf key paths of a document: array indices collapsed to `[]`, artifact-URL keys (whose text embeds
/// a session id) and actor-id keys collapsed to `<artifact>` / `<actor>`, so two derivations of the
/// same session compare by structure, not by identity.
fn key_paths(v: &Value) -> BTreeSet<String> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let seg = if k.contains("://") {
                        "<artifact>"
                    } else if prefix.ends_with("actors") {
                        "<actor>"
                    } else {
                        k.as_str()
                    };
                    walk(&format!("{prefix}.{seg}"), x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(&format!("{prefix}[]"), x, out)),
            _ => {
                out.insert(prefix.trim_start_matches('.').to_string());
            }
        }
    }
    let mut out = BTreeSet::new();
    walk("", v, &mut out);
    out
}

/// (steps, conversation text chars, tool uses) of a Toolpath document.
fn ir_counts(v: &Value) -> (usize, usize, usize) {
    fn walk(v: &Value, c: &mut (usize, usize, usize)) {
        match v {
            Value::Object(m) => {
                if m.get("type").and_then(Value::as_str) == Some("conversation.append") {
                    c.1 += m
                        .get("text")
                        .and_then(Value::as_str)
                        .map_or(0, |t| t.chars().count());
                    c.2 += m
                        .get("tool_uses")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                }
                if m.contains_key("step") && m.contains_key("change") {
                    c.0 += 1;
                }
                m.values().for_each(|x| walk(x, c));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, c)),
            _ => {}
        }
    }
    let mut c = (0, 0, 0);
    walk(v, &mut c);
    c
}

/// One round-trip row: what IR1 had that IR2 lost, what IR2 gained, and text / tool-use survival.
fn roundtrip_row(set: &str, adapter: &str, ir1: &Value, ir2: &Value) -> Value {
    let (k1, k2) = (key_paths(ir1), key_paths(ir2));
    let (c1, c2) = (ir_counts(ir1), ir_counts(ir2));
    json!({
        "set": set,
        "adapter": adapter,
        "ir1": { "steps": c1.0, "text_chars": c1.1, "tool_uses": c1.2, "key_paths": k1.len() },
        "ir2": { "steps": c2.0, "text_chars": c2.1, "tool_uses": c2.2, "key_paths": k2.len() },
        "lost": k1.difference(&k2).collect::<Vec<_>>(),
        "gained": k2.difference(&k1).collect::<Vec<_>>(),
    })
}

fn row_file(set: &str, adapter: &str) -> String {
    format!("goldens/roundtrip/{set}-{adapter}.json")
}

/// Problems when a fresh row is worse than the committed one: a loss that was not there before, or
/// less text / fewer tool uses surviving. Losses that shrank are not problems.
fn roundtrip_regressions(label: &str, committed: &Value, fresh: &Value) -> Vec<String> {
    match (committed["error"].as_str(), fresh["error"].as_str()) {
        (Some(a), Some(b)) if a == b => return Vec::new(),
        (Some(_), Some(b)) => {
            return vec![format!(
                "{label}: still cannot round-trip, but differently: {b}"
            )];
        }
        (Some(_), None) => {
            return vec![format!(
                "{label}: now round-trips (was pinned as an error); recapture on purpose"
            )];
        }
        (None, Some(b)) => return vec![format!("{label}: can no longer round-trip: {b}")],
        (None, None) => {}
    }
    let set_of = |v: &Value| -> BTreeSet<String> {
        v["lost"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    let (old, new) = (set_of(committed), set_of(fresh));
    let mut out = Vec::new();
    let grown: Vec<_> = new.difference(&old).take(8).collect();
    if !grown.is_empty() {
        out.push(format!(
            "{label}: round trip loses {} key path(s) it did not before, e.g. {}",
            new.difference(&old).count(),
            grown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for k in ["text_chars", "tool_uses", "steps"] {
        let (a, b) = (
            committed["ir2"][k].as_u64().unwrap_or(0),
            fresh["ir2"][k].as_u64().unwrap_or(0),
        );
        if b < a {
            out.push(format!(
                "{label}: round trip keeps fewer {k}: {b} (was {a})"
            ));
        }
    }
    out
}

impl Goldens {
    /// Round-trip rows for one run: for every adapter X the set projects to, derive IR2 from X's raw output.
    fn roundtrip_rows(&self, set: &str, run: &SetRun) -> Vec<std::result::Result<Value, String>> {
        let ir1: Value = match serde_json::from_slice(&run.ir) {
            Ok(v) => v,
            Err(e) => return vec![Err(format!("{set}: IR1 is not JSON: {e}"))],
        };
        let mut rows = Vec::new();
        for t in &run.targets {
            let Ok(bytes) = &t.result else { continue };
            let one = || -> Result<Value> {
                let adapter = Harness::parse(t.target)?;
                let tmp = tempfile::tempdir()?;
                let home = tmp.path().canonicalize()?;
                let (args, _) = Self::place(adapter, bytes, &home, None)?;
                let ir2 = Self::exec(self.hermetic(&home).args(&args))
                    .map_err(|e| anyhow!("derive from {} failed: {e}", t.target))?;
                let ir2: Value = serde_json::from_slice(&ir2)?;
                Ok(roundtrip_row(set, t.target, &ir1, &ir2))
            };
            // A pair that cannot be re-derived at all is a pinned row too (`error`), not a crash.
            rows.push(Ok(match one() {
                Ok(v) => v,
                Err(e) => json!({ "set": set, "adapter": t.target, "error": e.to_string() }),
            }));
        }
        rows
    }

    fn write_roundtrips(&self, m: &mut Value, set: &str, run: &SetRun) -> Result<()> {
        for row in self.roundtrip_rows(set, run) {
            let row = row.map_err(|e| anyhow!(e))?;
            let file = row_file(set, row["adapter"].as_str().unwrap_or("?"));
            let body = serde_json::to_string_pretty(&row)? + "\n";
            write(&self.root.join(&file), body.as_bytes())?;
            let doc = json!({
                "file": file,
                "sha256": sha256_hex(body.as_bytes()),
                "producer": self.producer(),
            });
            if m["roundtrip"].is_null() {
                m["roundtrip"] = json!([]);
            }
            let list = m["roundtrip"]
                .as_array_mut()
                .ok_or_else(|| anyhow!("roundtrip not an array"))?;
            match list.iter_mut().find(|d| d["file"] == file.as_str()) {
                Some(slot) => *slot = doc,
                None => list.push(doc),
            }
            list.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        }
        Ok(())
    }

    /// `path goldens roundtrip [set]`: print one line per (set, adapter); no files written.
    fn roundtrip_report(&self, only: Option<&str>) -> Result<()> {
        let m = self.read_manifest()?;
        let p = self.producer();
        println!(
            "producer: {} rev {}{} ({})",
            p["binary_sha256"].as_str().unwrap_or("?"),
            p["repo_rev"].as_str().unwrap_or("?"),
            if p["dirty"] == json!(true) {
                " dirty"
            } else {
                ""
            },
            p["binary_path"].as_str().unwrap_or("?")
        );
        println!(
            "{:<18} {:<8} {:>11} {:>14} {:>10} {:>6} {:>6}",
            "set", "adapter", "steps", "text chars", "tool uses", "lost", "gained"
        );
        for e in m["goldens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| only.is_none_or(|n| e["name"] == n))
        {
            let name = e["name"].as_str().unwrap_or("?");
            let harness = Harness::parse(e["harness"]["name"].as_str().unwrap_or(""))?;
            let input = fs::read(self.root.join(e["fixture"].as_str().unwrap_or("")))?;
            let run = self.run_set(harness, &input, e["project"].as_str())?;
            for row in self.roundtrip_rows(name, &run) {
                match row {
                    Ok(r) if r["error"].is_string() => println!(
                        "{:<18} {:<8} cannot round-trip: {}",
                        name,
                        r["adapter"].as_str().unwrap_or(""),
                        r["error"].as_str().unwrap_or("")
                    ),
                    Ok(r) => println!(
                        "{:<18} {:<8} {:>5}->{:<5} {:>6}->{:<7} {:>4}->{:<5} {:>6} {:>6}",
                        name,
                        r["adapter"].as_str().unwrap_or(""),
                        r["ir1"]["steps"],
                        r["ir2"]["steps"],
                        r["ir1"]["text_chars"],
                        r["ir2"]["text_chars"],
                        r["ir1"]["tool_uses"],
                        r["ir2"]["tool_uses"],
                        r["lost"].as_array().map_or(0, Vec::len),
                        r["gained"].as_array().map_or(0, Vec::len)
                    ),
                    Err(e) => println!("{name:<18} ERROR {e}"),
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Shed: what is lost between a transcript, the Toolpath document, and a projection
// ---------------------------------------------------------------------------------------------

/// `type` or `type/payload.type`, the way a transcript line names its event.
fn event_type(v: &Value) -> String {
    let t = v["type"].as_str().unwrap_or("?");
    match v["payload"]["type"].as_str() {
        Some(p) => format!("{t}/{p}"),
        None => t.to_string(),
    }
}

fn type_counts(text: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for l in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        *m.entry(event_type(&l)).or_default() += 1;
    }
    m
}

fn string_chars(v: &Value) -> usize {
    match v {
        Value::String(s) => s.chars().count(),
        Value::Array(a) => a.iter().map(string_chars).sum(),
        Value::Object(m) => m.values().map(string_chars).sum(),
        _ => 0,
    }
}

/// Leaf key paths (arrays collapsed) of every line, each with the first example value seen.
fn raw_paths(text: &str) -> BTreeMap<String, String> {
    fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
        match v {
            Value::Object(m) => m
                .iter()
                .for_each(|(k, x)| walk(&format!("{prefix}.{k}"), x, out)),
            Value::Array(a) => a.iter().for_each(|x| walk(&format!("{prefix}[]"), x, out)),
            leaf => {
                let s = match leaf {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.entry(prefix.trim_start_matches('.').to_string())
                    .or_insert(s);
            }
        }
    }
    let mut out = BTreeMap::new();
    for l in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        walk("", &l, &mut out);
    }
    out
}

/// An example value for a dropped field, clipped to 40 chars; never anything the leak rules would flag.
fn example_value(path: &str, value: &str) -> String {
    let last = path.rsplit(['.', ']']).next().unwrap_or("").to_lowercase();
    let sensitive = [
        "token",
        "secret",
        "password",
        "signature",
        "apikey",
        "auth",
        "cookie",
    ]
    .iter()
    .any(|s| last.contains(s))
        || is_identifier_key(&last)
        || last.ends_with("id")
        || last.ends_with("uuid")
        || (value.len() > 40 && !value.contains(' '));
    if sensitive {
        return "<withheld>".into();
    }
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() > 40 {
        format!("{}…", flat.chars().take(40).collect::<String>())
    } else {
        flat
    }
}

fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl Goldens {
    /// Project an IR document to `target` in a fresh temp HOME (used for the same-format round trip).
    fn project_ir(&self, ir: &[u8], target: &str) -> std::result::Result<Vec<u8>, String> {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let home = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        let (irf, out) = (home.join("ir.json"), home.join("out.jsonl"));
        fs::write(&irf, ir).map_err(|e| e.to_string())?;
        let mut a: Vec<String> = if target == "claude" {
            vec!["p".into(), "project".into(), "claude".into()]
        } else {
            vec!["p".into(), "export".into(), target.into()]
        };
        a.extend([
            "-i".into(),
            irf.to_string_lossy().into_owned(),
            "-o".into(),
            out.to_string_lossy().into_owned(),
        ]);
        Self::exec(self.hermetic(&home).args(&a))?;
        fs::read(&out).map_err(|e| e.to_string())
    }

    fn shed_set(&self, e: &Value) -> Result<Value> {
        let name = e["name"].as_str().unwrap_or("?");
        let harness = Harness::parse(e["harness"]["name"].as_str().unwrap_or(""))?;
        let input = fs::read(self.root.join(e["fixture"].as_str().unwrap_or("")))?;
        let src = String::from_utf8_lossy(&input).into_owned();
        let run = self.run_set(harness, &input, e["project"].as_str())?;
        let ir: Value = serde_json::from_slice(&run.ir)?;
        let (steps, text_chars, tool_uses) = ir_counts(&ir);
        let projections: Vec<Value> = run
            .targets
            .iter()
            .filter_map(|t| {
                let bytes = t.result.as_ref().ok()?;
                let text = String::from_utf8_lossy(bytes);
                let chars: usize = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| string_chars(&v)).sum();
                Some(json!({ "target": t.target, "lines": text.lines().count(), "bytes": bytes.len(), "string_chars": chars, "by_type": type_counts(&text) }))
            })
            .collect();
        // Same-format round trip: source -> IR -> source projection.
        let back = self.project_ir(&run.ir, harness.name());
        let same = match back {
            Ok(bytes) => {
                let back_text = String::from_utf8_lossy(&bytes).into_owned();
                let (before, after) = (raw_paths(&src), raw_paths(&back_text));
                let (tb, ta) = (type_counts(&src), type_counts(&back_text));
                let mut by_type = serde_json::Map::new();
                for (t, n) in &tb {
                    let m = ta.get(t).copied().unwrap_or(0);
                    if m < *n {
                        by_type.insert(t.clone(), json!({ "source": n, "after": m }));
                    }
                }
                let lost: Vec<&String> =
                    before.keys().filter(|k| !after.contains_key(*k)).collect();
                let gained: Vec<&String> =
                    after.keys().filter(|k| !before.contains_key(*k)).collect();
                let renamed: Vec<Value> = lost
                    .iter()
                    .filter_map(|l| {
                        gained
                            .iter()
                            .find(|g| **g != *l && snake_case(g) == snake_case(l))
                            .map(|g| json!({ "from": l, "to": g }))
                    })
                    .collect();
                let dropped: Vec<Value> = lost
                    .iter()
                    .map(|k| json!({ "path": k, "example": example_value(k, &before[*k]) }))
                    .collect();
                json!({
                    "lines": back_text.lines().count(), "bytes": bytes.len(),
                    "event_types_shed": by_type,
                    "key_paths": { "source": before.len(), "after": after.len(), "lost": lost.len(), "gained": gained.len() },
                    "renamed": renamed, "dropped": dropped,
                })
            }
            Err(err) => json!({ "error": err }),
        };
        Ok(json!({
            "set": name, "harness": harness.name(),
            "source": {
                "lines": src.lines().count(), "bytes": input.len(),
                "string_chars": src.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| string_chars(&v)).sum::<usize>(),
                "by_type": type_counts(&src),
            },
            "ir": { "bytes": run.ir.len(), "string_chars": string_chars(&ir), "steps": steps, "text_chars": text_chars, "tool_uses": tool_uses },
            "projections": projections,
            "same_format": same,
        }))
    }

    /// `path goldens shed [set] [--json]`.
    fn shed(&self, only: Option<&str>, json_out: bool) -> Result<()> {
        let mut m = self.read_manifest()?;
        let sets: Vec<Value> = m["goldens"].as_array().cloned().unwrap_or_default();
        println!(
            "{:<17} {:<7} {:>9} {:>9} {:>9}  {:<30} {:>5} {:>6} {:>7}",
            "set",
            "stage",
            "bytes",
            "chars",
            "lines",
            "same-format round trip",
            "lost",
            "gained",
            "renamed"
        );
        for e in sets.iter().filter(|e| only.is_none_or(|n| e["name"] == n)) {
            let r = self.shed_set(e)?;
            let name = r["set"].as_str().unwrap_or("?");
            println!(
                "{:<17} {:<7} {:>9} {:>9} {:>9}",
                name,
                "source",
                r["source"]["bytes"],
                r["source"]["string_chars"],
                r["source"]["lines"]
            );
            println!(
                "{:<17} {:<7} {:>9} {:>9} {:>9}  ({} steps, {} text chars, {} tool uses)",
                "",
                "IR",
                r["ir"]["bytes"],
                r["ir"]["string_chars"],
                "-",
                r["ir"]["steps"],
                r["ir"]["text_chars"],
                r["ir"]["tool_uses"]
            );
            for p in r["projections"].as_array().into_iter().flatten() {
                println!(
                    "{:<17} {:<7} {:>9} {:>9} {:>9}",
                    "",
                    p["target"].as_str().unwrap_or(""),
                    p["bytes"],
                    p["string_chars"],
                    p["lines"]
                );
            }
            let s = &r["same_format"];
            if let Some(err) = s["error"].as_str() {
                println!("{:<17} same-format round trip failed: {err}", "");
            } else {
                let shed: Vec<String> = s["event_types_shed"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(t, c)| format!("{t} {}->{}", c["source"], c["after"]))
                    .collect();
                println!(
                    "{:<17} {:<7} {:>9} {:>9} {:>9}  {:<30} {:>5} {:>6} {:>7}",
                    "",
                    "→source",
                    s["bytes"],
                    "-",
                    s["lines"],
                    "",
                    s["key_paths"]["lost"],
                    s["key_paths"]["gained"],
                    s["renamed"].as_array().map_or(0, Vec::len)
                );
                if !shed.is_empty() {
                    println!("{:<17} event types shed: {}", "", shed.join(", "));
                }
                for d in s["dropped"].as_array().into_iter().flatten().take(6) {
                    println!(
                        "{:<17}   dropped {}  e.g. {:?}",
                        "",
                        d["path"].as_str().unwrap_or(""),
                        d["example"].as_str().unwrap_or("")
                    );
                }
                for d in s["renamed"].as_array().into_iter().flatten() {
                    println!(
                        "{:<17}   renamed {} -> {}",
                        "",
                        d["from"].as_str().unwrap_or(""),
                        d["to"].as_str().unwrap_or("")
                    );
                }
            }
            if json_out {
                let file = format!("goldens/shed/{name}.json");
                let body = serde_json::to_string_pretty(&r)? + "\n";
                write(&self.root.join(&file), body.as_bytes())?;
                let doc = json!({ "file": file, "sha256": sha256_hex(body.as_bytes()) });
                if m["shed"].is_null() {
                    m["shed"] = json!([]);
                }
                let list = m["shed"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("shed not an array"))?;
                match list.iter_mut().find(|d| d["file"] == file.as_str()) {
                    Some(slot) => *slot = doc,
                    None => list.push(doc),
                }
                list.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
            }
        }
        if json_out {
            self.write_manifest(&m)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Binary provenance: code signature and a match against the upstream release
// ---------------------------------------------------------------------------------------------

/// First `x.y.z` in `s`.
fn semver_in(s: &str) -> Option<String> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|t| t.trim_matches('.'))
        .find(|t| {
            t.split('.').count() == 3
                && t.split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
}

/// What `codesign -dv --verbose=2` printed -> the first Developer ID authority, `adhoc`, or `none`.
fn parse_codesign(output: &str) -> String {
    if output.contains("not signed at all") {
        return "none".into();
    }
    if let Some(a) = output.lines().find_map(|l| l.strip_prefix("Authority=")) {
        return a.to_string();
    }
    if output.lines().any(|l| l.trim() == "Signature=adhoc") {
        return "adhoc".into();
    }
    "none".into()
}

fn codesign_of(p: &Path) -> String {
    Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(p)
        .output()
        .map(|o| parse_codesign(&String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_else(|_| "unverified".into())
}

fn curl_bytes(url: &str) -> std::result::Result<Vec<u8>, String> {
    let o = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(format!("curl {url}: {}", o.status))
    }
}

fn curl_json(url: &str) -> std::result::Result<Value, String> {
    serde_json::from_slice(&curl_bytes(url)?).map_err(|e| format!("{url}: not JSON: {e}"))
}

/// Provenance of an installed agent binary: `{ codesign_authority, upstream: { kind, ref, matched } }`,
/// with `upstream.kind == "unverified"` (and a reason) when it cannot be computed.
fn provenance(agent: &str, bin: &Path, version_line: &str) -> Value {
    let mut sig = codesign_of(bin);
    let mut signed_path = bin.to_path_buf();
    // A nix wrapper script carries no signature; the real binary behind it does.
    if sig == "none"
        && bin.starts_with("/nix/store")
        && let Some(real) = nix_real_binary(bin, agent, version_line)
    {
        sig = codesign_of(&real);
        signed_path = real;
    }
    let upstream = match agent {
        "claude" => upstream_claude(bin, version_line),
        "codex" => upstream_codex(bin, version_line),
        "pi" | "copilot" => upstream_npm_nix(bin, agent, version_line),
        _ => Err("no upstream check for this agent".into()),
    }
    .unwrap_or_else(
        |reason| json!({ "kind": "unverified", "ref": null, "matched": null, "reason": reason }),
    );
    let mut v = json!({ "codesign_authority": sig, "upstream": upstream });
    if signed_path != bin {
        v["codesign_path"] = json!(signed_path.to_string_lossy());
    }
    v
}

/// Store paths the binary depends on (`nix-store -qR` of its store root).
fn nix_requisites(bin: &Path) -> Vec<String> {
    let root: String = bin
        .to_string_lossy()
        .split('/')
        .take(4)
        .collect::<Vec<_>>()
        .join("/");
    Command::new("nix-store")
        .args(["-qR", &root])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The dependency that IS the package (store name ends `-<version>`, has `bin/<agent>`).
fn nix_package_dir(bin: &Path, agent: &str, version_line: &str) -> Option<String> {
    let ver = semver_in(version_line)?;
    nix_requisites(bin)
        .into_iter()
        .find(|p| p.ends_with(&format!("-{ver}")) && Path::new(p).join("bin").join(agent).exists())
        .or_else(|| {
            nix_requisites(bin)
                .into_iter()
                .find(|p| p.ends_with(&format!("-{ver}")))
        })
}

fn nix_real_binary(bin: &Path, agent: &str, version_line: &str) -> Option<PathBuf> {
    let dir = nix_package_dir(bin, agent, version_line)?;
    let p = Path::new(&dir).join("bin").join(agent);
    p.exists().then_some(p)
}

/// pi / copilot from nix: the package's fixed-output tarball hash must equal the npm registry's
/// `dist.integrity` for the package.json name@version found inside the installed package.
fn upstream_npm_nix(
    bin: &Path,
    agent: &str,
    version_line: &str,
) -> std::result::Result<Value, String> {
    if !bin.starts_with("/nix/store") {
        return Err("not a nix store path".into());
    }
    let ver = semver_in(version_line).ok_or("no version")?;
    let pkg = nix_package_dir(bin, agent, version_line).ok_or("package store path not found")?;
    let drv = Command::new("nix-store")
        .args(["-q", "--deriver", &pkg])
        .output()
        .map_err(|e| e.to_string())?;
    let drv = String::from_utf8_lossy(&drv.stdout).trim().to_string();
    let show = |d: &str| -> std::result::Result<Value, String> {
        let o = Command::new("nix")
            .args(["derivation", "show", d])
            .output()
            .map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&o.stdout).map_err(|e| e.to_string())?;
        v["derivations"]
            .as_object()
            .and_then(|m| m.values().next().cloned())
            .ok_or_else(|| "no derivation".to_string())
    };
    let d = show(&drv)?;
    let tgz_drv = d["inputs"]["drvs"]
        .as_object()
        .and_then(|m| m.keys().find(|k| k.ends_with(".tgz.drv")))
        .ok_or("no .tgz fixed-output input")?;
    let fod = show(&format!("/nix/store/{tgz_drv}"))?;
    let nix_hash = fod["outputs"]["out"]["hash"]
        .as_str()
        .ok_or("fixed-output hash missing")?
        .to_string();
    // name@version from the installed package's own package.json
    // nix lays the package out as lib/<dir>/package.json (lib/pi, lib/github-copilot-cli)
    let name = fs::read_dir(Path::new(&pkg).join("lib"))
        .into_iter()
        .flatten()
        .flatten()
        .find_map(|d| {
            let v: Value =
                serde_json::from_slice(&fs::read(d.path().join("package.json")).ok()?).ok()?;
            (v["version"].as_str() == Some(ver.as_str()))
                .then(|| v["name"].as_str().map(str::to_string))?
        })
        .ok_or("package.json name not found under lib/*/")?;
    let url = format!("https://registry.npmjs.org/{name}/{ver}");
    let reg = curl_json(&url)?;
    let integrity = reg["dist"]["integrity"]
        .as_str()
        .ok_or("registry integrity missing")?;
    // nix may pin the tarball with a different algorithm than npm publishes (copilot: sha256 vs
    // npm's sha512), so when the SRI strings differ, hash the registry's own tarball the way nix did.
    let matched = if integrity == nix_hash {
        true
    } else {
        let tarball = reg["dist"]["tarball"]
            .as_str()
            .ok_or("registry tarball url missing")?;
        let bytes = curl_bytes(tarball)?;
        sri(&nix_hash, &bytes).is_some_and(|s| s == nix_hash)
    };
    Ok(json!({ "kind": "npm-integrity", "ref": url, "matched": matched }))
}

/// `<algo>-<base64 digest>` of `bytes` using the algorithm named in `like` (sha256 or sha512).
fn sri(like: &str, bytes: &[u8]) -> Option<String> {
    use sha2::Sha512;
    let algo = like.split('-').next()?;
    let digest: Vec<u8> = match algo {
        "sha256" => Sha256::digest(bytes).to_vec(),
        "sha512" => Sha512::digest(bytes).to_vec(),
        _ => return None,
    };
    Some(format!("{algo}-{}", base64_std(&digest)))
}

fn base64_std(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// codex: the GitHub release asset's published digest must match the downloaded tarball, and the
/// binary inside must equal the installed one.
fn upstream_codex(bin: &Path, version_line: &str) -> std::result::Result<Value, String> {
    let ver = semver_in(version_line).ok_or("no version")?;
    if !cfg!(target_os = "macos") {
        return Err("codex upstream check implemented for macOS only".into());
    }
    let triple = format!("{}-apple-darwin", std::env::consts::ARCH);
    let rel = curl_json(&format!(
        "https://api.github.com/repos/openai/codex/releases/tags/rust-v{ver}"
    ))?;
    let asset_name = format!("codex-{triple}.tar.gz");
    let asset = rel["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == asset_name.as_str()))
        .ok_or_else(|| format!("release asset {asset_name} not found"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or("no download url")?;
    let digest = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or("release asset has no sha256 digest")?;
    let tar = curl_bytes(url)?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let tgz = tmp.path().join("a.tar.gz");
    fs::write(&tgz, &tar).map_err(|e| e.to_string())?;
    let digest_ok = sha256_hex(&tar) == digest;
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(tmp.path())
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("tar extraction failed".into());
    }
    let mut files = Vec::new();
    files_under(
        tmp.path(),
        &|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("codex"))
                && p.extension().is_none()
        },
        &mut files,
    );
    let ours = hash_file(bin).ok_or("cannot hash installed binary")?;
    let same = files
        .iter()
        .any(|f| hash_file(f).as_deref() == Some(ours.as_str()));
    Ok(json!({ "kind": "github-release-digest", "ref": url, "matched": digest_ok && same }))
}

/// claude: the release manifest's darwin checksum for this version. The manifest base URL is not
/// public API knowledge this tool can guess, so it comes from $CLAUDE_RELEASE_BASE_URL
/// (`<base>/<version>/manifest.json`).
fn upstream_claude(bin: &Path, version_line: &str) -> std::result::Result<Value, String> {
    let ver = semver_in(version_line).ok_or("no version")?;
    let base = std::env::var("CLAUDE_RELEASE_BASE_URL").map_err(|_| {
        "release manifest base URL not configured (set CLAUDE_RELEASE_BASE_URL)".to_string()
    })?;
    let url = format!("{}/{ver}/manifest.json", base.trim_end_matches('/'));
    let m = curl_json(&url)?;
    let plat = format!(
        "darwin-{}",
        if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x64"
        }
    );
    let want = m["platforms"][plat.as_str()]["checksum"]
        .as_str()
        .ok_or_else(|| format!("manifest has no platforms.{plat}.checksum"))?;
    let ours = hash_file(bin).ok_or("cannot hash installed binary")?;
    Ok(json!({ "kind": "release-manifest", "ref": url, "matched": want == ours }))
}

fn now_utc() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn hash_file(p: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            return Some(hex::encode(h.finalize()));
        }
        h.update(&buf[..n]);
    }
}

/// The resolved executable named `name` on PATH (a real file, symlinks followed), if any.
fn agent_binary(name: &str) -> Option<PathBuf> {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find_map(|p| fs::canonicalize(&p).ok().filter(|r| r.is_file()))
}

fn agent_version(bin: &Path) -> String {
    Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The agent version an input records about itself, where its format has one (claude entries carry
/// `version`, codex `session_meta.cli_version`, copilot `session.start.copilotVersion`; pi's header
/// `version` is the session-format revision, not the agent's).
fn declared_version(harness: Harness, input: &[u8]) -> Option<String> {
    String::from_utf8_lossy(input)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            let s = match harness {
                Harness::Claude => v.get("version")?.as_str()?,
                Harness::Codex => v["payload"].get("cli_version")?.as_str()?,
                Harness::Copilot => v["data"].get("copilotVersion")?.as_str()?,
                Harness::Pi => return None,
            };
            Some(s.to_string())
        })
}

/// Claude Code's own Co-Authored-By attribution constant, injected into the transcript inside a
/// system-reminder. It is not the user's address; it is the ONLY address the leak check allows.
const ATTRIBUTION_ADDRESS: &str = "noreply@anthropic.com";

/// Every email-address-shaped word in `text`.
fn emails(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || "@._%+-".contains(c)))
        .filter(|w| {
            w.split_once('@').is_some_and(|(l, d)| {
                !l.is_empty()
                    && d.contains('.')
                    && !d.starts_with('.')
                    && d.rsplit('.').next().is_some_and(|tld| {
                        tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
                    })
            })
        })
        .map(str::to_string)
        .collect()
}

/// `word` appearing as a whole word (not inside a longer alphanumeric run).
fn has_word(text: &str, word: &str) -> bool {
    !word.is_empty()
        && text
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == word)
}

/// The local username, for the leak check (`id -un`).
fn local_username() -> Option<String> {
    Command::new("id")
        .arg("-un")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|u| !u.is_empty())
}

/// An agent whose live headless run `capture-live` can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LiveHarness {
    Claude,
    Codex,
    Pi,
    Copilot,
    /// Not supported: cursor-agent keeps its transcript in a protobuf store the adapter does not parse.
    Cursor,
    /// Not supported: opencode keeps its transcript in SQLite; a golden input must be a single file.
    Opencode,
}

/// How one agent is driven headlessly and where its transcript lands in the temp HOME.
struct Driver {
    /// The source harness whose adapter reads the transcript.
    harness: Harness,
    exe: &'static str,
    /// Credential FILES copied (never read for content, never printed) from the real HOME into the
    /// temp HOME: (path relative to the real HOME, path relative to the temp HOME).
    seed_files: &'static [(&'static str, &'static str)],
    /// Credential environment variables, first one present wins; forwarded under its own name.
    env_creds: &'static [&'static str],
    /// Hint shown when no credential is found.
    hint: &'static str,
}

fn driver(h: LiveHarness) -> Result<Driver> {
    Ok(match h {
        LiveHarness::Claude => Driver {
            harness: Harness::Claude,
            exe: "claude",
            seed_files: &[],
            env_creds: &[], // handled by read_credential/normalize_credential
            hint: "",
        },
        LiveHarness::Codex => Driver {
            harness: Harness::Codex,
            exe: "codex",
            seed_files: &[
                (".codex/auth.json", ".codex/auth.json"),
                (".codex/config.toml", ".codex/config.toml"),
            ],
            env_creds: &[],
            hint: "log in with `codex login`; ~/.codex/auth.json and config.toml are copied into the temp CODEX_HOME",
        },
        LiveHarness::Pi => Driver {
            harness: Harness::Pi,
            exe: "pi",
            seed_files: &[(".pi/agent/auth.json", ".pi/agent/auth.json")],
            env_creds: &[],
            hint: "log in with pi; ~/.pi/agent/auth.json is copied into the temp HOME",
        },
        LiveHarness::Copilot => Driver {
            harness: Harness::Copilot,
            exe: "copilot",
            seed_files: &[],
            env_creds: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
            hint: "copilot has no config file here and uses the gh login; export COPILOT_GITHUB_TOKEN (for example from `gh auth token`) in your own shell",
        },
        LiveHarness::Cursor => bail!(
            "cursor-agent has a non-interactive mode (`-p --output-format ...`, CURSOR_API_KEY), but it stores its transcript in a protobuf store that the Cursor adapter does not parse (it reads the IDE's state.vscdb), so there is no adapter-readable transcript to capture"
        ),
        LiveHarness::Opencode => bail!(
            "opencode has a non-interactive mode (`opencode run`), but it stores its session in SQLite under XDG_DATA_HOME; a golden input is a single file, and the CLI has no JSON-file source to replay one (the test fixture is an export parsed by test code only)"
        ),
    })
}

/// The command line for a driver, with the prompt as given.
fn live_argv(h: LiveHarness, model: Option<&str>, proj: &Path, prompt: &str) -> Vec<String> {
    let s = |x: &str| x.to_string();
    let mut v = match h {
        LiveHarness::Claude => vec![
            s("-p"),
            s(prompt),
            s("--model"),
            s(model.unwrap_or("claude-haiku-4-5-20251001")),
            s("--output-format"),
            s("json"),
            s("--allowedTools"),
        ],
        LiveHarness::Codex => {
            let mut v = vec![
                s("exec"),
                s("--skip-git-repo-check"),
                s("--sandbox"),
                s("read-only"),
                s("-C"),
                proj.to_string_lossy().into_owned(),
            ];
            if let Some(m) = model {
                v.extend([s("-m"), s(m)]);
            }
            v.push(s(prompt));
            v
        }
        LiveHarness::Pi => {
            let mut v = vec![s("-p"), s(prompt)];
            if let Some(m) = model {
                v.extend([s("--model"), s(m)]);
            }
            v
        }
        LiveHarness::Copilot => {
            let mut v = vec![s("-p"), s(prompt), s("--allow-all-tools")];
            if let Some(m) = model {
                v.extend([s("--model"), s(m)]);
            }
            v
        }
        LiveHarness::Cursor | LiveHarness::Opencode => Vec::new(),
    };
    if h == LiveHarness::Claude {
        v.extend(CAPTURE_TOOLS.iter().map(|t| t.to_string()));
    }
    v
}

/// Every file under `dir` (recursive) satisfying `keep`.
fn files_under(dir: &Path, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, keep, out);
        } else if keep(&p) {
            out.push(p);
        }
    }
}

/// Where the adapter looks for this agent's transcript inside the temp HOME.
fn live_transcripts(h: LiveHarness, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let ext = |p: &Path, e: &str| p.extension().is_some_and(|x| x == e);
    match h {
        LiveHarness::Claude => files_under(
            &home.join(".claude/projects"),
            &|p| ext(p, "jsonl"),
            &mut out,
        ),
        LiveHarness::Codex => files_under(
            &home.join(".codex/sessions"),
            &|p| {
                ext(p, "jsonl")
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            },
            &mut out,
        ),
        LiveHarness::Pi => files_under(
            &home.join(".pi/agent/sessions"),
            &|p| ext(p, "jsonl"),
            &mut out,
        ),
        LiveHarness::Copilot => files_under(
            &home.join(".copilot/session-state"),
            &|p| p.file_name().is_some_and(|n| n == "events.jsonl"),
            &mut out,
        ),
        LiveHarness::Cursor | LiveHarness::Opencode => {}
    }
    out
}

/// An account identifier (account id, user id, email-shaped account field): not a bearer secret, but
/// identity. It is REDACTED to a stable placeholder in a captured transcript, never refused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identifier {
    field: String,
    value: String,
}

/// Field names that hold account identity (compared lower-cased, `_` and `-` removed).
fn is_identifier_key(key: &str) -> bool {
    let k: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_lowercase();
    matches!(
        k.as_str(),
        "accountid"
            | "userid"
            | "creatoraccountid"
            | "creatoruserid"
            | "organizationid"
            | "orgid"
            | "email"
            | "useremail"
            | "accountemail"
    )
}

/// The stable placeholder for an identifier field.
fn placeholder_for(field: &str) -> &'static str {
    let f = field.to_lowercase();
    if f.contains("email") {
        "email-redacted"
    } else if f.contains("user") {
        "user-redacted"
    } else {
        "acct-redacted"
    }
}

/// Split a credential file into secrets (string values >= 20 chars, refused if they reach a
/// transcript) and account identifiers (values under identifier-named keys, redacted instead). A
/// non-JSON file (e.g. TOML) yields only secrets. Values are never printed.
fn split_credentials(file_text: &str) -> (Vec<String>, Vec<Identifier>) {
    fn walk(key: &str, v: &Value, secrets: &mut Vec<String>, ids: &mut Vec<Identifier>) {
        match v {
            Value::String(s) if is_identifier_key(key) && s.len() >= 8 => ids.push(Identifier {
                field: key.to_string(),
                value: s.clone(),
            }),
            Value::String(s) if s.len() >= 20 => secrets.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| walk(key, x, secrets, ids)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(k, x, secrets, ids)),
            _ => {}
        }
    }
    let (mut secrets, mut ids) = (Vec::new(), Vec::new());
    match serde_json::from_str::<Value>(file_text) {
        Ok(v) => walk("", &v, &mut secrets, &mut ids),
        Err(_) => {
            // Not JSON (e.g. TOML): any quoted value or the whole trimmed line counts.
            for line in file_text.lines() {
                if let Some((_, v)) = line.split_once('=') {
                    let v = v.trim().trim_matches('"');
                    if v.len() >= 20 {
                        secrets.push(v.to_string());
                    }
                }
            }
        }
    }
    (secrets, ids)
}

/// Identifier fields found in the transcript itself (claude `userID`, copilot/pi account ids,
/// codex `creator_account_id`, ...): (field name, value >= 8 chars).
fn transcript_identifiers(text: &str) -> Vec<Identifier> {
    fn walk(key: &str, v: &Value, out: &mut Vec<Identifier>) {
        match v {
            Value::String(s) if is_identifier_key(key) && s.len() >= 8 => out.push(Identifier {
                field: key.to_string(),
                value: s.clone(),
            }),
            Value::Array(a) => a.iter().for_each(|x| walk(key, x, out)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(k, x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            walk("", &v, &mut out);
        }
    }
    out
}

/// Replace every occurrence of each identifier value with its stable placeholder. Returns the new
/// text and, per field name actually redacted, the placeholder and occurrence count (no values).
fn redact_identifiers(text: &str, ids: &[Identifier]) -> (String, Vec<Value>) {
    let mut uniq: Vec<&Identifier> = Vec::new();
    for i in ids {
        if !uniq.iter().any(|u| u.value == i.value) {
            uniq.push(i);
        }
    }
    uniq.sort_by_key(|i| std::cmp::Reverse(i.value.len()));
    let mut out = text.to_string();
    let mut report: Vec<Value> = Vec::new();
    for i in uniq {
        let n = out.matches(i.value.as_str()).count();
        if n == 0 {
            continue;
        }
        out = out.replace(i.value.as_str(), placeholder_for(&i.field));
        match report.iter_mut().find(|r| r["field"] == i.field.as_str()) {
            Some(r) => r["occurrences"] = json!(r["occurrences"].as_u64().unwrap_or(0) + n as u64),
            None => report.push(json!({ "field": i.field, "placeholder": placeholder_for(&i.field), "occurrences": n })),
        }
    }
    (out, report)
}

fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .fold(text.to_string(), |t, s| t.replace(s.as_str(), "<redacted>"))
}

/// Refusals for a transcript headed for the repo; also reports whether the one allowed address
/// (Claude Code's attribution constant) was present.
fn leak_check(text: &str, secrets: &[String]) -> (Vec<&'static str>, bool) {
    let mut leaks = Vec::new();
    if let Ok(h) = std::env::var("HOME")
        && h.len() > 1
        && text.contains(&h)
    {
        leaks.push("the real home directory");
    }
    if secrets.iter().any(|s| text.contains(s.as_str())) {
        leaks.push("a credential");
    }
    // Exactly one address is allowed: Claude Code's attribution constant. Any other is a leak.
    let found = emails(text);
    let attribution_present = found.iter().any(|e| e == ATTRIBUTION_ADDRESS);
    if found.iter().any(|e| e != ATTRIBUTION_ADDRESS) {
        leaks.push("an email address other than the attribution address");
    }
    if let Some(u) = local_username()
        && has_word(text, &u)
    {
        leaks.push("the local username");
    }
    (leaks, attribution_present)
}

impl Goldens {
    /// One REAL headless run of `which`, hermetic, then captured as a golden set.
    fn capture_live(&self, which: LiveHarness, a: CaptureLiveArgs) -> Result<()> {
        let drv = driver(which)?;
        let name = a
            .name
            .clone()
            .unwrap_or_else(|| format!("{}-session", drv.exe));
        let say = |m: &str| eprintln!("capture-live {}: {m}", drv.exe);
        let bin = resolve_agent(drv.exe, a.bin)?;
        say(&format!("binary: {}", bin.display()));

        // Credentials: only ever copied or forwarded into the child, and removed with the temp dir.
        let mut secrets: Vec<String> = Vec::new();
        let mut child_env: Vec<(String, String)> = Vec::new();
        let mut cred_ids: Vec<Identifier> = Vec::new();
        let mut cred_note = String::new();
        if which == LiveHarness::Claude {
            let (source, raw) = read_credential()?;
            let (kind, token) =
                normalize_credential(&raw).with_context(|| format!("credential from {source}"))?;
            cred_note = format!("{source}, kind: {}", kind.var());
            secrets.push(token.clone());
            child_env.push((kind.var().to_string(), token));
        }
        if let Some(var) = drv
            .env_creds
            .iter()
            .find(|v| std::env::var(v).is_ok_and(|x| !x.trim().is_empty()))
        {
            let v = std::env::var(var)?;
            cred_note = format!("${var}");
            secrets.push(v.trim().to_string());
            child_env.push((var.to_string(), v.trim().to_string()));
        } else if !drv.env_creds.is_empty() {
            bail!("no credential: {}", drv.hint);
        }
        let real_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        for (from, _) in drv.seed_files {
            if !real_home.join(from).is_file() {
                bail!("no credential file ~/{from}: {}", drv.hint);
            }
            cred_note = format!(
                "{cred_note}{}~/{from}",
                if cred_note.is_empty() { "" } else { ", " }
            );
        }
        say(&format!(
            "credential source: {cred_note} (values not shown)"
        ));

        let tmp = tempfile::tempdir()?;
        let base = tmp.path().canonicalize()?;
        let (home, proj) = (base.join("home"), base.join("project"));
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&proj)?;
        fs::write(
            proj.join("notes.txt"),
            "first line of the notes\nsecond line\n",
        )?;
        fs::write(proj.join("other.txt"), "unrelated\n")?;
        for (from, to) in drv.seed_files {
            let dest = home.join(to);
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(real_home.join(from), &dest)?;
            if let Ok(text) = fs::read_to_string(&dest) {
                let (s, ids) = split_credentials(&text);
                secrets.extend(s);
                cred_ids.extend(ids);
            }
        }
        say(&format!("temp dir: {}", base.display()));

        let keep = |tmp: tempfile::TempDir| {
            let p = tmp.keep();
            eprintln!(
                "capture-live {}: FAILED; temp dir kept for inspection (it holds a copy of any credential file; delete it): {}",
                drv.exe,
                p.display()
            );
        };
        let argv = live_argv(which, a.model.as_deref(), &proj, CAPTURE_PROMPT);
        say(&format!("running {} headless", drv.exe));
        let out_file = base.join("stdout.txt");
        let err_file = base.join("stderr.txt");
        let mut cmd = Command::new(&bin);
        cmd.current_dir(&proj)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin",
                    bin.parent().unwrap_or(Path::new("/")).display()
                ),
            )
            .env("HOME", &home)
            .env("TMPDIR", &base)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("CODEX_HOME", home.join(".codex"))
            .env("COPILOT_HOME", home.join(".copilot"))
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(fs::File::create(&out_file)?)
            .stderr(fs::File::create(&err_file)?);
        for (k, v) in &child_env {
            cmd.env(k, v);
        }
        let status = cmd.status()?;
        if !status.success() {
            // Show a short, redacted tail; `claude -p` reports its error as JSON on stdout.
            let tail = |p: &Path| {
                let t = fs::read_to_string(p).unwrap_or_default();
                redact(
                    t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(""),
                    &secrets,
                )
                .chars()
                .take(300)
                .collect::<String>()
            };
            say(&format!(
                "{} exited with {status}: stdout: {} | stderr: {}",
                drv.exe,
                tail(&out_file),
                tail(&err_file)
            ));
            keep(tmp);
            bail!("{} failed", drv.exe);
        }
        let transcripts = live_transcripts(which, &home);
        if transcripts.len() != 1 {
            say(&format!(
                "expected exactly one transcript, found {}",
                transcripts.len()
            ));
            keep(tmp);
            bail!("transcript count");
        }
        let src = &transcripts[0];
        let raw = fs::read(src)?;
        let raw_text = String::from_utf8_lossy(&raw).into_owned();
        // Account identifiers (from the seeded credential files and from the transcript's own
        // identity fields) are redacted to stable placeholders, never refused and never kept.
        let mut ids = cred_ids.clone();
        ids.extend(transcript_identifiers(&raw_text));
        let (text, redacted) = redact_identifiers(&raw_text, &ids);
        let bytes = text.clone().into_bytes();
        say(&format!(
            "transcript found ({} lines); redacted {} identifier field(s); leak check",
            text.lines().count(),
            redacted.len()
        ));
        let (leaks, attribution_present) = leak_check(&text, &secrets);
        if !leaks.is_empty() {
            say(&format!(
                "LEAK: transcript contains {}; refusing to write anything",
                leaks.join(", ")
            ));
            keep(tmp);
            bail!("leak check failed");
        }
        let out = self.dir().join(&name);
        write(&out.join("input.jsonl"), &bytes)?;
        let version = agent_version(&bin);
        let shown: Vec<String> = argv
            .iter()
            .map(|x| {
                if x == CAPTURE_PROMPT {
                    "<prompt>".into()
                } else {
                    x.replace(&proj.to_string_lossy().into_owned(), "<tmp>/project")
                }
            })
            .collect();
        let meta = json!({
            "captured_at": now_utc(),
            "agent": drv.exe,
            "input_sha256": sha256_hex(&bytes),
            "agent_version": version,
            "command": format!("cd <tmp>/project && env -i HOME=<tmp>/home XDG_*=<tmp> CLAUDE_CONFIG_DIR/CODEX_HOME/COPILOT_HOME=<tmp>/home/... <credentials redacted> {} {} < /dev/null", drv.exe, shown.join(" ")),
            "prompt": CAPTURE_PROMPT,
        });
        write(
            &out.join("capture.json"),
            (serde_json::to_string_pretty(&meta)? + "\n").as_bytes(),
        )?;
        say(&format!(
            "written: goldens/{name}/input.jsonl; capturing the golden set"
        ));
        let pin = json!({
            "name": drv.exe,
            "version": version,
            "binary_path": bin.to_string_lossy(),
            "binary_sha256": hash_file(&bin).unwrap_or_default(),
            "captured_at": now_utc(),
            "provenance": provenance(drv.exe, &bin, &version),
        });
        self.capture(
            drv.harness,
            &out.join("input.jsonl"),
            &name,
            None,
            Some(pin),
        )?;
        // Record which address the leak check let through, in the set's manifest entry.
        let mut m = self.read_manifest()?;
        if let Some(e) = m["goldens"]
            .as_array_mut()
            .and_then(|s| s.iter_mut().find(|e| e["name"] == name))
        {
            e["leak_check"] = json!({
                "refused": ["real home directory", "credential (token, key, or any string from a seeded credential file)", "local username", "email address other than the allowed one"],
                "allowed_addresses": [ATTRIBUTION_ADDRESS],
                "allowed_address_present": attribution_present,
                // Account identifiers replaced by stable placeholders before the input was written.
                "redacted": redacted,
                "reason": "noreply@anthropic.com is Claude Code's own Co-Authored-By attribution constant, injected in a system-reminder; it is not the user's address",
            });
        }
        self.write_manifest(&m)
    }
}

/// A real executable for `exe`: --bin, then $<EXE>_BIN, then PATH, then ~/.local/bin.
fn resolve_agent(exe: &str, explicit: Option<PathBuf>) -> Result<PathBuf> {
    let is_exe = |p: &Path| {
        p.is_file()
            && fs::metadata(p)
                .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0)
                .unwrap_or(false)
    };
    let env_name = format!("{}_BIN", exe.to_uppercase());
    let candidates: Vec<PathBuf> = explicit
        .into_iter()
        .chain(std::env::var_os(&env_name).map(PathBuf::from))
        .chain(
            std::env::var("PATH")
                .unwrap_or_default()
                .split(':')
                .map(|d| Path::new(d).join(exe)),
        )
        .chain(std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/bin").join(exe)))
        .collect();
    let found = candidates
        .iter()
        .find(|p| is_exe(p))
        .ok_or_else(|| anyhow!("no {exe} executable found (use --bin or {env_name})"))?;
    Ok(fs::canonicalize(found)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_plain_routes_by_prefix() {
        let (k, v) = normalize_credential("  sk-ant-oat01-abc\n").unwrap();
        assert_eq!(
            (k, v.as_str()),
            (CredentialKind::OauthToken, "sk-ant-oat01-abc")
        );
        let (k, _) = normalize_credential("sk-ant-api03-abc").unwrap();
        assert_eq!(k, CredentialKind::ApiKey);
    }

    #[test]
    fn credential_hex_with_encoded_newline_is_decoded() {
        let hexed = hex::encode("sk-ant-oat01-abc\n");
        let (k, v) = normalize_credential(&hexed).unwrap();
        assert_eq!(
            (k, v.as_str()),
            (CredentialKind::OauthToken, "sk-ant-oat01-abc")
        );
    }

    #[test]
    fn credential_bad_prefix_is_refused_without_the_value() {
        let err = normalize_credential("notatoken123")
            .unwrap_err()
            .to_string();
        assert!(err.contains("length 12") && !err.contains("notatoken"));
    }

    #[test]
    fn email_detection() {
        assert_eq!(
            emails("contact a.b+c@example.com now"),
            ["a.b+c@example.com"]
        );
        assert!(emails("user@host and @mention and a@b").is_empty());
        // Only the attribution address is allowed; anything else beside it still counts.
        let found = emails("Co-Authored-By: Claude <noreply@anthropic.com> and bobby@empathic.dev");
        assert!(found.iter().any(|e| e == ATTRIBUTION_ADDRESS));
        assert_eq!(
            found.iter().filter(|e| *e != ATTRIBUTION_ADDRESS).count(),
            1
        );
    }

    #[test]
    fn username_is_matched_as_a_whole_word() {
        assert!(has_word("-rw-r--r--  1 bobby  staff 12 notes.txt", "bobby"));
        assert!(!has_word("bobbysocks and abobby", "bobby"));
    }

    #[test]
    fn credentials_split_into_secrets_and_account_identifiers() {
        let (secrets, ids) = split_credentials(
            r#"{"tokens":{"access":"abcdefghijklmnopqrstuvwxyz","account_id":"acc-1234-5678"},"short":"x"}"#,
        );
        assert_eq!(secrets, ["abcdefghijklmnopqrstuvwxyz"]);
        assert_eq!(
            ids,
            [Identifier {
                field: "account_id".into(),
                value: "acc-1234-5678".into()
            }]
        );
        let (toml, toml_ids) =
            split_credentials("model = \"o3\"\napi_key = \"abcdefghijklmnopqrstuvwxyz\"\n");
        assert_eq!(
            (toml, toml_ids.len()),
            (vec!["abcdefghijklmnopqrstuvwxyz".to_string()], 0)
        );
    }

    #[test]
    fn identifiers_are_redacted_to_stable_placeholders_not_refused() {
        let text = "{\"creator_account_id\":\"acc-1234-5678\",\"creator_user_id\":\"usr-9999-0000\",\"note\":\"acc-1234-5678\"}\n";
        let mut ids = transcript_identifiers(text);
        ids.push(Identifier {
            field: "account_id".into(),
            value: "acc-1234-5678".into(),
        });
        let (out, report) = redact_identifiers(text, &ids);
        assert!(!out.contains("acc-1234") && !out.contains("usr-9999"));
        assert_eq!(out.matches("acct-redacted").count(), 2);
        assert!(out.contains("user-redacted"));
        assert_eq!(report.len(), 2);
        // A real secret still refuses.
        let (leaks, _) = leak_check(
            "bearer abcdefghijklmnopqrstuvwxyz",
            &["abcdefghijklmnopqrstuvwxyz".to_string()],
        );
        assert!(leaks.contains(&"a credential"));
    }

    #[test]
    fn leak_check_refuses_secrets_and_other_addresses_but_not_the_attribution_address() {
        let secrets = vec!["abcdefghijklmnopqrstuvwxyz".to_string()];
        let ok = "Co-Authored-By: Claude <noreply@anthropic.com> listing: notes.txt other.txt";
        let (leaks, attributed) = leak_check(ok, &secrets);
        assert!(attributed);
        // The username is the machine's own; only assert on what this text could trip.
        assert!(!leaks.contains(&"a credential") && !leaks.iter().any(|l| l.contains("email")));
        let (leaks, _) = leak_check("token abcdefghijklmnopqrstuvwxyz here", &secrets);
        assert!(leaks.contains(&"a credential"));
        let (leaks, _) = leak_check("mail someone@example.org", &secrets);
        assert!(leaks.iter().any(|l| l.contains("email")));
    }

    #[test]
    fn live_commands_keep_prompt_and_project_but_no_credentials() {
        let proj = Path::new("/tmp/p");
        let codex = live_argv(LiveHarness::Codex, None, proj, "PROMPT");
        assert_eq!(
            codex[..4],
            ["exec", "--skip-git-repo-check", "--sandbox", "read-only"]
        );
        assert!(codex.contains(&"/tmp/p".to_string()) && codex.last().unwrap() == "PROMPT");
        let claude = live_argv(LiveHarness::Claude, None, proj, "PROMPT");
        assert!(
            claude.contains(&"claude-haiku-4-5-20251001".to_string())
                && claude.contains(&"Bash(ls)".to_string())
        );
        assert!(driver(LiveHarness::Cursor).is_err() && driver(LiveHarness::Opencode).is_err());
    }

    #[test]
    fn roundtrip_row_counts_losses_and_flags_only_new_ones() {
        let doc = |extra: Value| {
            json!({ "paths": [{ "steps": [{ "step": {"id": "s"}, "change": { "claude-code://sess-1": { "structural": {
                "type": "conversation.append", "text": "hello", "tool_uses": [{"id": "t"}], "extra": extra } } } }] }] })
        };
        let ir1 = doc(json!({"a": 1, "b": 2}));
        let ir2 = doc(json!({"a": 1}));
        let row = roundtrip_row("s", "x", &ir1, &ir2);
        // the artifact key (which embeds a session id) is collapsed, so only real structure differs
        assert_eq!(
            row["lost"],
            json!(["paths[].steps[].change.<artifact>.structural.extra.b"])
        );
        assert_eq!(
            (
                row["ir1"]["text_chars"].as_u64(),
                row["ir2"]["tool_uses"].as_u64()
            ),
            (Some(5), Some(1))
        );
        assert!(roundtrip_regressions("s", &row, &row).is_empty());
        // a NEW loss against the committed row is a problem; the same loss is not
        let worse = roundtrip_row("s", "x", &ir1, &doc(json!({})));
        let msgs = roundtrip_regressions("s", &row, &worse);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("did not before"));
        // a pair that cannot round-trip is pinned by its error text
        let err = json!({"error": "no identity"});
        assert!(roundtrip_regressions("s", &err, &err).is_empty());
        assert!(!roundtrip_regressions("s", &err, &row).is_empty());
    }

    #[test]
    fn golden_factory_slices_and_mutates_captured_bytes() {
        let g = golden("claude");
        assert_eq!(g.name(), "claude");
        assert_eq!(g.input().lines().count(), g.events().len());
        // a prefix of the real session, and one field changed on one line of it
        assert_eq!(g.slice(0..3).lines().count(), 3);
        let mutated = g.with_field(0, "marker", json!("changed"));
        assert_eq!(mutated.lines().count(), g.input().lines().count());
        assert!(
            mutated
                .lines()
                .next()
                .unwrap()
                .contains("\"marker\":\"changed\"")
        );
        // identity fields come from the capture, not from a literal in the test
        assert!(g.first_string("sessionId").is_some() && g.first_string("cwd").is_some());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_std(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_std(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn canonicalize_sorts_keys_and_masks_the_session_id() {
        let raw = br#"{"z":1,"data":{"sessionId":"abc-1"},"a":"abc-1"}"#;
        assert_eq!(
            String::from_utf8(canonicalize(raw)).unwrap(),
            "{\"a\":\"<session-id>\",\"data\":{\"sessionId\":\"<session-id>\"},\"z\":1}\n"
        );
    }
}
