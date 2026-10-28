//! `path sync`: the periodic job. A bare `path sync` runs one pass, the
//! bulk upload `path share --all` runs, over the scope in `[sync]`;
//! `install` writes `[sync]` and a launchd agent or systemd user timer
//! that runs it, `uninstall` removes the timer, `status` reports on
//! both. See `docs/superpowers/specs/2026-10-13-path-sync-client-design.md`,
//! "Periodic job".

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::{
    CONFIG_FILE_NAME, CREDENTIALS_FILE_NAME, Config, SYNC_LOG_FILE_NAME, SYNC_STATUS_FILE_NAME,
};
use crate::harness::{Harness, HarnessBundle};
use crate::share_all::{self, Bulk, RecordScope, Report, Resolve};
use crate::share_config::ProjectRules;
use crate::sync_config::{InstallSettings, SyncConfig, parse_interval};
use crate::sync_service::{Job, Scheduler, installed_interval};

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SyncArgs {
    #[command(subcommand)]
    pub op: Option<SyncOp>,

    /// Print a line per session, not only the summary
    #[arg(long, short)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
pub enum SyncOp {
    /// Write [sync] to config.toml and schedule `path sync` with launchd
    /// (macOS) or a systemd user timer (Linux)
    Install(InstallArgs),
    /// Remove the scheduled job; config.toml and upload records stay
    Uninstall,
    /// Show whether the job is installed, its interval and scope, and
    /// how the last pass went
    Status,
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// How often to run a pass: 90s, 15m, 1h [default: the configured
    /// interval, else 15m]
    #[arg(long, value_name = "DURATION")]
    pub interval: Option<String>,

    /// Upload sessions whose project directory is under this path
    /// (repeatable); replaces the configured directories
    #[arg(long, value_name = "DIR", conflicts_with = "all")]
    pub project_under: Vec<PathBuf>,

    /// Upload every session on this machine
    #[arg(long)]
    pub all: bool,

    /// Only this harness (repeatable); replaces the configured list
    #[arg(long, value_enum)]
    pub harness: Vec<Harness>,

    /// Replace a job that is already installed
    #[arg(long)]
    pub force: bool,

    /// Write the config and the job's files but do not load the job
    #[arg(long)]
    pub no_load: bool,
}

pub fn run(args: SyncArgs, config: &Config) -> Result<()> {
    match args.op {
        None => one_pass(config, args.verbose),
        Some(SyncOp::Install(install_args)) => install(config, install_args),
        Some(SyncOp::Uninstall) => uninstall(config),
        Some(SyncOp::Status) => status(config),
    }
}

// ── the pass ────────────────────────────────────────────────────────

/// What the last pass did, kept in `sync-status.json` for `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PassStatus {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) finished_at: DateTime<Utc>,
    pub(crate) ok: bool,
    pub(crate) summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) failures: Vec<String>,
}

/// Who a pass uploads as.
pub(crate) struct Login {
    pub(crate) base_url: String,
    pub(crate) token: String,
    pub(crate) username: String,
}

/// A pass that ran, or found another holding the upload lock.
pub(crate) enum Pass {
    Ran(Report),
    Busy,
}

/// Run `path sync` once and exit: the summary on stdout, and a non-zero
/// exit after one line on stderr when the pass could not run or a
/// session failed.
fn one_pass(config: &Config, verbose: bool) -> Result<()> {
    let started_at = Utc::now();
    let config_dir = config.config_dir()?;
    trim_log(&config_dir);
    let outcome = login(&config_dir).and_then(|login| {
        let bundle = crate::providers::harness_bundle(config);
        pass(
            &config_dir,
            config.home_dir().map(PathBuf::as_path),
            &bundle,
            &login,
            verbose,
        )
    });
    let (status, code) = match outcome {
        Ok(Pass::Busy) => {
            println!("path sync: another upload pass is running; skipped");
            return Ok(());
        }
        Ok(Pass::Ran(report)) => {
            let failures = failure_lines(&report);
            let ok = failures.is_empty();
            let summary = report.summary_line();
            println!("path sync: {summary}");
            for line in &failures {
                eprintln!("  failed: {line}");
            }
            let status = PassStatus {
                started_at,
                finished_at: Utc::now(),
                ok,
                summary,
                failures,
            };
            (status, if ok { 0 } else { 1 })
        }
        Err(e) => {
            let summary = format!("{e:#}");
            eprintln!("path sync: {summary}");
            let status = PassStatus {
                started_at,
                finished_at: Utc::now(),
                ok: false,
                summary,
                failures: Vec::new(),
            };
            (status, 1)
        }
    };
    if let Err(e) = write_status(&config_dir, &status) {
        eprintln!("path sync: could not record the pass: {e:#}");
    }
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// The stored login, checked against its server as `share --all`
/// checks it.
fn login(config_dir: &Path) -> Result<Login> {
    let stored = crate::cmd_pathbase::load_session(&config_dir.join(CREDENTIALS_FILE_NAME))?
        .context("not logged in; run `path auth login`")?;
    let user = crate::cmd_pathbase::api_me(&stored.url, &stored.token)
        .context("the stored login does not check out; run `path auth login`")?;
    Ok(Login {
        base_url: stored.url,
        token: stored.token,
        username: user.username,
    })
}

/// One pass over the scope `[sync]` configures under `config_dir`, as
/// `login`, through the bulk upload `path share --all` runs. Sessions
/// under a `sync = false` rule are left out.
pub(crate) fn pass(
    config_dir: &Path,
    home: Option<&Path>,
    bundle: &HarnessBundle,
    login: &Login,
    verbose: bool,
) -> Result<Pass> {
    let sync = SyncConfig::load(config_dir)?.with_context(|| {
        format!(
            "no [sync] in {}; run `path sync install`",
            config_dir.join(CONFIG_FILE_NAME).display()
        )
    })?;
    let scope = sync.scope(home)?;
    let rules = ProjectRules::load(config_dir, home)?;
    let Some(_lock) = crate::sync::lock::lock_uploads(config_dir, false)? else {
        return Ok(Pass::Busy);
    };
    let resolve = Resolve {
        config_dir,
        home,
        base_url: &login.base_url,
        token: &login.token,
        username: &login.username,
        url: None,
        repo: None,
        public: false,
    };
    let destination =
        share_all::ensuring_pathstash(&login.username, |dir| resolve.destination_for(dir));
    let bulk = Bulk {
        config_dir,
        bundle,
        types: &scope.harnesses,
        include: &scope.include,
        exclusions: Some(&rules),
        records: RecordScope::Any,
        verbose: verbose || sync.verbose,
    };
    let report = share_all::bulk(&bulk, &destination, &mut |_| Ok(true))?
        .expect("a pass that always proceeds reports");
    Ok(Pass::Ran(report))
}

fn failure_lines(report: &Report) -> Vec<String> {
    report
        .sessions
        .iter()
        .filter_map(|(t, id, dest, result)| match result {
            share_all::SessionResult::Failed(why) => {
                let dest = if dest.is_empty() {
                    String::new()
                } else {
                    format!(" → {dest}")
                };
                Some(format!("{} {id}{dest}: {why}", t.name()))
            }
            _ => None,
        })
        .collect()
}

/// The log the timer appends to grows without bound; start it over
/// once it passes this size.
const LOG_LIMIT: u64 = 1 << 20;

fn trim_log(config_dir: &Path) {
    let log = config_dir.join(SYNC_LOG_FILE_NAME);
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_LIMIT)
        && let Ok(file) = std::fs::OpenOptions::new().write(true).open(&log)
    {
        let _ = file.set_len(0);
    }
}

fn write_status(config_dir: &Path, status: &PassStatus) -> Result<()> {
    write_file(
        &config_dir.join(SYNC_STATUS_FILE_NAME),
        &serde_json::to_string_pretty(status)?,
    )
}

fn read_status(config_dir: &Path) -> Result<Option<PassStatus>> {
    let path = config_dir.join(SYNC_STATUS_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Write `text` to `path` through a sibling temporary file, so a reader
/// never sees half of it.
fn write_file(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("a file path has a parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

// ── install / uninstall / status ────────────────────────────────────

fn host_scheduler() -> Result<Scheduler> {
    Scheduler::host().context("`path sync install` supports macOS (launchd) and Linux (systemd)")
}

fn require_home(config: &Config) -> Result<&Path> {
    config
        .home_dir()
        .map(PathBuf::as_path)
        .context("$HOME is not set; cannot place the scheduled job")
}

fn install(config: &Config, args: InstallArgs) -> Result<()> {
    let scheduler = host_scheduler()?;
    let home = require_home(config)?;
    let config_dir = std::path::absolute(config.config_dir()?)?;
    let installed: Vec<PathBuf> = scheduler
        .files(home)
        .into_iter()
        .filter(|f| f.exists())
        .collect();
    if !installed.is_empty() && !args.force {
        bail!(
            "path sync is already installed ({}); pass --force to replace it",
            installed[0].display()
        );
    }

    let config_path = config_dir.join(CONFIG_FILE_NAME);
    let text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", config_path.display())),
    };
    let include = args
        .project_under
        .iter()
        .map(|dir| {
            let dir = std::path::absolute(dir)?;
            dir.to_str()
                .map(String::from)
                .with_context(|| format!("not a UTF-8 path: {}", dir.display()))
        })
        .collect::<Result<_>>()?;
    let settings = InstallSettings {
        all: args.all,
        include,
        harnesses: args.harness.iter().map(|h| h.name().to_string()).collect(),
        interval: args.interval,
    };
    let updated = crate::sync_config::install_config(&text, &settings)?;
    let sync = SyncConfig::parse(&updated, &config_path.display().to_string())?
        .expect("install writes [sync]");
    let binary = std::env::current_exe().context("locate the path binary")?;
    let job = Job {
        binary: &binary,
        config_dir: &config_dir,
        interval_secs: parse_interval(sync.interval())?,
    };
    let files = scheduler.render(home, &job)?;

    if !installed.is_empty() {
        unload(scheduler);
    }
    write_file(&config_path, &updated)?;
    for (path, content) in &files {
        write_file(path, content)?;
    }
    if !args.no_load {
        for command in scheduler.load_commands(home, uid()) {
            run_command(&command)?;
        }
    }

    let log = config_dir.join(SYNC_LOG_FILE_NAME);
    let loaded = if args.no_load { "Wrote" } else { "Installed" };
    println!(
        "{loaded} the {}: {}",
        scheduler.name(),
        files
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "Runs `{} sync` every {}; output goes to {}",
        binary.display(),
        sync.interval(),
        log.display()
    );
    println!("Scope: {}", describe_scope(&sync, Some(home)));
    if crate::cmd_pathbase::load_session(&config_dir.join(CREDENTIALS_FILE_NAME))?.is_none() {
        println!("Not logged in: each pass fails until you run `path auth login`");
    }
    Ok(())
}

fn uninstall(config: &Config) -> Result<()> {
    let scheduler = host_scheduler()?;
    let home = require_home(config)?;
    let installed: Vec<PathBuf> = scheduler
        .files(home)
        .into_iter()
        .filter(|f| f.exists())
        .collect();
    if installed.is_empty() {
        println!("path sync is not installed");
        return Ok(());
    }
    unload(scheduler);
    for path in &installed {
        std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    println!(
        "Removed the {}: {}",
        scheduler.name(),
        installed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("Kept [sync] in config.toml and the upload records");
    Ok(())
}

fn status(config: &Config) -> Result<()> {
    let config_dir = config.config_dir()?;
    let home = config.home_dir().map(PathBuf::as_path);
    match (Scheduler::host(), home) {
        (Some(scheduler), Some(home)) => {
            let files: Vec<PathBuf> = scheduler
                .files(home)
                .into_iter()
                .filter(|f| f.exists())
                .collect();
            if files.is_empty() {
                println!("installed: no");
            } else {
                println!("installed: {} ({})", scheduler.name(), files[0].display());
                if let Some(secs) = installed_interval(scheduler, home) {
                    println!("runs every: {secs}s");
                }
            }
        }
        _ => println!("installed: no (no scheduler on this host)"),
    }
    match SyncConfig::load(&config_dir)? {
        Some(sync) => {
            println!("interval: {}", sync.interval());
            println!("scope: {}", describe_scope(&sync, home));
        }
        None => println!(
            "config: no [sync] in {}",
            config_dir.join(CONFIG_FILE_NAME).display()
        ),
    }
    match read_status(&config_dir)? {
        Some(last) => {
            let result = if last.ok { "ok" } else { "failed" };
            println!(
                "last pass: {} ({result}): {}",
                last.finished_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                last.summary
            );
            for failure in &last.failures {
                println!("  failed: {failure}");
            }
        }
        None => println!("last pass: never"),
    }
    Ok(())
}

fn describe_scope(sync: &SyncConfig, home: Option<&Path>) -> String {
    let Ok(scope) = sync.scope(home) else {
        return "not configured ([sync] has no `include`)".to_string();
    };
    let dirs = if scope.include.is_empty() {
        "every session".to_string()
    } else {
        scope
            .include
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let harnesses: Vec<&str> = scope.harnesses.iter().map(|t| t.name()).collect();
    format!("{dirs}; harnesses: {}", harnesses.join(", "))
}

/// Stop the job; a job that is not loaded fails the command, which is
/// fine.
fn unload(scheduler: Scheduler) {
    for command in scheduler.unload_commands(uid()) {
        let _ = run_command(&command);
    }
}

fn run_command(argv: &[String]) -> Result<()> {
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .with_context(|| format!("run {}", argv[0]))?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_pathbase::FakePathbase;
    use tempfile::TempDir;

    const CODEX_ID: &str = "019e03b2-11a2-7320-9098-5d831813e66c";

    /// A Codex home holding the fixture rollout, and a config dir.
    struct Machine {
        home: TempDir,
        cfg: TempDir,
    }

    impl Machine {
        fn new() -> Self {
            let m = Self {
                home: TempDir::new().unwrap(),
                cfg: TempDir::new().unwrap(),
            };
            let fixture =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/codex/convo.jsonl");
            let dir = m.home.path().join(".codex/sessions/2026/05/07");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(
                fixture,
                dir.join(format!("rollout-2026-05-07T18-27-42-{CODEX_ID}.jsonl")),
            )
            .unwrap();
            m
        }

        fn config(&self, text: &str) {
            std::fs::write(self.cfg.path().join(CONFIG_FILE_NAME), text).unwrap();
        }

        fn bundle(&self) -> HarnessBundle {
            let codex =
                toolpath_codex::PathResolver::new().with_codex_dir(self.home.path().join(".codex"));
            HarnessBundle {
                codex: Some(toolpath_codex::CodexConvo::with_resolver(codex)),
                ..Default::default()
            }
        }

        fn pass(&self, server: &FakePathbase) -> Result<Pass> {
            let login = Login {
                base_url: server.base(),
                token: "token".into(),
                username: "me".into(),
            };
            pass(self.cfg.path(), None, &self.bundle(), &login, false)
        }
    }

    fn ran(pass: Result<Pass>) -> Report {
        match pass.unwrap() {
            Pass::Ran(report) => report,
            Pass::Busy => panic!("the pass found the lock held"),
        }
    }

    #[test]
    fn a_pass_uploads_new_sessions_and_a_second_pass_sends_nothing() {
        let machine = Machine::new();
        machine.config("[sync]\ninclude = []\nharnesses = ['codex']\n");
        let server = FakePathbase::start();

        let first = ran(machine.pass(&server));
        assert!(
            first.summary_line().starts_with("1 session: 1 created ("),
            "{}",
            first.summary_line()
        );
        assert_eq!(server.graphs().len(), 1);
        assert!(!server.writes().is_empty());

        server.clear_log();
        let second = ran(machine.pass(&server));
        assert_eq!(second.summary_line(), "1 session: 1 unchanged");
        assert!(server.writes().is_empty(), "{:?}", server.writes());
        assert_eq!(server.graphs().len(), 1);
    }

    #[test]
    fn a_pass_leaves_out_sessions_under_a_sync_false_rule() {
        let machine = Machine::new();
        let cwd = crate::sync::sources::source_for(
            &machine.bundle(),
            crate::artifact::ArtifactType::Codex,
        )
        .unwrap()
        .peek_dir(CODEX_ID)
        .expect("the fixture records its working directory");
        machine.config(&format!(
            "[sync]\ninclude = []\n\n[[project]]\ndir = {cwd:?}\nsync = false\n"
        ));
        let server = FakePathbase::start();
        let report = ran(machine.pass(&server));
        assert_eq!(report.summary_line(), "0 sessions");
        assert!(server.writes().is_empty());
    }

    #[test]
    fn a_pass_keeps_to_the_included_directories() {
        let machine = Machine::new();
        machine.config("[sync]\ninclude = ['/nowhere/near']\n");
        let server = FakePathbase::start();
        assert_eq!(ran(machine.pass(&server)).summary_line(), "0 sessions");
        assert!(server.requests().is_empty());
    }

    #[test]
    fn a_pass_needs_a_sync_block() {
        let machine = Machine::new();
        let server = FakePathbase::start();
        let err = machine.pass(&server).err().expect("no [sync], no pass");
        assert!(err.to_string().contains("path sync install"), "{err}");
    }

    #[test]
    fn a_pass_while_another_holds_the_lock_is_skipped() {
        let machine = Machine::new();
        machine.config("[sync]\ninclude = []\n");
        let server = FakePathbase::start();
        let _held = crate::sync::lock::lock_uploads(machine.cfg.path(), false)
            .unwrap()
            .unwrap();
        assert!(matches!(machine.pass(&server).unwrap(), Pass::Busy));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn the_status_file_round_trips() {
        let cfg = TempDir::new().unwrap();
        assert_eq!(read_status(cfg.path()).unwrap(), None);
        let status = PassStatus {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            ok: false,
            summary: "2 sessions: 1 created (3 steps), 1 failed".into(),
            failures: vec!["codex abc → me/pathstash: boom".into()],
        };
        write_status(cfg.path(), &status).unwrap();
        assert_eq!(read_status(cfg.path()).unwrap(), Some(status));
    }

    #[test]
    fn the_log_starts_over_past_its_limit() {
        let cfg = TempDir::new().unwrap();
        let log = cfg.path().join(SYNC_LOG_FILE_NAME);
        std::fs::write(&log, vec![b'x'; 10]).unwrap();
        trim_log(cfg.path());
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 10);
        std::fs::write(&log, vec![b'x'; LOG_LIMIT as usize + 1]).unwrap();
        trim_log(cfg.path());
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
    }
}
