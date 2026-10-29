//! `toolpath-agents list|fetch|path|run`: the agent CLIs the tests run, through `agents.lock` only.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use toolpath_agents::{Cache, Hermetic, Lock, host_target};

#[derive(Parser, Debug)]
#[command(
    name = "toolpath-agents",
    about = "Hash-verified agent CLIs for toolpath's tests"
)]
struct Cli {
    /// The lock file
    #[arg(long, global = true, default_value = "agents.lock")]
    lock: PathBuf,
    /// Rust target triple (default: this machine)
    #[arg(long, global = true)]
    target: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// What the lock names and whether each entry is fetched
    List,
    /// Download an agent from the lock's url, verify its sha256, unpack it; prints the executable
    Fetch { agent: String },
    /// The installed executable of an already fetched agent
    Path { agent: String },
    /// Run a fetched agent hermetically (empty env, temp HOME, cwd /), passing ARGS
    Run {
        agent: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let lock = Lock::parse(
        &std::fs::read_to_string(&cli.lock)
            .with_context(|| format!("read {}", cli.lock.display()))?,
    )?;
    let cache = Cache::default_location()?;
    let target = cli.target.unwrap_or_else(|| host_target().to_string());
    match cli.command {
        Cmd::List => {
            for a in &lock.agents {
                for p in &a.platforms {
                    let state = if cache.lookup(&p.sha256).is_some() {
                        "fetched"
                    } else {
                        "not fetched"
                    };
                    println!(
                        "{} {} {} {} {state}",
                        a.name,
                        a.version,
                        p.target,
                        &p.sha256[..12]
                    );
                }
            }
        }
        Cmd::Fetch { agent } => {
            let (_, p) = lock.find(&agent, &target)?;
            cache.fetch(p)?;
            println!("{}", cache.install(p)?.display());
        }
        Cmd::Path { agent } => {
            let (_, p) = lock.find(&agent, &target)?;
            println!("{}", cache.install(p)?.display());
        }
        Cmd::Run { agent, args } => {
            let (_, p) = lock.find(&agent, &target)?;
            let h = Hermetic::new(cache.install(p)?)?;
            let status = h.command(&args).status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    Ok(())
}
