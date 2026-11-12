use anyhow::{Result, anyhow};
use clap::Subcommand;
use std::path::Path;

use crate::cmd_pathbase::{
    StoredSession, api_logout, api_me, api_redeem, clear_session_for, credentials_path,
    load_all_sessions, load_session, load_session_for, prompt_line, resolve_session_url,
    resolve_url, session_key, store_session,
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
    /// Log out of one server and clear its stored session
    Logout {
        /// Server to log out of (defaults to $PATHBASE_URL, then the most recent login)
        #[arg(long)]
        url: Option<String>,
    },
    /// Show the stored sessions: every server, or just the one named
    Status {
        /// Show only this server's session
        #[arg(long)]
        url: Option<String>,
    },
    /// Verify a stored session against its server and print the current user
    Whoami {
        /// Server to check (defaults to $PATHBASE_URL, then the most recent login)
        #[arg(long)]
        url: Option<String>,
    },
}

pub fn run(op: AuthOp) -> Result<()> {
    let path = credentials_path()?;
    match op {
        AuthOp::Login { url, code } => login(&path, url, code),
        AuthOp::Logout { url } => logout(&path, url),
        AuthOp::Status { url } => status(&path, url),
        AuthOp::Whoami { url } => whoami(&path, url),
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

fn logout(path: &Path, url: Option<String>) -> Result<()> {
    let base_url = resolve_session_url(url);
    let stored = match load_session_for(path, &base_url)? {
        Some(s) => s,
        None => {
            println!("Not logged in to {base_url}.");
            return Ok(());
        }
    };

    if let Err(e) = api_logout(&stored.url, &stored.token) {
        eprintln!("warning: server logout failed: {e}");
    }

    clear_session_for(path, &stored.url)?;
    println!("Logged out of {}.", stored.url);
    Ok(())
}

fn status(path: &Path, url: Option<String>) -> Result<()> {
    let sessions = match &url {
        Some(u) => load_session_for(path, u)?.into_iter().collect(),
        None => load_all_sessions(path)?,
    };
    if sessions.is_empty() {
        match url {
            Some(u) => println!("Not logged in to {u}. Run `path auth login --url {u}`."),
            None => println!("Not logged in. Run `path auth login`."),
        }
        return Ok(());
    }

    // The login commands fall back to when nothing names a server.
    let default_key = load_session(path)?.map(|s| session_key(&s.url));
    for s in &sessions {
        let is_default = default_key.as_deref() == Some(session_key(&s.url).as_str());
        println!(
            "Logged in to {} as {}{}",
            s.url,
            s.user.username,
            if is_default { " (default)" } else { "" }
        );
        if let Some(email) = &s.user.email {
            println!("  email: {email}");
        }
        println!("  user id: {}", s.user.id);
    }
    println!("  credentials: {}", path.display());
    println!("  (stored, not verified — `path auth whoami` checks with the server)");
    Ok(())
}

fn whoami(path: &Path, url: Option<String>) -> Result<()> {
    let base_url = resolve_session_url(url);
    let stored = load_session_for(path, &base_url)?.ok_or_else(|| {
        anyhow!("Not logged in to {base_url}. Run `path auth login --url {base_url}`.")
    })?;
    let user = api_me(&stored.url, &stored.token)?;
    println!("{} ({})", user.username, user.id);
    if let Some(email) = &user.email {
        println!("email: {email}");
    }
    println!("server: {}", stored.url);
    Ok(())
}
