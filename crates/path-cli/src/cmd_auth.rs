use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use std::io::IsTerminal;
use std::path::Path;

use crate::cmd_pathbase::{
    StoredSession, api_logout, api_me, api_redeem, clear_session, credentials_path, load_session,
    prompt_line, resolve_url, store_session,
};
use crate::store::{self, S3Settings};
use crate::target;

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
    /// Store S3 credentials once, so `s3://` share and resume targets
    /// need none on the command line. A folder target needs no
    /// credentials and so never needs this.
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
    /// This does not set *where* shares go — that's `path target`
    /// — so one stored credential serves any number of buckets.
    #[command(alias = "set")]
    Login {
        #[command(flatten)]
        args: S3LoginArgs,
    },
    /// Show the S3 settings in effect, with secrets redacted and
    /// environment-supplied values marked
    Status,
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

    #[arg(long)]
    pub access_key_id: Option<String>,

    /// Secret access key. Prefer omitting this so it's prompted for
    /// rather than landing in your shell history.
    #[arg(long)]
    pub secret_access_key: Option<String>,

    /// Session token for temporary (STS / assumed-role) credentials
    #[arg(long)]
    pub session_token: Option<String>,

    /// Address the bucket as `bucket.host/key` instead of `host/bucket/key`
    #[arg(long)]
    pub virtual_hosted_style: bool,
}

pub fn run(op: AuthOp) -> Result<()> {
    match op {
        AuthOp::S3 { op } => run_s3(op),
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

fn run_s3(op: S3Op) -> Result<()> {
    let path = store::config_path()?;
    match op {
        S3Op::Login { args } => s3_login(&path, args),
        S3Op::Status => s3_status(&path),
        S3Op::Logout => s3_logout(&path),
    }
}

/// Merge `args` into whatever is already stored, prompting for the
/// essentials when nothing was passed and we have a terminal.
///
/// Merge rather than replace: partial updates are the common case
/// (rotating a key, switching endpoint), and a replace would silently
/// drop the fields the user didn't repeat.
fn s3_login(path: &Path, args: S3LoginArgs) -> Result<()> {
    let mut cfg = store::load_stored(path)?.unwrap_or_default();
    let had_settings = cfg != S3Settings::default();

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
    if args.virtual_hosted_style {
        cfg.virtual_hosted_style = Some(true);
    }

    if std::io::stdin().is_terminal() && !had_settings {
        prompt_missing(&mut cfg)?;
    }

    if cfg == S3Settings::default() {
        anyhow::bail!(
            "Nothing to store. Pass at least one setting (e.g. \
             `path auth s3 login --access-key-id AKIA… --secret-access-key …`), \
             or run this from a terminal to be prompted."
        );
    }

    store::store(path, &cfg)?;
    println!("S3 settings saved to {}", path.display());
    // Everything printed here was just written, so nothing is `(env)`.
    print_settings(&cfg, &cfg);
    suggest_default_target()?;
    Ok(())
}

/// First-time interactive setup. Skipped when settings already exist,
/// so a targeted `--region` update doesn't re-interrogate the user
/// about credentials they already stored.
fn prompt_missing(cfg: &mut S3Settings) -> Result<()> {
    println!("Store S3 credentials for `s3://` share and resume targets.");
    println!("Leave a field blank to skip it (credentials can come from the AWS environment).");
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

/// Credentials alone don't make `path share` go to S3 — the target
/// does. Close that gap in the same breath rather than letting the user
/// discover it on their next share.
fn suggest_default_target() -> Result<()> {
    if target::default_target()?.0.is_some() {
        return Ok(());
    }
    println!();
    println!("`path share` still uploads to Pathbase. To make S3 the default:");
    println!("  path target s3://my-bucket/traces");
    Ok(())
}

fn s3_status(path: &Path) -> Result<()> {
    let stored = store::load_stored(path)?;
    let effective = store::effective_settings()?;

    match &stored {
        Some(_) => println!("S3 settings in {}", path.display()),
        None => println!("No stored S3 settings ({} does not exist).", path.display()),
    }
    if effective == S3Settings::default() {
        println!("Run `path auth s3 login` to store some.");
    } else {
        print_settings(&effective, &stored.unwrap_or_default());
    }
    println!("Share target: {}", target::describe_effective()?);
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
    if effective.access_key_id.is_none() {
        println!("  credentials: none stored — falling back to the AWS credential chain");
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
