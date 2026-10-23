//! The record/replay server, and a launcher that points one agent at it. Also reachable as
//! `path d webmock …` (path-cli built with `--features develop`).
//!
//! ```text
//! webmock run <agent> replay --cell <dir> [--mask <live>=<placeholder>]… -- <agent args, {sid} {project} {home} {base}>
//! webmock run <agent> record --cell <dir> --upstream <url> [--github-upstream <url>] [--pass-env NAME]… -- …
//! webmock serve        # env-configured, for a shell harness:
//! WEBMOCK_MODE=record WEBMOCK_UPSTREAM=https://api.anthropic.com WEBMOCK_AGENT=claude \
//!   WEBMOCK_DIR=crates/toolpath-webmock/cassettes WEBMOCK_PORT=8900 webmock serve
//! WEBMOCK_MODE=replay WEBMOCK_AGENT=claude WEBMOCK_DIR=… WEBMOCK_PORT=8900 webmock serve
//! ```
//!
//! Cassettes live in `<dir>/<agent>/`. `GET /__webmock/health` reports the mode. With no
//! subcommand, `webmock` serves as `serve` does.

use std::sync::Arc;

use toolpath_webmock::run::{self, USAGE};
use toolpath_webmock::{Config, Mode, Store, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run_one(&args[1..]).await,
        None | Some("serve") => serve_env().await,
        Some("-h" | "--help" | "help") => {
            println!(
                "{USAGE}\n   or: webmock serve (WEBMOCK_MODE, WEBMOCK_AGENT, WEBMOCK_DIR, WEBMOCK_PORT, WEBMOCK_UPSTREAM)"
            );
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n{USAGE}"),
    }
}

async fn run_one(args: &[String]) -> anyhow::Result<()> {
    let opts = run::parse(args)?;
    let (agent, keep) = (opts.agent, opts.keep);
    let out = run::run(opts).await?;
    let t = &out.tally;
    println!(
        "{} rc={} {}s  exact={} loose={} miss={} recorded={}",
        agent.name(),
        out.rc
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        out.seconds,
        t.exact,
        t.loose,
        t.miss,
        t.recorded
    );
    for l in out.lines.iter().filter(|l| {
        ["EXACT", "LOOSE", "MISS", "recorded", "  jsonrpc"]
            .iter()
            .any(|w| l.starts_with(w))
    }) {
        println!("  {l}");
    }
    let reply = std::fs::read_to_string(out.run_dir.join("agent.out")).unwrap_or_default();
    let tail: String = reply
        .chars()
        .rev()
        .take(300)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    println!("agent stdout (tail): {}", tail.trim());
    if keep {
        println!("kept: {}", out.run_dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&out.run_dir);
    }
    if t.miss > 0 || out.rc != Some(0) {
        std::process::exit(1);
    }
    Ok(())
}

async fn serve_env() -> anyhow::Result<()> {
    let agent = std::env::var("WEBMOCK_AGENT").unwrap_or_else(|_| "unknown".into());
    let dir = std::env::var("WEBMOCK_DIR").unwrap_or_else(|_| "cassettes".into());
    let port: u16 = std::env::var("WEBMOCK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let mode = match std::env::var("WEBMOCK_MODE")
        .unwrap_or_else(|_| "replay".into())
        .as_str()
    {
        "record" => {
            let upstream = std::env::var("WEBMOCK_UPSTREAM")
                .map_err(|_| anyhow::anyhow!("record mode needs WEBMOCK_UPSTREAM"))?;
            Mode::Record {
                upstream: upstream.trim_end_matches('/').to_string(),
            }
        }
        _ => Mode::Replay,
    };
    let store = Arc::new(Store::open(std::path::Path::new(&dir).join(&agent))?);
    let tag = agent.clone();
    let log: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |line: &str| eprintln!("[webmock/{tag}] {line}"));
    if matches!(mode, Mode::Replay) {
        log(&format!("loaded {} cassette(s)", store.len()));
    }
    let (_addr, handle) = serve(
        Config {
            agent,
            mode,
            store,
            log,
            masks: Vec::new(),
        },
        port,
    )
    .await?;
    handle.await?;
    Ok(())
}
