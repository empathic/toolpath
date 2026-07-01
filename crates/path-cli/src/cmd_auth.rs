use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use std::path::Path;

use crate::cmd_pathbase::{
    StoredSession, User, api_logout, api_me, api_redeem, clear_session, credentials_path,
    load_session, prompt_line, resolve_url, store_session,
};

#[derive(Subcommand, Debug)]
pub enum AuthOp {
    /// Log in to Pathbase — paste a code from the browser, or a token
    Login {
        /// Pathbase server URL (defaults to $PATHBASE_URL or https://pathbase.dev)
        #[arg(long)]
        url: Option<String>,

        /// Paste the code directly instead of prompting
        #[arg(long)]
        code: Option<String>,

        /// Log in with a token straight from Pathbase, skipping the browser
        /// code exchange. When you're already signed in on the Pathbase site
        /// it hands you a ready-to-paste `path auth login --token <token>`
        /// command; this validates that token and stores the session.
        #[arg(long, conflicts_with = "code")]
        token: Option<String>,
    },
    /// Log out and clear the stored session
    Logout,
    /// Show the stored session's server URL and cached user
    Status,
    /// Verify the stored session against the server and print the current user
    Whoami,
}

pub fn run(op: AuthOp) -> Result<()> {
    let path = credentials_path()?;
    match op {
        AuthOp::Login { url, code, token } => login(&path, url, code, token),
        AuthOp::Logout => logout(&path),
        AuthOp::Status => status(&path),
        AuthOp::Whoami => whoami(&path),
    }
}

fn login(
    path: &Path,
    url: Option<String>,
    code_arg: Option<String>,
    token_arg: Option<String>,
) -> Result<()> {
    let base_url = resolve_url(url);

    if let Some(raw) = token_arg {
        return login_with_token(path, &base_url, raw);
    }

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
            println!("  (already signed in there? paste the one-line");
            println!("   `path auth login --token <token>` command instead)");
            println!();
            prompt_line("Paste code: ")?
        }
    };

    let (token, user) = api_redeem(&base_url, &code)?;
    finish_login(path, &base_url, token, user)
}

/// Sign in with a token issued directly by Pathbase, no browser round-trip.
///
/// The token is used as-is as the bearer credential; we validate it (and
/// resolve the account it belongs to) with a `GET /users/me` before writing
/// it to disk, so a bad or expired token fails here instead of silently at
/// the next upload.
fn login_with_token(path: &Path, base_url: &str, raw: String) -> Result<()> {
    let token = raw.trim().to_string();
    if token.is_empty() {
        bail!("--token was empty; paste the token from {base_url}/auth/cli");
    }

    let user = api_me(base_url, &token)
        .with_context(|| format!("failed to sign in to {base_url} with the provided token"))?;
    finish_login(path, base_url, token, user)
}

/// Persist the session and print the "logged in as …" confirmation. Shared
/// by the code-exchange and token login flows.
fn finish_login(path: &Path, base_url: &str, token: String, user: User) -> Result<()> {
    store_session(
        path,
        &StoredSession {
            url: base_url.to_string(),
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
