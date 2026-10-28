//! The `[sync]` block of `config.toml`: which sessions the periodic
//! `path sync` job uploads, how often, and how much it reports. Where
//! each session goes is not here: the `[[project]]` `remote` rules
//! decide that for the job as they do for `path share`, and a rule's
//! `sync = false` takes its sessions out of the job's scope.

use crate::artifact::ArtifactType;
use crate::harness::Harness;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_INTERVAL: &str = "15m";

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub(crate) struct SyncConfig {
    /// How often the timer runs a pass: a whole number of seconds,
    /// minutes or hours (`90s`, `15m`, `1h`).
    pub(crate) interval: Option<String>,
    /// Directory subtrees whose sessions the job uploads; `[]` is every
    /// session. Absent means not configured, and a pass refuses to run.
    pub(crate) include: Option<Vec<String>>,
    /// Only these harnesses; absent is every harness.
    pub(crate) harnesses: Option<Vec<String>>,
    /// Print a line per session uploaded, not only the summary.
    #[serde(default)]
    pub(crate) verbose: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    sync: Option<SyncConfig>,
}

/// The sessions a pass covers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scope {
    /// Empty: every directory.
    pub(crate) include: Vec<PathBuf>,
    pub(crate) harnesses: Vec<ArtifactType>,
}

impl SyncConfig {
    /// The `[sync]` block of `text`, validated; `None` when there is
    /// none. `file` names the file in errors.
    pub(crate) fn parse(text: &str, file: &str) -> Result<Option<Self>> {
        let parsed: ConfigFile =
            toml::from_str(text).with_context(|| format!("failed to parse {file}"))?;
        if let Some(sync) = &parsed.sync {
            sync.validate()
                .with_context(|| format!("{file}: invalid [sync]"))?;
        }
        Ok(parsed.sync)
    }

    /// The `[sync]` block of the config file under `config_dir`.
    pub(crate) fn load(config_dir: &Path) -> Result<Option<Self>> {
        let path = config_dir.join(crate::config::CONFIG_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    fn validate(&self) -> Result<()> {
        parse_interval(self.interval())?;
        if let Some(include) = &self.include {
            validate_include(include)?;
        }
        if let Some(harnesses) = &self.harnesses {
            parse_harnesses(harnesses)?;
        }
        Ok(())
    }

    pub(crate) fn interval(&self) -> &str {
        self.interval.as_deref().unwrap_or(DEFAULT_INTERVAL)
    }

    /// The scope of a pass, `~` expanded against `home`.
    pub(crate) fn scope(&self, home: Option<&Path>) -> Result<Scope> {
        let Some(include) = &self.include else {
            bail!(
                "[sync] has no `include`: list the directories to upload sessions from, \
                 or `include = []` for every session"
            );
        };
        let include = include
            .iter()
            .map(|dir| expand_home(dir, home))
            .collect::<Result<_>>()?;
        let harnesses = match &self.harnesses {
            Some(names) => parse_harnesses(names)?,
            None => Harness::ALL.iter().map(Harness::artifact_type).collect(),
        };
        Ok(Scope { include, harnesses })
    }
}

fn expand_home(dir: &str, home: Option<&Path>) -> Result<PathBuf> {
    let rest = match dir.strip_prefix('~') {
        None => return Ok(PathBuf::from(dir)),
        Some(rest) => rest.trim_start_matches('/'),
    };
    let home = home.context("cannot expand `~` in [sync].include without a home directory")?;
    Ok(if rest.is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    })
}

fn validate_include(include: &[String]) -> Result<()> {
    for dir in include {
        if dir.trim().is_empty() {
            bail!("an `include` directory is empty; use `include = []` for every session");
        }
        if !Path::new(dir).is_absolute() && dir != "~" && !dir.starts_with("~/") {
            bail!("`include` directories must be absolute or start with `~/`: {dir:?}");
        }
    }
    Ok(())
}

fn parse_harnesses(names: &[String]) -> Result<Vec<ArtifactType>> {
    names
        .iter()
        .map(|name| {
            <Harness as clap::ValueEnum>::from_str(name, false)
                .map(|h| h.artifact_type())
                .map_err(|_| {
                    let known: Vec<&str> = Harness::ALL.iter().map(Harness::name).collect();
                    anyhow::anyhow!(
                        "unknown harness {name:?}; expected one of {}",
                        known.join(", ")
                    )
                })
        })
        .collect()
}

/// Seconds in an interval of the form `<n>s`, `<n>m` or `<n>h`.
pub(crate) fn parse_interval(value: &str) -> Result<u64> {
    let unit = match value.chars().last() {
        Some('s') => 1,
        Some('m') => 60,
        Some('h') => 3600,
        _ => bail!("invalid interval {value:?}; expected a duration such as 90s, 15m or 1h"),
    };
    let seconds = value[..value.len() - 1]
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit));
    match seconds {
        Some(n) if n > 0 && n <= u64::from(u32::MAX) => Ok(n),
        _ => bail!("invalid interval {value:?}; expected a duration such as 90s, 15m or 1h"),
    }
}

/// What `path sync install` writes into `[sync]`. Only the values given
/// replace what is there, so reinstalling to change the interval keeps
/// the configured scope.
#[derive(Debug, Default)]
pub(crate) struct InstallSettings {
    /// `include = []`: every session.
    pub(crate) all: bool,
    pub(crate) include: Vec<String>,
    pub(crate) harnesses: Vec<String>,
    pub(crate) interval: Option<String>,
}

/// `text` with `settings` written into its `[sync]` table. Every byte
/// outside `[sync]` stays as it was, and the result must parse.
pub(crate) fn install_config(text: &str, settings: &InstallSettings) -> Result<String> {
    let current = SyncConfig::parse(text, "config.toml")?;
    let configured_scope = current.as_ref().is_some_and(|s| s.include.is_some());
    if !settings.all && settings.include.is_empty() && !configured_scope {
        bail!(
            "say which sessions to upload: --project-under <dir> (repeatable), \
             or --all for every session"
        );
    }
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .context("failed to parse config.toml")?;
    let sync = doc
        .as_table_mut()
        .entry("sync")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .context("config.toml: `sync` must be a table")?;
    if settings.all || !settings.include.is_empty() {
        sync["include"] = string_array(&settings.include);
    }
    if !settings.harnesses.is_empty() {
        sync["harnesses"] = string_array(&settings.harnesses);
    }
    if let Some(interval) = &settings.interval {
        sync["interval"] = toml_edit::value(interval.as_str());
    } else if !sync.contains_key("interval") {
        sync["interval"] = toml_edit::value(DEFAULT_INTERVAL);
    }
    let result = doc.to_string();
    SyncConfig::parse(&result, "config.toml")?;
    crate::share_config::validate_config_text(&result, "config.toml")?;
    only_sync_changed(text, &result)?;
    Ok(result)
}

fn string_array(values: &[String]) -> toml_edit::Item {
    toml_edit::value(
        values
            .iter()
            .map(String::as_str)
            .collect::<toml_edit::Array>(),
    )
}

fn only_sync_changed(before: &str, after: &str) -> Result<()> {
    let mut before: toml::Table = toml::from_str(before).context("failed to parse config.toml")?;
    let mut after: toml::Table = toml::from_str(after).context("failed to parse config.toml")?;
    before.remove("sync");
    after.remove("sync");
    if before != after {
        bail!("refusing to write config.toml: the edit would change something outside [sync]");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> SyncConfig {
        SyncConfig::parse(text, "config.toml").unwrap().unwrap()
    }

    #[test]
    fn a_config_without_sync_has_no_block() {
        assert_eq!(SyncConfig::parse("", "c").unwrap(), None);
        let text = "[[project]]\ndir = '/a'\nremote = 'o/r'\n";
        assert_eq!(SyncConfig::parse(text, "c").unwrap(), None);
    }

    #[test]
    fn the_block_parses_with_defaults() {
        let c = parse("[sync]\ninclude = ['/work']\n");
        assert_eq!(c.interval(), "15m");
        assert!(!c.verbose);
        let scope = c.scope(None).unwrap();
        assert_eq!(scope.include, [PathBuf::from("/work")]);
        assert_eq!(scope.harnesses.len(), Harness::ALL.len());
        assert!(!scope.harnesses.contains(&ArtifactType::Git));
    }

    #[test]
    fn scope_expands_home_and_narrows_harnesses() {
        let c = parse(
            "[sync]\ninclude = ['~/work', '~', '/abs']\nharnesses = ['codex', 'claude']\n\
             interval = '5m'\nverbose = true\n",
        );
        let scope = c.scope(Some(Path::new("/home/me"))).unwrap();
        assert_eq!(
            scope.include,
            [
                PathBuf::from("/home/me/work"),
                PathBuf::from("/home/me"),
                PathBuf::from("/abs")
            ]
        );
        assert_eq!(scope.harnesses, [ArtifactType::Codex, ArtifactType::Claude]);
        assert!(c.verbose);
        assert!(c.scope(None).is_err());
    }

    #[test]
    fn an_empty_include_is_every_session_and_a_missing_one_is_an_error() {
        assert!(
            parse("[sync]\ninclude = []\n")
                .scope(None)
                .unwrap()
                .include
                .is_empty()
        );
        let err = parse("[sync]\ninterval = '1h'\n").scope(None).unwrap_err();
        assert!(err.to_string().contains("include"), "{err}");
    }

    #[test]
    fn invalid_blocks_are_refused() {
        for text in [
            "[sync]\ninterval = '0m'",
            "[sync]\ninterval = '15'",
            "[sync]\ninterval = 'm'",
            "[sync]\ninterval = '1d'",
            "[sync]\ninterval = '99999999999999999h'",
            "[sync]\ninclude = ['relative/dir']",
            "[sync]\ninclude = ['']",
            "[sync]\nharnesses = ['git']",
            "[sync]\nharnesses = ['vim']",
            "[sync]\nverbose = 'yes'",
            "sync = 1",
        ] {
            assert!(SyncConfig::parse(text, "c").is_err(), "{text}");
        }
    }

    #[test]
    fn intervals_parse_to_seconds() {
        assert_eq!(parse_interval("90s").unwrap(), 90);
        assert_eq!(parse_interval("15m").unwrap(), 900);
        assert_eq!(parse_interval("2h").unwrap(), 7200);
    }

    #[test]
    fn first_install_needs_a_scope() {
        let err = install_config("", &InstallSettings::default()).unwrap_err();
        assert!(err.to_string().contains("--project-under"), "{err}");
        let all = install_config(
            "",
            &InstallSettings {
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(all, "[sync]\ninclude = []\ninterval = \"15m\"\n");
    }

    #[test]
    fn install_writes_only_the_values_given_and_keeps_the_rest() {
        let text = "# mine\ntitle = 'keep'   # trailing\n\n[[project]]\ndir = '/work/private'\nsync = false\n\n[sync]\n# how often\ninterval = '30m'\ninclude = ['/work']\n";
        let reinstalled = install_config(text, &InstallSettings::default()).unwrap();
        assert_eq!(reinstalled, text);
        let changed = install_config(
            text,
            &InstallSettings {
                harnesses: vec!["codex".into()],
                interval: Some("5m".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let head = text.split("[sync]").next().unwrap();
        assert!(changed.starts_with(head), "{changed}");
        assert!(changed.contains("# how often\n"), "{changed}");
        let c = parse(&changed);
        assert_eq!(c.interval(), "5m");
        assert_eq!(c.include.unwrap(), ["/work"]);
        assert_eq!(c.harnesses.unwrap(), ["codex"]);
    }

    #[test]
    fn install_refuses_values_a_pass_would_refuse() {
        for settings in [
            InstallSettings {
                include: vec!["relative".into()],
                ..Default::default()
            },
            InstallSettings {
                all: true,
                interval: Some("soon".into()),
                ..Default::default()
            },
        ] {
            assert!(install_config("", &settings).is_err(), "{settings:?}");
        }
    }

    #[test]
    fn install_refuses_a_broken_config() {
        let broken = "[[project]]\nremote = 'o/r'\n";
        let settings = InstallSettings {
            all: true,
            ..Default::default()
        };
        assert!(install_config(broken, &settings).is_err());
    }
}
