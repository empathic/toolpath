//! Automatic upload policy from the user's single config file.
//!
//! Resolution receives a config snapshot and never reads process environment.
use crate::artifact::ArtifactType;
use crate::remote::parse_remote;
use crate::share_config::{
    ConfiguredRemote, ProjectRule, canonicalize_prefix, expand_tilde, resolve_project_fields,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SyncConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    /// None distinguishes an unconfigured scope from explicitly global [].
    pub(crate) include: Option<Vec<String>>,
    pub(crate) harnesses: Option<Vec<String>>,
    pub(crate) remote: Option<String>,
    pub(crate) interval: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UserSyncConfig {
    #[serde(default)]
    pub(crate) sync: SyncConfig,
    #[serde(default)]
    pub(crate) project: Vec<ProjectRule>,
}

#[derive(Debug, Default)]
pub(crate) struct ScopeOverrides {
    pub(crate) all: bool,
    pub(crate) include: Vec<String>,
    pub(crate) harnesses: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct EffectiveScope {
    pub(crate) enabled: bool,
    pub(crate) include: Vec<PathBuf>,
    pub(crate) harnesses: Vec<ArtifactType>,
}

impl UserSyncConfig {
    pub(crate) fn parse(text: &str, file: &str) -> Result<Self> {
        crate::share_config::validate_config_text(text, file)?;
        let config: Self =
            toml::from_str(text).with_context(|| format!("failed to parse {file}"))?;
        config.sync.validate()?;
        Ok(config)
    }

    /// None means disabled, out of scope, or explicitly opted out. Destination
    /// flags never bypass exclusions. The caller supplies the authenticated user.
    pub(crate) fn destination(
        &self,
        scope: &EffectiveScope,
        provider: ArtifactType,
        session_dir: Option<&str>,
        home: Option<&Path>,
        repo_override: Option<&str>,
        authenticated_user: Option<&str>,
    ) -> Result<Option<ConfiguredRemote>> {
        if !scope.contains(provider, session_dir) {
            return Ok(None);
        }
        let (remote, sync) = resolve_project_fields(
            &self.project,
            home,
            session_dir.map(Path::new),
            true,
            |dir, root| provider_in_scope(provider, dir, root),
        );
        if sync.and_then(|rule| rule.sync) == Some(false) {
            return Ok(None);
        }
        let fallback;
        let (value, origin) = if let Some(value) = repo_override {
            (value, "--repo")
        } else if let Some(value) = remote.and_then(|rule| rule.remote.as_deref()) {
            (value, "[[project]].remote")
        } else if let Some(value) = self.sync.remote.as_deref() {
            (value, "[sync].remote")
        } else {
            let Some(user) = authenticated_user else {
                bail!("automatic sync requires authentication; run `path auth login`")
            };
            fallback = format!("{user}/pathstash");
            (fallback.as_str(), "authenticated pathstash")
        };
        let (repo, base_url) = parse_remote(value, origin)?;
        Ok(Some(ConfiguredRemote {
            repo,
            base_url,
            display: value.to_owned(),
            origin: origin.to_owned(),
        }))
    }
}

impl SyncConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        self.interval_seconds()?;
        if let Some(remote) = &self.remote {
            parse_remote(remote, "[sync].remote")?;
        }
        if let Some(providers) = &self.harnesses {
            parse_harnesses(providers)?;
        }
        if let Some(include) = &self.include {
            validate_include(include)?;
        }
        Ok(())
    }

    pub(crate) fn interval_seconds(&self) -> Result<u64> {
        parse_interval(self.interval.as_deref().unwrap_or("15m"))
    }

    pub(crate) fn scope(
        &self,
        overrides: &ScopeOverrides,
        home: Option<&Path>,
    ) -> Result<EffectiveScope> {
        if overrides.all && !overrides.include.is_empty() {
            bail!("--all and --include are mutually exclusive");
        }
        let includes = if overrides.all {
            &[][..]
        } else if !overrides.include.is_empty() {
            overrides.include.as_slice()
        } else {
            self.include.as_deref().unwrap_or_default()
        };
        validate_include(includes)?;
        if home.is_none()
            && includes
                .iter()
                .any(|dir| dir == "~" || dir.starts_with("~/"))
        {
            bail!("cannot expand sync include directory without a home directory");
        }
        let include = includes
            .iter()
            .map(|dir| canonicalize_prefix(&expand_tilde(dir, home)))
            .collect();
        let harnesses = if !overrides.harnesses.is_empty() {
            parse_harnesses(&overrides.harnesses)?
        } else if let Some(providers) = &self.harnesses {
            parse_harnesses(providers)?
        } else {
            ArtifactType::ALL
                .into_iter()
                .filter(|t| *t != ArtifactType::Git)
                .collect()
        };
        Ok(EffectiveScope {
            enabled: self.enabled,
            include,
            harnesses,
        })
    }
}

impl EffectiveScope {
    pub(crate) fn contains(&self, provider: ArtifactType, dir: Option<&str>) -> bool {
        self.enabled
            && self.harnesses.contains(&provider)
            && (self.include.is_empty()
                || dir.is_some_and(|dir| {
                    self.include
                        .iter()
                        .any(|root| provider_in_scope(provider, dir, root))
                }))
    }
}

fn provider_in_scope(provider: ArtifactType, dir: &str, root: &Path) -> bool {
    if Path::new(dir).is_absolute() {
        let normalized = canonicalize_prefix(Path::new(dir));
        crate::sync::sources::project_in_scope(provider, &normalized.to_string_lossy(), root)
    } else {
        crate::sync::sources::project_in_scope(provider, dir, root)
    }
}

fn validate_include(include: &[String]) -> Result<()> {
    for dir in include {
        if !Path::new(dir).is_absolute() && dir != "~" && !dir.starts_with("~/") {
            bail!("sync include directories must be absolute or start with ~/: {dir:?}");
        }
        if dir.trim().is_empty() {
            bail!("sync include directories cannot be empty strings; use [] for global scope");
        }
    }
    Ok(())
}

fn parse_harnesses(names: &[String]) -> Result<Vec<ArtifactType>> {
    names.iter().map(|name| match ArtifactType::parse(name) {
        Some(provider) if provider != ArtifactType::Git => Ok(provider),
        _ => bail!("unknown sync harness {name:?}; expected claude, gemini, codex, opencode, cursor, pi, or copilot"),
    }).collect()
}

pub(crate) fn parse_interval(value: &str) -> Result<u64> {
    let (number, multiplier) = if let Some(n) = value.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = value.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = value.strip_suffix('h') {
        (n, 3600)
    } else {
        bail!("invalid sync interval {value:?}; expected a positive duration such as 15m");
    };
    let seconds = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier));
    match seconds {
        Some(n) if n > 0 && n <= i32::MAX as u64 => Ok(n),
        _ => bail!("invalid sync interval {value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(text: &str) -> UserSyncConfig {
        UserSyncConfig::parse(text, "config.toml").unwrap()
    }
    fn scope(c: &UserSyncConfig) -> EffectiveScope {
        c.sync.scope(&ScopeOverrides::default(), None).unwrap()
    }
    #[test]
    fn disabled_is_authoritative_even_with_all_override() {
        let c = config("");
        let scope = c
            .sync
            .scope(
                &ScopeOverrides {
                    all: true,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert!(!scope.contains(ArtifactType::Codex, Some("/work")));
        assert!(
            c.destination(&scope, ArtifactType::Codex, None, None, Some("a/b"), None)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn scope_union_intersects_provider_and_handles_unknown_directories() {
        let c = config("[sync]\nenabled=true\ninclude=['/gone/a','/gone/b']\nharnesses=['codex']");
        let s = scope(&c);
        assert!(s.contains(ArtifactType::Codex, Some("/gone/b/checkout")));
        assert!(!s.contains(ArtifactType::Codex, Some("/gone/b-other")));
        assert!(!s.contains(ArtifactType::Claude, Some("/gone/a")));
        assert!(!s.contains(ArtifactType::Codex, None));
        let global = c
            .sync
            .scope(
                &ScopeOverrides {
                    all: true,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert!(global.contains(ArtifactType::Codex, None));
    }
    #[test]
    fn fields_resolve_independently_and_overrides_never_bypass_opt_out() {
        let c = config(
            "[sync]\nenabled=true\nremote='me/default'\n[[project]]\ndir='/gone'\nsync=false\nremote='me/broad'\n[[project]]\ndir='/gone/child'\nremote='me/narrow'\n[[project]]\ndir='/gone/child/yes'\nsync=true",
        );
        let s = scope(&c);
        assert!(
            c.destination(
                &s,
                ArtifactType::Codex,
                Some("/gone/child"),
                None,
                Some("me/override"),
                Some("me")
            )
            .unwrap()
            .is_none()
        );
        let got = c
            .destination(
                &s,
                ArtifactType::Codex,
                Some("/gone/child/yes"),
                None,
                None,
                Some("me"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(got.display, "me/narrow");
    }
    #[test]
    fn equal_specificity_first_defined_field_wins() {
        let c = config(
            "[sync]\nenabled=true\n[[project]]\ndir='/gone'\nsync=true\n[[project]]\ndir='/gone'\nsync=false\nremote='me/first'\n[[project]]\ndir='/gone'\nremote='me/second'",
        );
        let got = c
            .destination(
                &scope(&c),
                ArtifactType::Codex,
                Some("/gone"),
                None,
                None,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(got.display, "me/first");
    }
    #[test]
    fn destination_precedence_and_authentication() {
        let c = config("[sync]\nenabled=true\nremote='me/config'");
        assert_eq!(
            c.destination(
                &scope(&c),
                ArtifactType::Codex,
                None,
                None,
                Some("me/flag"),
                None
            )
            .unwrap()
            .unwrap()
            .display,
            "me/flag"
        );
        assert_eq!(
            c.destination(&scope(&c), ArtifactType::Codex, None, None, None, None)
                .unwrap()
                .unwrap()
                .display,
            "me/config"
        );
        let c = config("[sync]\nenabled=true");
        assert!(
            c.destination(&scope(&c), ArtifactType::Codex, None, None, None, None)
                .is_err()
        );
        assert_eq!(
            c.destination(
                &scope(&c),
                ArtifactType::Codex,
                None,
                None,
                None,
                Some("me")
            )
            .unwrap()
            .unwrap()
            .display,
            "me/pathstash"
        );
    }
    #[test]
    fn invalid_config_fails_before_upload() {
        for text in [
            "[sync]\ninterval='0m'",
            "[sync]\ninterval='999999999999999999h'",
            "[sync]\nharnesses=['git']",
            "[sync]\ninclude=['']",
            "[sync]\ninclude=['relative/dir']",
            "[sync]\nremote='invalid'",
            "[[project]]\ndir='/work'\nsync='false'",
        ] {
            assert!(
                UserSyncConfig::parse(text, "config.toml").is_err(),
                "{text}"
            );
        }
        assert_eq!(parse_interval("15m").unwrap(), 900);
    }
    #[test]
    fn provider_encoded_directories_match_in_their_own_space() {
        let c = config(
            "[sync]\nenabled=true\ninclude=['/gone/work']\n[[project]]\ndir='/gone/work/private'\nsync=false",
        );
        let s = scope(&c);
        assert!(s.contains(ArtifactType::Claude, Some("-gone-work-proj")));
        assert!(s.contains(ArtifactType::Pi, Some("-gone-work-proj")));
        assert!(s.contains(ArtifactType::Claude, Some("/gone/work/proj")));
        assert!(!s.contains(ArtifactType::Claude, Some("-gone-workshop")));
        assert!(!s.contains(ArtifactType::Codex, Some("-gone-work-proj")));
        let destination = |dir| {
            c.destination(&s, ArtifactType::Claude, Some(dir), None, None, Some("me"))
                .unwrap()
        };
        assert!(destination("-gone-work-private-x").is_none());
        assert!(destination("-gone-work-public").is_some());
        assert!(destination("/gone/work/private/x").is_none());
    }
    #[test]
    fn explicit_global_scope_remains_distinct_from_missing_scope() {
        assert!(config("[sync]\ninclude=[]").sync.include.is_some());
        assert!(config("[sync]").sync.include.is_none());
    }
}
