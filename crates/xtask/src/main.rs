//! `cargo xtask <verb>`: dev tooling in Rust. Every `just` recipe is a one-liner calling this.
//!
//! * `pins list|verify [--lock agents.lock]`: what the lock pins and whether the hash-named cache
//!   holds it. OFFLINE: nothing here touches the network.
//! * `pins fetch <agent>`: the same code as `path d agents fetch` (download, verify the sha256,
//!   keep by hash). Only an entry with an approved url and sha256 can be fetched.
//! * `pins bump`: not built (needs approved sources).
//! * `contract [--branch pr/NN-x]`: the PR record's `## Proves` claims, each run and tabled (contract.rs).
//! * `red-green [--base <rev>]`: the tests a change adds, red at the base and green at the head (contract.rs).

mod agents;
mod contract;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use path_cli::agents::{Lock, PENDING, cache_dir, lookup};

/// One line per lock entry: `<agent> <version> <target> pending|cached|missing`.
pub fn verify(lock: &Lock, cache: &Path) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut ok = true;
    for a in &lock.agents {
        for p in &a.platforms {
            let state = if p.sha256 == PENDING {
                ok = false;
                "pending"
            } else if lookup(cache, &p.sha256).is_some() {
                "cached"
            } else {
                ok = false;
                "missing"
            };
            lines.push(format!("{} {} {} {state}", a.name, a.version, p.target));
        }
    }
    (lines, ok)
}

#[derive(serde::Deserialize)]
pub struct ActLock {
    act: Tool,
    runner: Runner,
    #[serde(default)]
    secrets: Secrets,
}

#[derive(serde::Deserialize)]
struct Tool {
    version: String,
    sha256: String,
}

#[derive(serde::Deserialize)]
struct Runner {
    image: String,
    digest: String,
}

#[derive(serde::Deserialize, Default)]
struct Secrets {
    #[serde(default)]
    pass: Vec<String>,
}

fn plain(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The `act` command line for `just act <workflow> <agent>`, or why it cannot be built: every
/// PENDING entry in ci/act.lock is named, and no secret is ever passed.
pub fn act_plan(lock: &ActLock, workflow: &str, agent: &str) -> Result<Vec<String>> {
    if !plain(workflow) || !plain(agent) {
        bail!("act: workflow and agent must be plain names (letters, digits, - and _)");
    }
    if let Some(s) = lock.secrets.pass.first() {
        bail!("act: ci/act.lock passes secret {s:?}; replay takes none");
    }
    let pending: Vec<&str> = [
        ("act.version", &lock.act.version),
        ("act.sha256", &lock.act.sha256),
        ("runner.image", &lock.runner.image),
        ("runner.digest", &lock.runner.digest),
    ]
    .into_iter()
    .filter(|(_, v)| v.as_str() == PENDING)
    .map(|(k, _)| k)
    .collect();
    if !pending.is_empty() {
        bail!(
            "act: not approved yet in ci/act.lock: {}",
            pending.join(", ")
        );
    }
    if !lock.runner.digest.starts_with("sha256:") {
        bail!("act: runner.digest must be sha256:<hex>, never a tag");
    }
    Ok(vec![
        "act".into(),
        "workflow_dispatch".into(),
        "-W".into(),
        format!(".github/workflows/{workflow}.yml"),
        "-P".into(),
        // The workflows pin `runs-on: ubuntu-24.04` (pr/118); map that label to the pinned image.
        format!("ubuntu-24.04={}@{}", lock.runner.image, lock.runner.digest),
        "--input".into(),
        format!("agent={agent}"),
        "--pull=false".into(),
        // The image is pulled by digest for the VM's own architecture (colima on Apple silicon).
        "--container-architecture".into(),
        "linux/arm64".into(),
        // Never hand the job the Docker socket: a workflow step must not control the VM's daemon.
        // (colima's socket also cannot be bind-mounted, so act fails to start without this.)
        "--container-daemon-socket".into(),
        "-".into(),
    ])
}

fn lock_arg(args: &[String]) -> PathBuf {
    args.iter()
        .position(|a| a == "--lock")
        .and_then(|i| args.get(i + 1))
        .map_or_else(|| PathBuf::from("agents.lock"), PathBuf::from)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [pins, verb, rest @ ..] if pins == "pins" => {
            let lock_path = lock_arg(rest);
            match verb.as_str() {
                "list" | "verify" => {
                    let lock = Lock::parse(&std::fs::read_to_string(&lock_path)?)?;
                    let (lines, ok) = verify(&lock, &cache_dir()?);
                    for l in &lines {
                        println!("{l}");
                    }
                    if verb == "verify" && !ok {
                        bail!("pins verify: not every entry is approved and cached");
                    }
                    Ok(())
                }
                "fetch" | "bump" => bail!(
                    "pins {verb}: needs approved sources in agents.lock; use `path d agents fetch <agent>` once they exist"
                ),
                other => bail!("unknown pins verb {other:?} (list, verify, fetch, bump)"),
            }
        }
        [a] if a == "agents" => agents::run(Path::new(".")),
        [c, rest @ ..] if c == "contract" => contract::contract(Path::new("."), rest),
        [c, rest @ ..] if c == "red-green" => contract::red_green(Path::new("."), rest),
        [act, workflow, agent] if act == "act" => {
            let lock: ActLock = toml::from_str(&std::fs::read_to_string("ci/act.lock")?)?;
            // Dry run only: running needs the pinned act in the cache and a started VM.
            println!("{}", act_plan(&lock, workflow, agent)?.join(" "));
            Ok(())
        }
        _ => bail!(
            "usage: cargo xtask pins list|verify|fetch|bump [--lock agents.lock] | act <workflow> <agent> | agents | contract [--branch B] | red-green [--base REV]"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use path_cli::agents::{sha256_hex, store};

    fn act_lock(extra: &str, image: &str, digest: &str) -> ActLock {
        toml::from_str(&format!(
            "[act]\nversion='0.2.0'\nsha256='{}'\n[runner]\nimage='{image}'\ndigest='{digest}'\n{extra}",
            sha256_hex(b"act")
        ))
        .unwrap()
    }

    #[test]
    fn act_names_every_pending_pin_refuses_secrets_and_tags_and_builds_a_digest_pinned_command() {
        let e = act_plan(&act_lock("", PENDING, PENDING), "replay", "claude").unwrap_err();
        let m = e.to_string();
        assert!(
            m.contains("runner.image") && m.contains("runner.digest"),
            "{m}"
        );
        let d = format!("sha256:{}", sha256_hex(b"img"));
        let ok = act_lock("", "ghcr.io/x/runner", &d);
        let argv = act_plan(&ok, "replay", "claude").unwrap();
        assert!(argv.contains(&format!("ubuntu-24.04=ghcr.io/x/runner@{d}")));
        assert!(argv.contains(&"agent=claude".to_string()));
        assert!(act_plan(&ok, "../x", "claude").is_err(), "path-like names");
        assert!(
            act_plan(&act_lock("", "i", "latest"), "replay", "claude").is_err(),
            "a tag"
        );
        let with_secret = act_lock("[secrets]\npass=['TOKEN']\n", "i", &d);
        assert!(act_plan(&with_secret, "replay", "claude").is_err());
    }

    #[test]
    fn verify_reports_pending_missing_and_cached_and_fails_unless_all_are_cached() {
        let cache = tempfile::tempdir().unwrap();
        let good = sha256_hex(b"blob");
        store(cache.path(), &good, b"blob").unwrap();
        let lock = Lock::parse(&format!(
            r#"
[[agent]]
name = "a"
version = "1"
[[agent.platform]]
target = "t1"
url = "https://x.invalid/a"
sha256 = "{good}"
[[agent.platform]]
target = "t2"
url = "https://x.invalid/b"
sha256 = "{}"
[[agent.platform]]
target = "t3"
url = "https://x.invalid/c"
sha256 = "PENDING"
"#,
            sha256_hex(b"other")
        ))
        .unwrap();
        let (lines, ok) = verify(&lock, cache.path());
        assert_eq!(lines, ["a 1 t1 cached", "a 1 t2 missing", "a 1 t3 pending"]);
        assert!(!ok);
        let only = Lock::parse(&format!(
            "[[agent]]\nname='a'\nversion='1'\n[[agent.platform]]\ntarget='t1'\nurl='https://x.invalid/a'\nsha256='{good}'\n"
        ))
        .unwrap();
        assert!(verify(&only, cache.path()).1);
    }
}
