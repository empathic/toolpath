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
use crate::sync_service::{Installed, Job, Scheduler, installed_job};

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

#[derive(Args, Debug, Default)]
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
    let code = finish(&config_dir, started_at, outcome);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Report how a pass went and record it in the status file, unless it
/// was skipped; the exit code.
fn finish(config_dir: &Path, started_at: DateTime<Utc>, outcome: Result<Pass>) -> i32 {
    let (status, code) = match outcome {
        Ok(Pass::Busy) => {
            println!("path sync: another upload pass is running; skipped");
            return 0;
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
    if let Err(e) = write_status(config_dir, &status) {
        eprintln!("path sync: could not record the pass: {e:#}");
    }
    code
}

/// The stored login, checked against its server as `share --all`
/// checks it.
fn login(config_dir: &Path) -> Result<Login> {
    let stored = crate::cmd_pathbase::load_session(&config_dir.join(CREDENTIALS_FILE_NAME))?
        .context("not logged in; run `path auth login`")?;
    let user = crate::cmd_pathbase::api_me(&stored.url, &stored.token).map_err(|e| {
        if e.is::<crate::cmd_pathbase::CredentialsRejected>() {
            anyhow::anyhow!("{e}; run `path auth login`")
        } else {
            e
        }
    })?;
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
    trim_log(config_dir);
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
    crate::sync::write_config_file(
        config_dir,
        SYNC_STATUS_FILE_NAME,
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

/// Write a launchd or systemd file through a sibling temporary file, so
/// the scheduler never reads half of it.
fn write_job_file(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("a file path has a parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

// ── install / uninstall / status ────────────────────────────────────

/// The account and scheduler a job command acts on. The job is one per
/// account (one label, one path under `home`), while `[sync]` and the
/// records are per config dir, so every command checks which config dir
/// an installed job carries before touching it.
pub(crate) struct Host<'a> {
    pub(crate) scheduler: Scheduler,
    pub(crate) home: &'a Path,
    /// Absolute.
    pub(crate) config_dir: &'a Path,
    /// `home` is this account's home, so launchctl and systemctl reach
    /// the job these files describe; otherwise the files are only
    /// written or removed.
    pub(crate) live: bool,
    pub(crate) uid: u32,
    pub(crate) run: &'a mut dyn FnMut(&[String]) -> Result<()>,
}

impl Host<'_> {
    fn job_file(&self) -> PathBuf {
        self.scheduler.files(self.home).remove(0)
    }

    fn not_live_note(&self, what: &str) -> String {
        format!(
            "Not {what}: {} is not this account's home, so launchctl or systemctl would act on another job",
            self.home.display()
        )
    }

    /// Refuse to touch an installed job that uploads from another
    /// config dir.
    fn check_config_dir(&self, job: &Installed) -> Result<()> {
        match &job.config_dir {
            Some(dir) if same_dir(dir, self.config_dir) => Ok(()),
            Some(dir) => bail!(
                "the installed job ({}) uploads from {}, not {}; run this with {}={} to change it",
                self.job_file().display(),
                dir.display(),
                self.config_dir.display(),
                crate::config::CONFIG_DIR_ENV,
                dir.display()
            ),
            None => bail!(
                "cannot tell which config dir the installed job ({}) uploads from; remove it by hand",
                self.job_file().display()
            ),
        }
    }

    fn unload(&mut self) {
        for command in self.scheduler.unload_commands(self.uid) {
            let _ = (self.run)(&command);
        }
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    crate::share_config::canonicalize_prefix(a) == crate::share_config::canonicalize_prefix(b)
}

fn host_scheduler() -> Result<Scheduler> {
    Scheduler::host().context("`path sync install` supports macOS (launchd) and Linux (systemd)")
}

fn require_home(config: &Config) -> Result<&Path> {
    config
        .home_dir()
        .map(PathBuf::as_path)
        .context("$HOME is not set; cannot place the scheduled job")
}

/// Run `f` with the [`Host`] for this process.
fn with_host<T>(config: &Config, f: impl FnOnce(&mut Host) -> Result<T>) -> Result<T> {
    let home = require_home(config)?;
    let config_dir = std::path::absolute(config.config_dir()?)?;
    let mut run = run_command;
    let mut host = Host {
        scheduler: host_scheduler()?,
        home,
        config_dir: &config_dir,
        live: account_home().is_some_and(|account| same_dir(&account, home)),
        uid: uid(),
        run: &mut run,
    };
    f(&mut host)
}

/// The harness locations set here, which a pass under the scheduler
/// would otherwise not see.
fn harness_env(config: &Config) -> Vec<(&'static str, String)> {
    [
        ("CLAUDE_CONFIG_DIR", &config.claude_config_dir),
        ("COPILOT_HOME", &config.copilot_home),
        ("XDG_DATA_HOME", &config.xdg_data_home),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        let value = std::path::absolute(value.as_ref()?).ok()?;
        Some((key, value.to_str()?.to_string()))
    })
    .collect()
}

fn install(config: &Config, args: InstallArgs) -> Result<()> {
    let binary = std::env::current_exe().context("locate the path binary")?;
    let env = harness_env(config);
    with_host(config, |host| install_job(host, &args, &binary, &env))
}

fn uninstall(config: &Config) -> Result<()> {
    with_host(config, uninstall_job)
}

fn status(config: &Config) -> Result<()> {
    let config_dir = std::path::absolute(config.config_dir()?)?;
    let home = config.home_dir().map(PathBuf::as_path);
    print!("{}", status_text(Scheduler::host(), home, &config_dir)?);
    Ok(())
}

pub(crate) fn install_job(
    host: &mut Host,
    args: &InstallArgs,
    binary: &Path,
    env: &[(&str, String)],
) -> Result<()> {
    let installed = installed_job(host.scheduler, host.home);
    if let Some(job) = &installed {
        if !args.force {
            bail!(
                "path sync is already installed ({}); pass --force to replace it",
                host.job_file().display()
            );
        }
        host.check_config_dir(job)?;
    }

    let config_path = host.config_dir.join(CONFIG_FILE_NAME);
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
        interval: args.interval.clone(),
    };
    let updated = crate::sync_config::install_config(&text, &settings)?;
    let sync = SyncConfig::parse(&updated, &config_path.display().to_string())?
        .expect("install writes [sync]");
    let job = Job {
        binary,
        config_dir: host.config_dir,
        interval_secs: parse_interval(sync.interval())?,
        env,
    };
    let files = host.scheduler.render(host.home, &job)?;

    let load = !args.no_load && host.live;
    if installed.is_some() && host.live {
        host.unload();
    }
    crate::sync::write_config_file(host.config_dir, CONFIG_FILE_NAME, &updated)?;
    for (path, content) in &files {
        write_job_file(path, content)?;
    }
    if load {
        // launchd finishes a bootout after launchctl returns, so a
        // bootstrap right after one can find the label still taken.
        let attempts = match (host.scheduler, installed.is_some()) {
            (Scheduler::Launchd, true) => 5,
            _ => 1,
        };
        for command in host.scheduler.load_commands(host.home, host.uid) {
            for attempt in 1..=attempts {
                match (host.run)(&command) {
                    Ok(()) => break,
                    Err(_) if attempt < attempts => {
                        std::thread::sleep(std::time::Duration::from_millis(300))
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    let log = host.config_dir.join(SYNC_LOG_FILE_NAME);
    println!(
        "{} the {}: {}",
        if load { "Installed" } else { "Wrote" },
        host.scheduler.name(),
        files
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !args.no_load && !host.live {
        println!("{}", host.not_live_note("loaded"));
    }
    println!(
        "Runs `{} sync` every {}; output goes to {}",
        binary.display(),
        sync.interval(),
        log.display()
    );
    println!("Scope: {}", describe_scope(&sync, Some(host.home)));
    if crate::cmd_pathbase::load_session(&host.config_dir.join(CREDENTIALS_FILE_NAME))?.is_none() {
        println!("Not logged in: each pass fails until you run `path auth login`");
    }
    Ok(())
}

pub(crate) fn uninstall_job(host: &mut Host) -> Result<()> {
    let Some(job) = installed_job(host.scheduler, host.home) else {
        println!("path sync is not installed");
        return Ok(());
    };
    host.check_config_dir(&job)?;
    if host.live {
        host.unload();
    }
    let installed: Vec<PathBuf> = host
        .scheduler
        .files(host.home)
        .into_iter()
        .filter(|f| f.exists())
        .collect();
    for path in &installed {
        std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    if host.live {
        for command in host.scheduler.after_remove_commands() {
            let _ = (host.run)(&command);
        }
    }
    println!(
        "Removed the {}: {}",
        host.scheduler.name(),
        installed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !host.live {
        println!("{}", host.not_live_note("unloaded"));
    }
    println!("Kept [sync] in config.toml and the upload records");
    Ok(())
}

pub(crate) fn status_text(
    scheduler: Option<Scheduler>,
    home: Option<&Path>,
    config_dir: &Path,
) -> Result<String> {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    let sync = SyncConfig::load(config_dir)?;
    let installed = match (scheduler, home) {
        (Some(scheduler), Some(home)) => installed_job(scheduler, home).map(|job| {
            let file = scheduler.files(home).remove(0);
            (scheduler, file, job)
        }),
        _ => None,
    };
    let mut stale = false;
    match &installed {
        None if scheduler.is_none() => line("installed: no (no scheduler on this host)".into()),
        None => line("installed: no".into()),
        Some((scheduler, file, job)) => {
            line(format!(
                "installed: {} ({})",
                scheduler.name(),
                file.display()
            ));
            let ours = job
                .config_dir
                .as_deref()
                .is_some_and(|d| same_dir(d, config_dir));
            let dir = match &job.config_dir {
                Some(d) if ours => d.display().to_string(),
                Some(d) => format!(
                    "{} (not this config dir, {})",
                    d.display(),
                    config_dir.display()
                ),
                None => "unknown".into(),
            };
            line(format!("  config dir: {dir}"));
            match &job.binary {
                Some(b) if b.exists() => line(format!("  binary: {}", b.display())),
                Some(b) => {
                    stale = ours;
                    line(format!("  binary: {} (missing)", b.display()));
                }
                None => line("  binary: unknown".into()),
            }
            if let Some(secs) = job.interval_secs {
                line(format!("  runs every: {secs}s"));
                let configured = sync
                    .as_ref()
                    .and_then(|s| parse_interval(s.interval()).ok());
                stale |= ours && configured.is_some_and(|c| c != secs);
            }
        }
    }
    match &sync {
        Some(sync) => {
            line(format!("interval: {}", sync.interval()));
            line(format!("scope: {}", describe_scope(sync, home)));
        }
        None => line(format!(
            "config: no [sync] in {}",
            config_dir.join(CONFIG_FILE_NAME).display()
        )),
    }
    if stale {
        line("the installed job is out of date; run `path sync install --force`".into());
    }
    match read_status(config_dir)? {
        Some(last) => {
            let result = if last.ok { "ok" } else { "failed" };
            line(format!(
                "last pass: {} ({result}): {}",
                last.finished_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                last.summary
            ));
            for failure in &last.failures {
                line(format!("  failed: {failure}"));
            }
        }
        None => line("last pass: never".into()),
    }
    Ok(out)
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
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

/// This account's home directory from the password database, which
/// `$HOME` can be pointed away from.
#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns null or a pointer to static storage that
    // stays valid until the next getpw* call; pw_dir is copied out
    // before returning.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
    }
}

#[cfg(not(unix))]
fn account_home() -> Option<PathBuf> {
    None
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
        assert_eq!(report.summary_line(), "1 session: 1 excluded");
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

    fn store_login(cfg: &Path, url: &str) {
        crate::cmd_pathbase::store_session(
            &cfg.join(CREDENTIALS_FILE_NAME),
            &crate::cmd_pathbase::StoredSession {
                url: url.into(),
                token: "token".into(),
                user: crate::cmd_pathbase::User {
                    id: "1".into(),
                    username: "alex".into(),
                    email: None,
                    display_name: None,
                },
            },
        )
        .unwrap();
    }

    #[test]
    fn only_rejected_credentials_are_told_to_log_in() {
        let cfg = TempDir::new().unwrap();
        let err = login(cfg.path()).err().unwrap();
        assert_eq!(err.to_string(), "not logged in; run `path auth login`");

        let server = FakePathbase::start();
        store_login(cfg.path(), &server.base());
        assert_eq!(login(cfg.path()).unwrap().username, "alex");

        server.force(
            "GET",
            "/api/v1/u/me",
            401,
            r#"{"code":"unauthorized","error":"bad token"}"#,
        );
        let err = format!("{:#}", login(cfg.path()).err().unwrap());
        assert!(
            err.ends_with("(401 Unauthorized); run `path auth login`"),
            "{err}"
        );
        let rejected = FakePathbase::start();
        rejected.force("GET", "/api/v1/u/me", 403, "forbidden");
        store_login(cfg.path(), &rejected.base());
        let err = format!("{:#}", login(cfg.path()).err().unwrap());
        assert!(
            err.ends_with("(403 Forbidden); run `path auth login`"),
            "{err}"
        );

        store_login(cfg.path(), "http://127.0.0.1:1");
        let err = format!("{:#}", login(cfg.path()).err().unwrap());
        assert!(!err.contains("auth login"), "{err}");
    }

    /// A home and config dir for job commands, and every command they
    /// ran.
    struct Account {
        home: TempDir,
        cfg: TempDir,
        ran: Vec<Vec<String>>,
    }

    impl Account {
        fn new() -> Self {
            Self {
                home: TempDir::new().unwrap(),
                cfg: TempDir::new().unwrap(),
                ran: Vec::new(),
            }
        }

        /// Run `f` as `scheduler` on this account (`live`) or not,
        /// against `config_dir`, recording commands; `fail` makes a
        /// command fail.
        fn with<T>(
            &mut self,
            scheduler: Scheduler,
            live: bool,
            config_dir: &Path,
            fail: &dyn Fn(&[String]) -> bool,
            f: impl FnOnce(&mut Host) -> Result<T>,
        ) -> Result<T> {
            let ran = &mut self.ran;
            let mut run = |argv: &[String]| {
                ran.push(argv.to_vec());
                if fail(argv) {
                    bail!("{} failed", argv.join(" "))
                }
                Ok(())
            };
            let mut host = Host {
                scheduler,
                home: self.home.path(),
                config_dir,
                live,
                uid: 501,
                run: &mut run,
            };
            f(&mut host)
        }

        fn install(&mut self, scheduler: Scheduler, live: bool, args: InstallArgs) -> Result<()> {
            let cfg = self.cfg.path().to_path_buf();
            let binary = std::env::current_exe().unwrap();
            self.with(scheduler, live, &cfg, &|_| false, |h| {
                install_job(h, &args, &binary, &[])
            })
        }

        fn uninstall(&mut self, scheduler: Scheduler, live: bool, config_dir: &Path) -> Result<()> {
            self.with(scheduler, live, config_dir, &|_| false, uninstall_job)
        }

        fn files_exist(&self, scheduler: Scheduler) -> bool {
            scheduler.files(self.home.path()).iter().all(|f| f.exists())
        }

        fn programs(&self) -> Vec<String> {
            self.ran.iter().map(|c| c[..3].join(" ")).collect()
        }
    }

    fn all() -> InstallArgs {
        InstallArgs {
            all: true,
            ..Default::default()
        }
    }

    const BOTH: [Scheduler; 2] = [Scheduler::Launchd, Scheduler::Systemd];

    #[test]
    fn install_writes_files_without_loading_and_refuses_a_second_time() {
        for scheduler in BOTH {
            let mut a = Account::new();
            let no_load = InstallArgs {
                no_load: true,
                interval: Some("5m".into()),
                ..all()
            };
            a.install(scheduler, true, no_load).unwrap();
            assert!(a.files_exist(scheduler));
            assert!(a.ran.is_empty(), "{:?}", a.ran);
            let config = std::fs::read_to_string(a.cfg.path().join(CONFIG_FILE_NAME)).unwrap();
            assert_eq!(config, "[sync]\ninclude = []\ninterval = \"5m\"\n");
            let job = installed_job(scheduler, a.home.path()).unwrap();
            assert_eq!(job.config_dir.as_deref(), Some(a.cfg.path()));
            assert_eq!(job.interval_secs, Some(300));

            let err = a.install(scheduler, true, all()).unwrap_err();
            assert!(err.to_string().contains("pass --force"), "{err}");
            assert!(a.ran.is_empty());
        }
    }

    #[test]
    fn a_live_install_loads_the_job_and_force_replaces_it() {
        let mut a = Account::new();
        a.install(Scheduler::Systemd, true, all()).unwrap();
        assert_eq!(
            a.programs(),
            ["systemctl --user daemon-reload", "systemctl --user enable"]
        );
        a.ran.clear();
        let force = InstallArgs {
            force: true,
            ..Default::default()
        };
        a.install(Scheduler::Systemd, true, force).unwrap();
        assert_eq!(
            a.programs(),
            [
                "systemctl --user disable",
                "systemctl --user daemon-reload",
                "systemctl --user enable"
            ]
        );
    }

    #[test]
    fn a_launchd_replace_retries_the_bootstrap_while_the_bootout_settles() {
        let mut a = Account::new();
        a.install(Scheduler::Launchd, true, all()).unwrap();
        a.ran.clear();
        let cfg = a.cfg.path().to_path_buf();
        let binary = std::env::current_exe().unwrap();
        let tries = std::cell::Cell::new(0);
        let flaky = |argv: &[String]| {
            argv[1] == "bootstrap" && {
                tries.set(tries.get() + 1);
                tries.get() < 3
            }
        };
        let force = InstallArgs {
            force: true,
            ..Default::default()
        };
        a.with(Scheduler::Launchd, true, &cfg, &flaky, |h| {
            install_job(h, &force, &binary, &[])
        })
        .unwrap();
        assert_eq!(
            a.programs(),
            [
                "launchctl bootout gui/501/net.toolpath.path-sync",
                "launchctl bootstrap gui/501",
                "launchctl bootstrap gui/501",
                "launchctl bootstrap gui/501"
            ]
        );
    }

    #[test]
    fn with_another_home_the_scheduler_is_never_called() {
        for scheduler in BOTH {
            let mut a = Account::new();
            a.install(scheduler, false, all()).unwrap();
            assert!(a.files_exist(scheduler));
            let cfg = a.cfg.path().to_path_buf();
            a.uninstall(scheduler, false, &cfg).unwrap();
            assert!(!scheduler.files(a.home.path()).iter().any(|f| f.exists()));
            assert!(a.ran.is_empty(), "{:?}", a.ran);
        }
    }

    #[test]
    fn a_live_uninstall_unloads_removes_and_keeps_the_config() {
        let mut a = Account::new();
        a.install(Scheduler::Systemd, true, all()).unwrap();
        a.ran.clear();
        let cfg = a.cfg.path().to_path_buf();
        a.uninstall(Scheduler::Systemd, true, &cfg).unwrap();
        assert_eq!(
            a.programs(),
            ["systemctl --user disable", "systemctl --user daemon-reload"]
        );
        assert!(
            !Scheduler::Systemd
                .files(a.home.path())
                .iter()
                .any(|f| f.exists())
        );
        assert!(cfg.join(CONFIG_FILE_NAME).exists());
    }

    #[test]
    fn a_job_for_another_config_dir_is_left_alone() {
        for scheduler in BOTH {
            let mut a = Account::new();
            a.install(scheduler, true, all()).unwrap();
            a.ran.clear();
            let other = TempDir::new().unwrap();
            let binary = std::env::current_exe().unwrap();

            let err = a.uninstall(scheduler, true, other.path()).unwrap_err();
            assert!(err.to_string().contains("not "), "{err}");
            let force = InstallArgs {
                force: true,
                ..all()
            };
            let err = a
                .with(scheduler, true, other.path(), &|_| false, |h| {
                    install_job(h, &force, &binary, &[])
                })
                .unwrap_err();
            assert!(
                err.to_string()
                    .contains(&a.cfg.path().display().to_string()),
                "{err}"
            );
            assert!(a.ran.is_empty(), "{:?}", a.ran);
            assert!(a.files_exist(scheduler));
            assert!(!other.path().join(CONFIG_FILE_NAME).exists());

            let text = status_text(Some(scheduler), Some(a.home.path()), other.path()).unwrap();
            assert!(text.contains("(not this config dir"), "{text}");
            assert!(!text.contains("out of date"), "{text}");
        }
    }

    #[test]
    fn status_flags_a_missing_binary_and_a_changed_interval() {
        let a = Account::new();
        let missing = Path::new("/nowhere/path");
        let job = Job {
            binary: missing,
            config_dir: a.cfg.path(),
            interval_secs: 300,
            env: &[],
        };
        for (path, content) in Scheduler::Launchd.render(a.home.path(), &job).unwrap() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        std::fs::write(
            a.cfg.path().join(CONFIG_FILE_NAME),
            "[sync]\ninclude = []\ninterval = '5m'\n",
        )
        .unwrap();
        let text =
            status_text(Some(Scheduler::Launchd), Some(a.home.path()), a.cfg.path()).unwrap();
        assert!(
            text.contains("  binary: /nowhere/path (missing)\n"),
            "{text}"
        );
        assert!(text.contains("install --force"), "{text}");

        let present = std::env::current_exe().unwrap();
        let job = Job {
            binary: &present,
            ..job
        };
        for (path, content) in Scheduler::Launchd.render(a.home.path(), &job).unwrap() {
            std::fs::write(path, content).unwrap();
        }
        let text =
            status_text(Some(Scheduler::Launchd), Some(a.home.path()), a.cfg.path()).unwrap();
        assert!(!text.contains("install --force"), "{text}");
        std::fs::write(
            a.cfg.path().join(CONFIG_FILE_NAME),
            "[sync]\ninclude = []\ninterval = '1h'\n",
        )
        .unwrap();
        let text =
            status_text(Some(Scheduler::Launchd), Some(a.home.path()), a.cfg.path()).unwrap();
        assert!(text.contains("  runs every: 300s\n"), "{text}");
        assert!(text.contains("install --force"), "{text}");
    }

    #[test]
    fn a_failed_pass_is_recorded_and_exits_non_zero() {
        let cfg = TempDir::new().unwrap();
        let code = finish(
            cfg.path(),
            Utc::now(),
            Err(anyhow::anyhow!("not logged in; run `path auth login`")),
        );
        assert_eq!(code, 1);
        let status = read_status(cfg.path()).unwrap().unwrap();
        assert!(!status.ok);
        assert_eq!(status.summary, "not logged in; run `path auth login`");

        let mut report = Report::default();
        report.sessions.push((
            crate::artifact::ArtifactType::Codex,
            "abc".into(),
            "me/pathstash".into(),
            share_all::SessionResult::Failed("boom".into()),
        ));
        assert_eq!(finish(cfg.path(), Utc::now(), Ok(Pass::Ran(report))), 1);
        let status = read_status(cfg.path()).unwrap().unwrap();
        assert_eq!(status.summary, "1 session: 1 failed");
        assert_eq!(status.failures, ["codex abc → me/pathstash: boom"]);

        assert_eq!(
            finish(cfg.path(), Utc::now(), Ok(Pass::Ran(Report::default()))),
            0
        );
        assert!(read_status(cfg.path()).unwrap().unwrap().ok);
    }

    #[test]
    fn a_skipped_pass_leaves_the_status_file_alone() {
        let cfg = TempDir::new().unwrap();
        assert_eq!(finish(cfg.path(), Utc::now(), Ok(Pass::Busy)), 0);
        assert_eq!(read_status(cfg.path()).unwrap(), None);
        finish(cfg.path(), Utc::now(), Err(anyhow::anyhow!("earlier")));
        let before = read_status(cfg.path()).unwrap();
        assert_eq!(finish(cfg.path(), Utc::now(), Ok(Pass::Busy)), 0);
        assert_eq!(read_status(cfg.path()).unwrap(), before);
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
