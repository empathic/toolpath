//! `path sync`: one pass of automatic upload over the sessions in scope,
//! plus `status`, `install`, and `uninstall`.

use crate::config::{CONFIG_FILE_NAME, Config, SYNC_STATUS_FILE_NAME, UPLOAD_LOCK_FILE_NAME};
use crate::sync::api::{PathbaseSync, SyncApi};
use crate::sync::pass::{Destination, Outcome, PassContext, Session, sync_session};
use crate::sync_config::{ScopeOverrides, UserSyncConfig};
use crate::sync_service::{self, InstallOptions, LAUNCHD_LABEL, SERVICE_NAME};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct SyncArgs {
    #[command(subcommand)]
    pub op: Option<SyncOp>,

    /// Derive and plan, but replay nothing, send nothing, and record nothing
    #[arg(long)]
    pub dry_run: bool,

    /// Only sessions under this directory, replacing the configured scope
    /// for this run (repeatable)
    #[arg(long, value_name = "DIR", conflicts_with = "all")]
    pub include: Vec<String>,

    /// Every session, replacing the configured scope for this run
    #[arg(long)]
    pub all: bool,

    /// Only this harness (repeatable): claude, gemini, codex, opencode,
    /// cursor, pi, copilot
    #[arg(long, value_name = "NAME")]
    pub harness: Vec<String>,

    /// Upload to this repo (`owner/name`) instead of the configured
    /// destination. Never bypasses a `sync = false` exclusion.
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Pathbase server for this run only (default: the server in a URL-form
    /// remote, else $PATHBASE_URL, else https://pathbase.dev). To persist a
    /// server, configure a remote that carries it:
    /// `remote = "https://host/u/owner/name"`.
    #[arg(long)]
    pub url: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SyncOp {
    /// Show the configured scope, the last pass, and staged operations
    Status,
    /// Enable sync in config.toml and schedule `path sync` with the
    /// user's launchd or systemd
    Install {
        /// Directory subtree to sync (repeatable)
        #[arg(long, value_name = "DIR", conflicts_with = "all")]
        include: Vec<String>,
        /// Sync every session on this machine
        #[arg(long)]
        all: bool,
        /// Pass interval, e.g. 15m, 1h
        #[arg(long, value_name = "DURATION")]
        interval: Option<String>,
        /// Default destination repo (`owner/name`)
        #[arg(long, value_name = "OWNER/NAME")]
        remote: Option<String>,
    },
    /// Disable sync in config.toml and remove the scheduled service
    Uninstall,
}

pub fn run(args: SyncArgs, config: &Config) -> Result<()> {
    match args.op {
        Some(SyncOp::Status) => status(config),
        Some(SyncOp::Install {
            include,
            all,
            interval,
            remote,
        }) => install(
            config,
            InstallOptions {
                all,
                include,
                interval,
                remote,
            },
        ),
        Some(SyncOp::Uninstall) => uninstall(config),
        None => pass(args, config),
    }
}

// ── the pass ────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) created: usize,
    pub(crate) updated: usize,
    pub(crate) continued: usize,
    pub(crate) frozen: usize,
    pub(crate) unchanged: usize,
    pub(crate) planned: usize,
    pub(crate) pending: usize,
    pub(crate) failed: usize,
}

impl Counts {
    fn tally(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Created(_) => self.created += 1,
            Outcome::Updated(_) => self.updated += 1,
            Outcome::Continued(_) => self.continued += 1,
            Outcome::Frozen(_) => self.frozen += 1,
            Outcome::Unchanged => self.unchanged += 1,
            Outcome::Planned(_) => self.planned += 1,
            Outcome::Pending(_) => self.pending += 1,
            Outcome::Failed(_) => self.failed += 1,
        }
    }
}

/// What the last pass did, written to `sync-status.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PassStatus {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) finished_at: DateTime<Utc>,
    pub(crate) dry_run: bool,
    pub(crate) include: Vec<String>,
    pub(crate) harnesses: Vec<String>,
    pub(crate) destinations: Vec<String>,
    pub(crate) sessions: usize,
    pub(crate) counts: Counts,
    /// Bounded: the first `MAX_STATUS_ERRORS` problems.
    pub(crate) errors: Vec<String>,
    pub(crate) staged_operations: usize,
}

const MAX_STATUS_ERRORS: usize = 50;

fn load_user_config(config_dir: &Path) -> Result<(PathBuf, String, UserSyncConfig)> {
    let path = config_dir.join(CONFIG_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let user = UserSyncConfig::parse(&text, &path.display().to_string())?;
    Ok((path, text, user))
}

/// The exclusive lock every uploader holds across decide, send, and
/// record. Held for the whole pass; a concurrent `share` waits.
fn lock_uploads(config_dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("create {}", config_dir.display()))?;
    let path = config_dir.join(UPLOAD_LOCK_FILE_NAME);
    let file =
        std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    file.lock()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}

fn pass(args: SyncArgs, config: &Config) -> Result<()> {
    let started_at = Utc::now();
    let config_dir = config.config_dir()?;
    let home = config.home_dir().cloned();
    let (_, _, user) = load_user_config(&config_dir)?;
    let overrides = ScopeOverrides {
        all: args.all,
        include: args.include.clone(),
        harnesses: args.harness.clone(),
    };
    let scope = user.sync.scope(&overrides, home.as_deref())?;
    if !scope.enabled {
        eprintln!(
            "sync is disabled ([sync].enabled is not true in {}); run `path sync install` to enable it",
            config_dir.join(CONFIG_FILE_NAME).display()
        );
        return Ok(());
    }
    let credentials = crate::cmd_pathbase::load_session(&crate::cmd_pathbase::credentials_path()?)?;
    let Some(credentials) = credentials else {
        bail!("sync uploads require login; run `path auth login`");
    };
    let default_url = crate::cmd_pathbase::resolve_url(args.url.clone());
    let username = credentials.user.username.clone();

    let _lock = lock_uploads(&config_dir)?;

    // Ingest through the same engine `p cache sync` uses, so the cache
    // holds every in-scope session at its current stamp.
    let bundle = crate::providers::harness_bundle(config);
    if scope.include.is_empty() {
        crate::sync::sync_bundle(&config_dir, &bundle, &scope.harnesses, None, &mut ())?;
    } else {
        for dir in &scope.include {
            crate::sync::sync_bundle(&config_dir, &bundle, &scope.harnesses, Some(dir), &mut ())?;
        }
    }

    let manifest = crate::sync::load_manifest(&config_dir)?;
    let mut apis: HashMap<String, PathbaseSync> = HashMap::new();
    let mut counts = Counts::default();
    let mut errors = Vec::new();
    let mut destinations: BTreeMap<String, usize> = BTreeMap::new();
    let mut sessions = 0usize;
    for &harness in &scope.harnesses {
        let Some(records) = manifest.get(harness.name()) else {
            continue;
        };
        let Some(source) = crate::sync::sources::source_for(&bundle, harness) else {
            continue;
        };
        for (id, rec) in records {
            let Some(cache_id) = rec.cache_id.clone() else {
                continue;
            };
            let dir = rec.path.as_deref();
            if !scope.contains(harness, dir) {
                continue;
            }
            let Some(remote) = user.destination(
                &scope,
                harness,
                dir,
                home.as_deref(),
                args.repo.as_deref(),
                Some(&username),
            )?
            else {
                continue;
            };
            let base_url = args
                .url
                .clone()
                .or(remote.base_url.clone())
                .unwrap_or_else(|| default_url.clone());
            let destination = Destination {
                repo: format!("{}/{}", remote.repo.owner, remote.repo.name),
                base_url: base_url.trim_end_matches('/').to_string(),
            };
            *destinations.entry(destination.repo_url()).or_default() += 1;
            let api = match apis.entry(destination.base_url.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => e.insert(PathbaseSync::new(
                    &destination.base_url,
                    &credentials.token,
                )?),
            };
            let project = harness.path_keyed().then(|| rec.path.clone()).flatten();
            let session = Session {
                harness,
                id: id.clone(),
                project: project.clone(),
                path: rec.path.clone(),
                stamp: (rec.modified, rec.size),
            };
            let ctx = PassContext {
                config_dir: &config_dir,
                api: api as &dyn SyncApi,
                now: Utc::now(),
                dry_run: args.dry_run,
            };
            let live = || -> Result<toolpath::v1::Graph> {
                let path = crate::cache::cache_path(&cache_id)?;
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?;
                toolpath::v1::Graph::from_json(&text)
                    .with_context(|| format!("parse {}", path.display()))
            };
            let restat = || source.stamp(project.as_deref(), id).unwrap_or((None, None));
            sessions += 1;
            let outcome = match sync_session(&ctx, &session, &destination, &live, &restat) {
                Ok(outcome) => outcome,
                Err(e) => Outcome::Failed(format!("{e:#}")),
            };
            counts.tally(&outcome);
            let label = format!("{} {}", harness.name(), short_id(id));
            match &outcome {
                Outcome::Unchanged => {}
                Outcome::Created(url) => eprintln!("{label}: created {url}"),
                Outcome::Updated(url) => eprintln!("{label}: updated {url}"),
                Outcome::Continued(url) => eprintln!("{label}: continued in {url}"),
                Outcome::Frozen(url) => eprintln!("{label}: frozen {url}"),
                Outcome::Planned(what) => eprintln!("{label}: {what}"),
                Outcome::Pending(why) | Outcome::Failed(why) => {
                    eprintln!("{label}: {why}");
                    if errors.len() < MAX_STATUS_ERRORS {
                        errors.push(format!("{label}: {why}"));
                    }
                }
            }
        }
    }

    let staged_operations = crate::sync::journal::list(&config_dir)?.len();
    let status = PassStatus {
        started_at,
        finished_at: Utc::now(),
        dry_run: args.dry_run,
        include: scope
            .include
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        harnesses: scope
            .harnesses
            .iter()
            .map(|h| h.name().to_string())
            .collect(),
        destinations: destinations.keys().cloned().collect(),
        sessions,
        counts: counts.clone(),
        errors,
        staged_operations,
    };
    if !args.dry_run {
        write_status(&config_dir, &status)?;
    }
    eprintln!("{}", summary_line(&status));
    if counts.failed > 0 || counts.pending > 0 {
        bail!(
            "{} session(s) failed and {} left a staged operation pending",
            counts.failed,
            counts.pending
        );
    }
    Ok(())
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn summary_line(status: &PassStatus) -> String {
    let c = &status.counts;
    let mut parts = Vec::new();
    for (n, what) in [
        (c.created, "created"),
        (c.updated, "updated"),
        (c.continued, "continued"),
        (c.frozen, "frozen"),
        (c.unchanged, "unchanged"),
        (c.planned, "planned"),
        (c.pending, "pending"),
        (c.failed, "failed"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let detail = if parts.is_empty() {
        "nothing in scope".to_string()
    } else {
        parts.join(", ")
    };
    format!(
        "{}{} session(s): {detail}",
        if status.dry_run { "dry run, " } else { "" },
        status.sessions
    )
}

fn status_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SYNC_STATUS_FILE_NAME)
}

fn write_status(config_dir: &Path, status: &PassStatus) -> Result<()> {
    let path = status_path(config_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(status)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))
}

// ── status ──────────────────────────────────────────────────────────

fn status(config: &Config) -> Result<()> {
    let config_dir = config.config_dir()?;
    let home = config.home_dir().cloned();
    let (config_path, _, user) = load_user_config(&config_dir)?;
    let scope = user
        .sync
        .scope(&ScopeOverrides::default(), home.as_deref())?;
    println!(
        "sync: {} ({})",
        if scope.enabled { "enabled" } else { "disabled" },
        config_path.display()
    );
    println!(
        "scope: {}",
        if scope.include.is_empty() {
            "all sessions".to_string()
        } else {
            scope
                .include
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!(
        "harnesses: {}",
        scope
            .harnesses
            .iter()
            .map(|h| h.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("interval: {}s", user.sync.interval_seconds()?);
    println!(
        "default remote: {}",
        user.sync.remote.as_deref().unwrap_or("<you>/pathstash")
    );
    let staged = crate::sync::journal::list(&config_dir)?;
    println!("staged operations: {}", staged.len());
    match std::fs::read_to_string(status_path(&config_dir)) {
        Ok(text) => {
            let last: PassStatus = serde_json::from_str(&text).context("parse sync-status.json")?;
            println!(
                "last pass: started {}, finished {}",
                last.started_at.to_rfc3339(),
                last.finished_at.to_rfc3339()
            );
            println!("  {}", summary_line(&last));
            if !last.destinations.is_empty() {
                println!("  destinations: {}", last.destinations.join(", "));
            }
            for e in &last.errors {
                println!("  ! {e}");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("last pass: never"),
        Err(e) => return Err(e).context("read sync-status.json"),
    }
    Ok(())
}

// ── install / uninstall ─────────────────────────────────────────────

fn write_config(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))
}

fn install(config: &Config, options: InstallOptions) -> Result<()> {
    let config_dir = config.config_dir()?;
    let home = config.home_dir().cloned();
    let (config_path, text, _) = load_user_config(&config_dir)?;
    let updated = sync_service::install_config(&text, &options)?;
    let user = UserSyncConfig::parse(&updated, &config_path.display().to_string())?;
    let interval = user.sync.interval.clone().unwrap_or_else(|| "15m".into());
    let binary = std::env::current_exe().context("locate the path binary")?;
    let files = ServiceFiles::render(&binary, &config_dir, &interval, home.as_deref())?;
    preflight(&user)?;
    write_config(&config_path, &updated)?;
    files.install()?;
    eprintln!(
        "Sync enabled; scheduled every {interval} via {}",
        files.kind()
    );
    status(config)
}

/// Every server and repo the configuration can route a session to.
fn configured_destinations(user: &UserSyncConfig, username: &str) -> Result<Vec<Destination>> {
    let default_url = crate::cmd_pathbase::resolve_url(None);
    let mut out: Vec<Destination> = Vec::new();
    let mut push = |value: &str, origin: &str| -> Result<()> {
        let (repo, base_url) = crate::remote::parse_remote(value, origin)?;
        let destination = Destination {
            repo: format!("{}/{}", repo.owner, repo.name),
            base_url: base_url
                .unwrap_or_else(|| default_url.clone())
                .trim_end_matches('/')
                .to_string(),
        };
        if !out.contains(&destination) {
            out.push(destination);
        }
        Ok(())
    };
    match user.sync.remote.as_deref() {
        Some(remote) => push(remote, "[sync].remote")?,
        None => push(&format!("{username}/pathstash"), "authenticated pathstash")?,
    }
    for rule in &user.project {
        if rule.sync == Some(false) {
            continue;
        }
        if let Some(remote) = rule.remote.as_deref() {
            push(remote, "[[project]].remote")?;
        }
    }
    Ok(out)
}

/// Before anything is written: the credentials work against every
/// configured server and every configured repo is there. A scheduled
/// pass has nobody to tell when these fail.
fn preflight(user: &UserSyncConfig) -> Result<()> {
    let credentials = crate::cmd_pathbase::load_session(&crate::cmd_pathbase::credentials_path()?)?
        .context("sync uploads require login; run `path auth login` first")?;
    let destinations = configured_destinations(user, &credentials.user.username)?;
    let mut checked_servers: Vec<String> = Vec::new();
    for destination in &destinations {
        if !checked_servers.contains(&destination.base_url) {
            let me = crate::cmd_pathbase::api_me(&destination.base_url, &credentials.token)
                .with_context(|| format!("sync cannot reach {}", destination.base_url))?;
            eprintln!(
                "{}: reachable, logged in as {}",
                destination.base_url, me.username
            );
            checked_servers.push(destination.base_url.clone());
        }
        let (owner, name) = destination
            .repo
            .split_once('/')
            .expect("parse_remote yields owner/name");
        crate::cmd_pathbase::repo_get(&destination.base_url, &credentials.token, owner, name)
            .with_context(|| format!("sync cannot upload to {}", destination.repo_url()))?;
        eprintln!("{}: exists", destination.repo_url());
    }
    Ok(())
}

fn uninstall(config: &Config) -> Result<()> {
    let config_dir = config.config_dir()?;
    let home = config.home_dir().cloned();
    let (config_path, text, _) = load_user_config(&config_dir).or_else(|_| {
        let path = config_dir.join(CONFIG_FILE_NAME);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        Ok::<_, anyhow::Error>((path, text, UserSyncConfig::default()))
    })?;
    write_config(&config_path, &sync_service::disable_config(&text)?)?;
    ServiceFiles::remove(home.as_deref())?;
    eprintln!("Sync disabled; upload records and staged operations were kept");
    Ok(())
}

enum ServiceFiles {
    Launchd {
        plist: PathBuf,
        content: String,
    },
    Systemd {
        dir: PathBuf,
        service: String,
        timer: String,
    },
}

impl ServiceFiles {
    fn render(
        binary: &Path,
        config_dir: &Path,
        interval: &str,
        home: Option<&Path>,
    ) -> Result<Self> {
        let home = home.context("cannot install a user service without a home directory")?;
        if cfg!(target_os = "macos") {
            Ok(ServiceFiles::Launchd {
                plist: home
                    .join("Library/LaunchAgents")
                    .join(format!("{LAUNCHD_LABEL}.plist")),
                content: sync_service::launchd_plist(binary, config_dir, interval)?,
            })
        } else if cfg!(target_os = "linux") {
            let files = sync_service::systemd_files(binary, config_dir, interval)?;
            Ok(ServiceFiles::Systemd {
                dir: home.join(".config/systemd/user"),
                service: files.service,
                timer: files.timer,
            })
        } else {
            bail!("scheduling is only supported on macOS (launchd) and Linux (systemd) for now")
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            ServiceFiles::Launchd { .. } => "launchd",
            ServiceFiles::Systemd { .. } => "systemd",
        }
    }

    fn install(&self) -> Result<()> {
        match self {
            ServiceFiles::Launchd { plist, content } => {
                write_config(plist, content)?;
                let domain = launchd_domain();
                let _ = command(
                    "launchctl",
                    &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
                );
                command(
                    "launchctl",
                    &["bootstrap", &domain, &plist.display().to_string()],
                )
                .with_context(|| format!("load {}", plist.display()))
            }
            ServiceFiles::Systemd {
                dir,
                service,
                timer,
            } => {
                write_config(&dir.join(format!("{SERVICE_NAME}.service")), service)?;
                write_config(&dir.join(format!("{SERVICE_NAME}.timer")), timer)?;
                command("systemctl", &["--user", "daemon-reload"])?;
                command(
                    "systemctl",
                    &[
                        "--user",
                        "enable",
                        "--now",
                        &format!("{SERVICE_NAME}.timer"),
                    ],
                )
            }
        }
    }

    fn remove(home: Option<&Path>) -> Result<()> {
        let home = home.context("cannot remove a user service without a home directory")?;
        if cfg!(target_os = "macos") {
            let plist = home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist"));
            let _ = command(
                "launchctl",
                &["bootout", &format!("{}/{LAUNCHD_LABEL}", launchd_domain())],
            );
            remove_if_present(&plist)
        } else if cfg!(target_os = "linux") {
            let _ = command(
                "systemctl",
                &[
                    "--user",
                    "disable",
                    "--now",
                    &format!("{SERVICE_NAME}.timer"),
                ],
            );
            let dir = home.join(".config/systemd/user");
            remove_if_present(&dir.join(format!("{SERVICE_NAME}.timer")))?;
            remove_if_present(&dir.join(format!("{SERVICE_NAME}.service")))?;
            let _ = command("systemctl", &["--user", "daemon-reload"]);
            Ok(())
        } else {
            Ok(())
        }
    }
}

fn launchd_domain() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc_getuid() };
        return format!("gui/{uid}");
    }
    #[allow(unreachable_code)]
    "gui/501".to_string()
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}

fn command(program: &str, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_destinations_cover_the_default_and_every_project_remote_once() {
        let user = UserSyncConfig::parse(
            "[sync]\nremote='me/main'\n[[project]]\ndir='/a'\nremote='https://h.test/u/o/r'\n[[project]]\ndir='/b'\nremote='me/main'\n[[project]]\ndir='/c'\nremote='me/skipped'\nsync=false",
            "c",
        )
        .unwrap();
        let repos: Vec<String> = configured_destinations(&user, "me")
            .unwrap()
            .into_iter()
            .map(|d| d.repo_url())
            .collect();
        assert_eq!(repos.len(), 2);
        assert!(repos[0].ends_with("/u/me/main"));
        assert_eq!(repos[1], "https://h.test/u/o/r");
        let user = UserSyncConfig::parse("[sync]\nenabled=true", "c").unwrap();
        let repos = configured_destinations(&user, "me").unwrap();
        assert_eq!(repos.len(), 1);
        assert!(repos[0].repo_url().ends_with("/u/me/pathstash"));
    }

    #[test]
    fn summary_line_names_only_nonzero_counts() {
        let status = PassStatus {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            dry_run: true,
            include: vec![],
            harnesses: vec![],
            destinations: vec![],
            sessions: 3,
            counts: Counts {
                planned: 2,
                unchanged: 1,
                ..Default::default()
            },
            errors: vec![],
            staged_operations: 0,
        };
        assert_eq!(
            summary_line(&status),
            "dry run, 3 session(s): 1 unchanged, 2 planned"
        );
    }

    #[test]
    fn status_file_roundtrips() {
        let dir = tempfile::TempDir::new().unwrap();
        let status = PassStatus {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            dry_run: false,
            include: vec!["/work".into()],
            harnesses: vec!["codex".into()],
            destinations: vec!["https://h/u/me/stash".into()],
            sessions: 1,
            counts: Counts {
                created: 1,
                ..Default::default()
            },
            errors: vec!["codex abc: boom".into()],
            staged_operations: 0,
        };
        write_status(dir.path(), &status).unwrap();
        let text = std::fs::read_to_string(status_path(dir.path())).unwrap();
        let back: PassStatus = serde_json::from_str(&text).unwrap();
        assert_eq!(back.counts, status.counts);
        assert_eq!(back.errors, status.errors);
    }
}
