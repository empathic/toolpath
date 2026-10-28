//! `agents.lock`: what each agent is, per platform, and where its verified bytes come from.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Lock {
    #[serde(default, rename = "agent")]
    pub agents: Vec<Agent>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub version: String,
    /// Where the version and checksums were read (a release page or a manifest url).
    pub source: String,
    #[serde(default, rename = "platform")]
    pub platforms: Vec<Platform>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct Platform {
    /// Rust target triple, e.g. `aarch64-apple-darwin`.
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub archive: Archive,
    /// For an archive: the path of the executable inside it.
    pub member: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Archive {
    /// The url serves the executable itself.
    Binary,
    /// A gzip-compressed tar; `member` names the executable.
    TarGz,
}

impl Lock {
    /// Parse and validate: every sha256 is 64 lowercase hex, every url is https, a tar.gz names
    /// its member, and no (agent, target) pair appears twice.
    pub fn parse(text: &str) -> Result<Lock> {
        let lock: Lock = toml::from_str(text)?;
        let mut seen = std::collections::BTreeSet::new();
        for a in &lock.agents {
            for p in &a.platforms {
                let what = format!("{} {} {}", a.name, a.version, p.target);
                if p.sha256.len() != 64
                    || !p
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    bail!("{what}: sha256 must be 64 lowercase hex digits");
                }
                if !p.url.starts_with("https://") {
                    bail!("{what}: only https urls are allowed");
                }
                if p.archive == Archive::TarGz && p.member.is_none() {
                    bail!("{what}: a tar-gz entry must name its member");
                }
                if !seen.insert((a.name.clone(), p.target.clone())) {
                    bail!("{what}: listed twice");
                }
            }
        }
        Ok(lock)
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

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "ca923c86ac84b30574a82331a05b6e066c27477a926ceaffe46a845a33e4dd37";

    fn lock(sha: &str, url: &str, extra: &str) -> String {
        format!(
            r#"
[[agent]]
name = "claude"
version = "2.1.308"
source = "https://example.invalid/manifest.json"
[[agent.platform]]
target = "aarch64-apple-darwin"
url = "{url}"
sha256 = "{sha}"
{extra}
"#
        )
    }

    #[test]
    fn the_lock_names_agents_by_platform() {
        let l = Lock::parse(&lock(SHA, "https://x/claude", "archive = \"binary\"")).unwrap();
        let (a, p) = l.find("claude", "aarch64-apple-darwin").unwrap();
        assert_eq!(
            (a.version.as_str(), p.archive),
            ("2.1.308", Archive::Binary)
        );
        let e = l.find("claude", "riscv").unwrap_err().to_string();
        assert!(e.contains("no riscv entry"), "{e}");
        let e = l.find("nope", "x").unwrap_err().to_string();
        assert!(e.contains("not in the lock"), "{e}");
    }

    #[test]
    fn a_lock_without_a_real_hash_an_https_url_or_a_member_is_refused() {
        let bad = [
            lock("PENDING", "https://x/c", "archive = \"binary\""),
            lock(&SHA.to_uppercase(), "https://x/c", "archive = \"binary\""),
            lock(SHA, "http://x/c", "archive = \"binary\""),
            lock(SHA, "https://x/c.tgz", "archive = \"tar-gz\""),
        ];
        for text in bad {
            assert!(Lock::parse(&text).is_err(), "accepted:\n{text}");
        }
    }

    #[test]
    fn the_committed_lock_parses_and_covers_this_repos_ci_platforms() {
        let text =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../agents.lock"))
                .expect("agents.lock at the repo root");
        let l = Lock::parse(&text).unwrap();
        assert!(!l.agents.is_empty());
        for a in &l.agents {
            for t in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
                l.find(&a.name, t).unwrap();
            }
        }
    }
}
