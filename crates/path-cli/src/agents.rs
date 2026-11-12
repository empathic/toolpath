//! `path d agents`: the agent CLIs the tests and replay run, resolved ONLY through `agents.lock`.
//!
//! The lock names, per agent and platform, a version, the url to fetch, the sha256 of what that
//! url serves, and the codesign team id (for the human who reviews a bump). `fetch` downloads the
//! url, verifies the hash and refuses on a mismatch, then keeps the bytes in a content-addressed
//! cache (`$XDG_CACHE_HOME/toolpath/agents/<sha256>/blob`). Nothing is put on `PATH` and nothing
//! is executed here: the cache path is what a test is handed.
//!
//! A lock entry whose sha256 is `PENDING` is a placeholder for a source nobody has approved yet;
//! `fetch` refuses it. Nothing in this module downloads unless `fetch` is run on an entry.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Lock {
    #[serde(default, rename = "agent")]
    pub agents: Vec<Agent>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub version: String,
    /// What the blob needs to run (`native`, `node`, ...); informational for the human reviewing.
    pub runtime: Option<String>,
    #[serde(default, rename = "platform")]
    pub platforms: Vec<Platform>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Platform {
    /// Rust-style target, e.g. `aarch64-apple-darwin`.
    pub target: String,
    pub url: String,
    pub sha256: String,
    /// Apple Developer team id the binary must be signed by (checked by a human on a bump).
    pub team_id: Option<String>,
}

pub const PENDING: &str = "PENDING";

impl Lock {
    pub fn parse(text: &str) -> Result<Lock> {
        Ok(toml::from_str(text)?)
    }

    pub fn find(&self, agent: &str, target: &str) -> Result<(&Agent, &Platform)> {
        let a = self
            .agents
            .iter()
            .find(|a| a.name == agent)
            .ok_or_else(|| anyhow!("agent {agent:?} is not in the lock"))?;
        let p = a
            .platforms
            .iter()
            .find(|p| p.target == target)
            .ok_or_else(|| anyhow!("{agent} {} has no {target} entry in the lock", a.version))?;
        Ok((a, p))
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `$XDG_CACHE_HOME/toolpath/agents`, else `$HOME/.cache/toolpath/agents`.
pub fn cache_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("no HOME"))?)
            .join(".cache"),
    };
    Ok(base.join("toolpath/agents"))
}

fn blob_path(cache: &Path, sha: &str) -> PathBuf {
    cache.join(sha).join("blob")
}

/// Keep `bytes` under their sha256, which must be `expected`. Refuses otherwise. The stored file
/// has no execute bit: making it runnable is a separate, deliberate step of whoever unpacks it.
pub fn store(cache: &Path, expected: &str, bytes: &[u8]) -> Result<PathBuf> {
    if expected == PENDING {
        bail!("the lock entry is PENDING: no source has been approved for it");
    }
    let got = sha256_hex(bytes);
    if got != expected {
        bail!("sha256 mismatch: the lock says {expected}, the download is {got}");
    }
    let path = blob_path(cache, expected);
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The cached blob for `sha`, re-hashed on every lookup (a cache entry that no longer matches its
/// name is treated as absent, never trusted).
pub fn lookup(cache: &Path, sha: &str) -> Option<PathBuf> {
    let p = blob_path(cache, sha);
    let bytes = std::fs::read(&p).ok()?;
    (sha256_hex(&bytes) == sha).then_some(p)
}

fn host_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

#[derive(Args, Debug)]
pub struct AgentsArgs {
    /// The lock file
    #[arg(long, global = true, default_value = "agents.lock")]
    pub lock: PathBuf,
    #[command(subcommand)]
    pub command: AgentsCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentsCommand {
    /// List what the lock names and whether each entry is cached
    List,
    /// Download one agent's blob from the url in the lock, verify its sha256, keep it in the cache
    Fetch {
        agent: String,
        /// Rust-style target (default: this machine)
        #[arg(long)]
        target: Option<String>,
    },
    /// Print the cache path of an already fetched agent (fails if it is not cached and valid)
    Path {
        agent: String,
        #[arg(long)]
        target: Option<String>,
    },
}

pub fn run(args: AgentsArgs) -> Result<()> {
    let lock = Lock::parse(&std::fs::read_to_string(&args.lock).with_context(|| {
        format!(
            "read {} (no lock yet? entries are added by hand with approved sources)",
            args.lock.display()
        )
    })?)?;
    let cache = cache_dir()?;
    match args.command {
        AgentsCommand::List => {
            for a in &lock.agents {
                for p in &a.platforms {
                    let state = if p.sha256 == PENDING {
                        "PENDING"
                    } else if lookup(&cache, &p.sha256).is_some() {
                        "cached"
                    } else {
                        "not cached"
                    };
                    println!("{} {} {} {state}", a.name, a.version, p.target);
                }
            }
        }
        AgentsCommand::Fetch { agent, target } => {
            let t = target.unwrap_or_else(|| host_target().to_string());
            let (a, p) = lock.find(&agent, &t)?;
            if p.sha256 == PENDING {
                bail!(
                    "{} {} {t}: sha256 is PENDING, nothing to fetch",
                    a.name,
                    a.version
                );
            }
            if !p.url.starts_with("https://") {
                bail!("{}: only https urls are fetched", p.url);
            }
            if lookup(&cache, &p.sha256).is_some() {
                println!("{}", blob_path(&cache, &p.sha256).display());
                return Ok(());
            }
            let bytes = reqwest::blocking::get(&p.url)?
                .error_for_status()?
                .bytes()?;
            println!("{}", store(&cache, &p.sha256, &bytes)?.display());
        }
        AgentsCommand::Path { agent, target } => {
            let t = target.unwrap_or_else(|| host_target().to_string());
            let (_, p) = lock.find(&agent, &t)?;
            match lookup(&cache, &p.sha256) {
                Some(path) => println!("{}", path.display()),
                None => {
                    bail!("{agent} {t} is not in the cache (run `path d agents fetch {agent}`)")
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
[[agent]]
name = "codex"
version = "0.162.1"
runtime = "native"
[[agent.platform]]
target = "aarch64-apple-darwin"
url = "https://example.invalid/codex.tgz"
sha256 = "PENDING"
team_id = "TEAM123"
[[agent.platform]]
target = "x86_64-unknown-linux-gnu"
url = "https://example.invalid/codex-linux.tgz"
sha256 = "abc"
"#;

    #[test]
    fn the_lock_names_agents_by_platform() {
        let l = Lock::parse(LOCK).unwrap();
        let (a, p) = l.find("codex", "aarch64-apple-darwin").unwrap();
        assert_eq!(
            (a.version.as_str(), p.team_id.as_deref()),
            ("0.162.1", Some("TEAM123"))
        );
        assert!(
            l.find("codex", "riscv")
                .unwrap_err()
                .to_string()
                .contains("no riscv entry")
        );
        assert!(
            l.find("nope", "x")
                .unwrap_err()
                .to_string()
                .contains("not in the lock")
        );
    }

    #[test]
    fn a_blob_is_kept_only_under_its_own_hash_and_a_mismatch_is_refused() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"agent bytes";
        let sha = sha256_hex(bytes);
        let p = store(cache.path(), &sha, bytes).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        assert_eq!(lookup(cache.path(), &sha), Some(p.clone()));
        // no execute bit, ever
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o111,
                0
            );
        }
        let err = store(cache.path(), &sha, b"tampered")
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256 mismatch"), "{err}");
    }

    #[test]
    fn a_pending_entry_is_never_stored_and_a_corrupted_cache_entry_is_not_trusted() {
        let cache = tempfile::tempdir().unwrap();
        assert!(
            store(cache.path(), PENDING, b"x")
                .unwrap_err()
                .to_string()
                .contains("PENDING")
        );
        let sha = sha256_hex(b"good");
        let p = store(cache.path(), &sha, b"good").unwrap();
        std::fs::write(&p, b"bad").unwrap();
        assert_eq!(
            lookup(cache.path(), &sha),
            None,
            "re-hashed on every lookup"
        );
    }
}
