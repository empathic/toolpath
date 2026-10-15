//! Standalone recorder/replayer, env-configured so a shell harness (the resume matrix, the goldens'
//! capture-live) can point an agent's base URL at it:
//!
//!   WEBMOCK_MODE=record WEBMOCK_UPSTREAM=https://api.anthropic.com WEBMOCK_AGENT=claude \
//!     WEBMOCK_DIR=crates/toolpath-webmock/cassettes WEBMOCK_PORT=8900 webmock
//!   WEBMOCK_MODE=replay WEBMOCK_AGENT=claude WEBMOCK_DIR=… WEBMOCK_PORT=8900 webmock
//!
//! Cassettes land in `WEBMOCK_DIR/<agent>/`. `GET /__webmock/health` reports the mode.

use std::sync::Arc;

use toolpath_webmock::{Config, Mode, Store, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        },
        port,
    )
    .await?;
    handle.await?;
    Ok(())
}
