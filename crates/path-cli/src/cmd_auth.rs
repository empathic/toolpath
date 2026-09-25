use anyhow::{Context, Result, anyhow};
use clap::Args;
use std::io::IsTerminal;

use crate::store::{self, S3Settings};
use clap::Subcommand;
use std::path::{Path, PathBuf};

use crate::cmd_pathbase::{
    StoredSession, api_logout, api_me, api_redeem, clear_session, credentials_path, load_session,
    prompt_line, resolve_url, store_session,
};

#[derive(Subcommand, Debug)]
pub enum AuthOp {
    /// Log in by opening a browser to Pathbase and pasting the displayed code
    Login {
        /// Pathbase server URL (defaults to $PATHBASE_URL or https://pathbase.dev)
        #[arg(long)]
        url: Option<String>,

        /// Paste the code directly instead of prompting
        #[arg(long)]
        code: Option<String>,
    },
    /// Log out and clear the stored session
    Logout,
    /// Show the stored session's server URL and cached user
    Status,
    /// Verify the stored session against the server and print the current user
    Whoami,
    /// Store S3 credentials for endpoints AWS tooling doesn't know
    /// about (MinIO, R2, Ceph). Your `~/.aws` profiles — SSO included —
    /// are picked up automatically and need none of this.
    S3 {
        #[command(subcommand)]
        op: S3Op,
    },
}

#[derive(Subcommand, Debug)]
pub enum S3Op {
    /// Store S3 credentials and connection settings.
    ///
    /// Only the fields you pass are updated; the rest keep their stored
    /// values, so `path auth s3 login --region eu-west-1` is a valid
    /// tweak. Run interactively with no flags and it prompts, without
    /// echoing the secret.
    ///
    /// `--to <destination>` sets where shares go by default: it writes
    /// `[share] remote` in `~/.toolpath/config.toml`, after which
    /// `path share`, `path resume`, `p list object`, and `p export
    /// object` need no destination. The connection settings stay
    /// separate from it, so one stored credential serves any number of
    /// buckets (`--to` on a single command, or a `[[project]]` remote).
    #[command(alias = "set")]
    Login {
        #[command(flatten)]
        args: Box<S3LoginArgs>,
    },
    /// Show the S3 settings in effect, with secrets redacted and
    /// environment-supplied values marked
    Status,
    /// Ask STS who the resolved credentials belong to (account, ARN,
    /// user ID). Runs `aws sts get-caller-identity` with the credentials
    /// this CLI would use, so it works for stored keys, environment
    /// keys, and any AWS profile alike.
    Whoami,
    /// Forget the stored S3 settings
    #[command(alias = "clear")]
    Logout,
}

#[derive(Args, Debug, Default)]
pub struct S3LoginArgs {
    /// AWS region (default: us-east-1)
    #[arg(long)]
    pub region: Option<String>,

    /// Endpoint URL for an S3-compatible service (R2, MinIO, Ceph).
    /// Omit for real AWS S3.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Access key ID (stored in ~/.toolpath/s3.json, 0600)
    #[arg(long)]
    pub access_key_id: Option<String>,

    /// Secret access key. Prefer omitting this so it's prompted for
    /// rather than landing in your shell history.
    #[arg(long)]
    pub secret_access_key: Option<String>,

    /// Session token for temporary (STS / assumed-role) credentials
    #[arg(long)]
    pub session_token: Option<String>,

    /// AWS profile to resolve credentials from, instead of storing keys.
    /// Works with SSO and assume-role profiles — those are resolved
    /// through the AWS CLI, so nothing expires in our config.
    #[arg(long)]
    pub profile: Option<String>,

    /// Address the bucket as `bucket.host/key` instead of `host/bucket/key`
    #[arg(long)]
    pub virtual_hosted_style: bool,

    /// Refuse to replace existing objects on every export (create-only
    /// puts). Pass --no-overwrite on a single export for a one-off.
    #[arg(long, conflicts_with = "overwrite")]
    pub no_overwrite: bool,

    /// Clear a stored --no-overwrite
    #[arg(long)]
    pub overwrite: bool,

    /// Server-side encryption for uploads: `AES256` or `aws:kms`
    #[arg(long, value_name = "ALGORITHM")]
    pub sse: Option<String>,

    /// KMS key ID or ARN for `--sse aws:kms`
    #[arg(long, value_name = "KEY", requires = "sse")]
    pub kms_key_id: Option<String>,

    /// Default destination for `share`, `resume`, `p list object`, and
    /// `p export object`: `s3://bucket/prefix`, or a folder. Written to
    /// `[share] remote` in `~/.toolpath/config.toml`; a `[[project]]`
    /// rule or a per-command `--to` still overrides it. May be given on
    /// its own, with nothing else to store.
    #[arg(long, value_name = "DESTINATION")]
    pub to: Option<String>,
}

pub fn run(op: AuthOp, config: &crate::config::Config) -> Result<()> {
    match op {
        AuthOp::S3 { op } => run_s3(op, config),
        other => {
            let path = credentials_path()?;
            match other {
                AuthOp::Login { url, code } => login(&path, url, code),
                AuthOp::Logout => logout(&path),
                AuthOp::Status => status(&path),
                AuthOp::Whoami => whoami(&path),
                AuthOp::S3 { .. } => unreachable!("handled above"),
            }
        }
    }
}

fn login(path: &Path, url: Option<String>, code_arg: Option<String>) -> Result<()> {
    let base_url = resolve_url(url);
    let auth_url = format!("{base_url}/auth/cli");

    let code = match code_arg {
        Some(c) => c,
        None => {
            println!("To connect this CLI to Pathbase:");
            println!();
            println!("  1. Open {auth_url} in your browser");
            println!("  2. Sign in if prompted");
            println!("  3. Copy the 8-character code shown on that page");
            println!();
            prompt_line("Paste code: ")?
        }
    };

    let (token, user) = api_redeem(&base_url, &code)?;
    store_session(
        path,
        &StoredSession {
            url: base_url.clone(),
            token,
            user: user.clone(),
        },
    )?;

    println!(
        "Logged in to {} as {}{}",
        base_url,
        user.username,
        user.email
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );
    println!("Credentials saved to {}", path.display());
    Ok(())
}

fn logout(path: &Path) -> Result<()> {
    let stored = match load_session(path)? {
        Some(s) => s,
        None => {
            println!("Not logged in.");
            return Ok(());
        }
    };

    if let Err(e) = api_logout(&stored.url, &stored.token) {
        eprintln!("warning: server logout failed: {e}");
    }

    clear_session(path)?;
    println!("Logged out.");
    Ok(())
}

fn status(path: &Path) -> Result<()> {
    match load_session(path)? {
        Some(s) => {
            println!("Logged in to {} as {}", s.url, s.user.username);
            if let Some(email) = &s.user.email {
                println!("  email: {email}");
            }
            println!("  user id: {}", s.user.id);
            println!("  credentials: {}", path.display());
            Ok(())
        }
        None => {
            println!("Not logged in. Run `path auth login`.");
            Ok(())
        }
    }
}

// ── S3 ──────────────────────────────────────────────────────────────────

fn run_s3(op: S3Op, config: &crate::config::Config) -> Result<()> {
    let path = store::config_path()?;
    match op {
        S3Op::Login { args } => s3_login(&path, *args, config),
        S3Op::Status => s3_status(&path, config),
        S3Op::Whoami => s3_whoami(),
        S3Op::Logout => s3_logout(&path),
    }
}

/// Merge `args` into whatever is already stored, prompting for the
/// essentials when nothing was passed and we have a terminal.
///
/// Merge rather than replace: partial updates are the common case
/// (rotating a key, switching endpoint), and a replace would silently
/// drop the fields the user didn't repeat.
fn s3_login(path: &Path, mut args: S3LoginArgs, config: &crate::config::Config) -> Result<()> {
    let default_to = args.to.take();
    let mut cfg = store::load_stored(path)?.unwrap_or_default();
    let had_settings = cfg != S3Settings::default();
    let before = cfg.clone();

    let set = |slot: &mut Option<String>, value: Option<String>| {
        if let Some(v) = value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            *slot = Some(v);
        }
    };
    set(&mut cfg.region, args.region);
    set(&mut cfg.endpoint, args.endpoint);
    set(&mut cfg.access_key_id, args.access_key_id);
    set(&mut cfg.secret_access_key, args.secret_access_key);
    set(&mut cfg.session_token, args.session_token);
    set(&mut cfg.profile, args.profile);
    if args.virtual_hosted_style {
        cfg.virtual_hosted_style = Some(true);
    }
    if args.no_overwrite {
        cfg.no_overwrite = Some(true);
    }
    if args.overwrite {
        cfg.no_overwrite = None;
    }
    set(&mut cfg.server_side_encryption, args.sse);
    set(&mut cfg.sse_kms_key_id, args.kms_key_id);

    // `--to` on its own changes nothing in the connection settings, so
    // it neither prompts for credentials nor complains that there is
    // nothing to store: for anyone whose `~/.aws` profile already
    // works, it is the whole login.
    if let Some(to) = &default_to
        && cfg == before
    {
        return set_default_destination(to, config);
    }

    if std::io::stdin().is_terminal() && !had_settings {
        prompt_missing(&mut cfg)?;
    }

    if cfg == S3Settings::default() {
        anyhow::bail!(
            "Nothing to store. Run `path auth s3 login` from a terminal to be \
             prompted, or pass `--access-key-id AKIA…` and you'll be prompted \
             for the secret. `--secret-access-key` on the command line exists \
             for scripts, but it lands in shell history."
        );
    }

    store::store(path, &cfg)?;
    println!("S3 settings saved to {}", path.display());
    // Everything printed here was just written, so nothing is `(env)`.
    print_settings(&cfg, &cfg);
    if let Some(to) = &default_to {
        set_default_destination(to, config)?;
    }
    Ok(())
}

/// Write `to` as `[share] remote` in the config file and say what it
/// now means.
fn set_default_destination(to: &str, config: &crate::config::Config) -> Result<()> {
    let file = config.config_dir()?.join(crate::config::CONFIG_FILE_NAME);
    let home = config.home_dir().map(PathBuf::as_path);
    crate::share_config::write_default_remote(&file, home, to)?;
    println!(
        "Default destination set to {to} in {}",
        crate::config::home_relative(&file, home)
    );
    println!(
        "  `path share` exports there, `path resume` browses it, and \
         `p list object` / `p export object` need no destination."
    );
    Ok(())
}

/// First-time interactive setup. Skipped when settings already exist,
/// so a targeted `--region` update doesn't re-interrogate the user
/// about credentials they already stored.
fn prompt_missing(cfg: &mut S3Settings) -> Result<()> {
    println!("Store S3 connection settings for `s3://` share and resume targets.");
    println!();
    println!("If you already use the AWS CLI, you probably need none of this — your");
    println!("`~/.aws` profiles are picked up automatically, including SSO. This is");
    println!("for endpoints the AWS tooling doesn't know about (MinIO, R2, Ceph).");
    println!("Leave any field blank to skip it.");
    println!();

    if cfg.region.is_none() {
        let v = prompt_line(&format!("Region [{}]: ", store::DEFAULT_REGION))?;
        cfg.region = Some(v).filter(|v| !v.is_empty());
    }
    if cfg.endpoint.is_none() {
        let v = prompt_line("Endpoint URL (blank for AWS): ")?;
        cfg.endpoint = Some(v).filter(|v| !v.is_empty());
    }
    if cfg.access_key_id.is_none() {
        let v = prompt_line("Access key id (blank to use the AWS environment): ")?;
        cfg.access_key_id = Some(v).filter(|v| !v.is_empty());
    }
    if cfg.access_key_id.is_some() && cfg.secret_access_key.is_none() {
        let v = rpassword::prompt_password("Secret access key: ")?;
        cfg.secret_access_key = Some(v.trim().to_string()).filter(|v| !v.is_empty());
    }
    Ok(())
}

fn s3_status(path: &Path, config: &crate::config::Config) -> Result<()> {
    let stored = store::load_stored(path)?;
    let effective = store::effective_settings()?;

    match &stored {
        Some(_) => println!("S3 settings in {}", path.display()),
        None => println!("No stored S3 settings ({} does not exist).", path.display()),
    }
    if effective != S3Settings::default() {
        print_settings(&effective, &stored.unwrap_or_default());
    }
    // The default destination for share, resume, list, and export;
    // same column as the settings above.
    match crate::share_config::default_object_destination(config)? {
        Some(found) => println!(
            "  {:<19}{} ({})",
            "destination:", found.display, found.origin
        ),
        None => println!(
            "  {:<19}none (`path auth s3 login --to s3://bucket/prefix` sets one)",
            "destination:"
        ),
    }
    let resolved = effective.resolve_real();
    print_credential_source(&effective, &resolved);
    // Advice only when it would change anything: someone whose profile
    // already resolves has nothing to store.
    if matches!(&resolved, Ok(r) if r.source == crate::aws_creds::Source::InstanceChain) {
        println!("Run `path auth s3 login` to store credentials, or configure an AWS profile.");
    }
    Ok(())
}

fn s3_logout(path: &Path) -> Result<()> {
    if store::load_stored(path)?.is_none() {
        println!("No stored S3 settings.");
        return Ok(());
    }
    store::clear(path)?;
    println!("S3 settings cleared.");
    Ok(())
}

fn s3_whoami() -> Result<()> {
    let effective = store::effective_settings()?;
    let resolved = effective.resolve_real()?;
    let Some(creds) = &resolved.credentials else {
        anyhow::bail!(
            "no local credentials to identify ({}); on a host with an instance role, run \
             `aws sts get-caller-identity` directly",
            resolved.source
        );
    };
    let region = effective
        .region
        .clone()
        .or_else(|| resolved.region.clone())
        .unwrap_or_else(|| store::DEFAULT_REGION.to_string());

    let mut command = std::process::Command::new("aws");
    command
        .args(["sts", "get-caller-identity", "--output", "json"])
        .env("AWS_ACCESS_KEY_ID", &creds.access_key_id)
        .env("AWS_SECRET_ACCESS_KEY", &creds.secret_access_key)
        .env("AWS_REGION", &region)
        .env_remove("AWS_PROFILE");
    match &creds.session_token {
        Some(t) => {
            command.env("AWS_SESSION_TOKEN", t);
        }
        None => {
            command.env_remove("AWS_SESSION_TOKEN");
        }
    }
    let out = command.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!(
            "`aws` isn't on PATH; `path auth s3 whoami` asks STS through the AWS CLI. \
             Install it, or run `aws sts get-caller-identity` wherever it is installed."
        ),
        _ => anyhow!("running `aws sts get-caller-identity`: {e}"),
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "`aws sts get-caller-identity` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("the AWS CLI returned output that isn't JSON")?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("?").to_string();
    println!("{:<13}{}", "arn:", field("Arn"));
    println!("{:<13}{}", "account:", field("Account"));
    println!("{:<13}{}", "user id:", field("UserId"));
    println!("{:<13}{}", "credentials:", resolved.source);
    Ok(())
}

/// Print `effective`, tagging any field that `stored` didn't supply as
/// `(env)` — otherwise "where did this endpoint come from?" is a guess.
fn print_settings(effective: &S3Settings, stored: &S3Settings) {
    let line = |label: &str, value: Option<&str>, from_store: bool| {
        if let Some(v) = value {
            let origin = if from_store { "" } else { " (env)" };
            // Width matches the longest label so the values line up.
            println!("  {:<19}{v}{origin}", format!("{label}:"));
        }
    };
    line(
        "region",
        effective.region.as_deref(),
        stored.region.is_some(),
    );
    line(
        "endpoint",
        effective.endpoint.as_deref(),
        stored.endpoint.is_some(),
    );
    line(
        "access key id",
        effective.access_key_id.as_deref(),
        stored.access_key_id.is_some(),
    );
    line(
        "secret access key",
        effective
            .secret_access_key
            .as_deref()
            .map(redact)
            .as_deref(),
        stored.secret_access_key.is_some(),
    );
    line(
        "session token",
        effective.session_token.as_deref().map(redact).as_deref(),
        stored.session_token.is_some(),
    );
    line(
        "profile",
        effective.profile.as_deref(),
        stored.profile.is_some(),
    );
    line(
        "encryption",
        effective.server_side_encryption.as_deref(),
        stored.server_side_encryption.is_some(),
    );
    line(
        "kms key id",
        effective.sse_kms_key_id.as_deref(),
        stored.sse_kms_key_id.is_some(),
    );
    if effective.no_overwrite == Some(true) {
        println!(
            "  {:<19}create-only (existing objects are never replaced)",
            "put mode:"
        );
    }
}

/// Say which credentials a share would actually use, and as which key.
///
/// The first question when an upload fails is *which* credential was
/// tried — a stored key, an AWS profile, or nothing at all are three
/// completely different fixes, and only this line distinguishes them.
/// The key ID is printed for every source (never the secret) so the
/// answer can be matched against IAM.
fn print_credential_source(effective: &S3Settings, resolved: &Result<crate::aws_creds::Resolved>) {
    match resolved {
        Ok(r) => {
            println!("  {:<19}{}", "credentials:", r.source);
            if let Some(c) = &r.credentials
                && effective.access_key_id.is_none()
            {
                println!("  {:<19}{}", "access key id:", c.access_key_id);
            }
            if effective.region.is_none() {
                match &r.region {
                    Some(region) => println!("  {:<19}{region} (from the profile)", "region:"),
                    None => {
                        println!("  {:<19}{} (default)", "region:", store::DEFAULT_REGION)
                    }
                }
            }
        }
        // The reason *is* the answer here — "no such profile" tells the
        // user exactly what to fix.
        Err(e) => println!("  {:<19}unresolved — {e:#}", "credentials:"),
    }
}

/// Show enough of a secret to recognize which one it is, and no more.
fn redact(secret: &str) -> String {
    let tail: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("****{tail}")
}

fn whoami(path: &Path) -> Result<()> {
    let stored =
        load_session(path)?.ok_or_else(|| anyhow!("Not logged in. Run `path auth login`."))?;
    let user = api_me(&stored.url, &stored.token)?;
    println!("{} ({})", user.username, user.id);
    if let Some(email) = &user.email {
        println!("email: {email}");
    }
    println!("server: {}", stored.url);
    Ok(())
}
