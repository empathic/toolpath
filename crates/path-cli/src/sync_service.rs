//! Pure install configuration edits and scheduler templates. The command owns
//! atomic writes, launchctl/systemctl execution, and partial-failure reporting.
use crate::sync_config::{UserSyncConfig, parse_interval};
use anyhow::{Context, Result, bail};
use std::path::Path;

pub(crate) const SERVICE_NAME: &str = "toolpath-sync";
pub(crate) const LAUNCHD_LABEL: &str = "net.toolpath.sync";

#[derive(Debug, Default)]
pub(crate) struct InstallOptions {
    pub(crate) all: bool,
    pub(crate) include: Vec<String>,
    pub(crate) harnesses: Vec<String>,
    pub(crate) interval: Option<String>,
    pub(crate) remote: Option<String>,
}

/// Merge only supplied values; a reinstall never widens an explicit scope.
/// No file is changed until the complete result has been validated.
pub(crate) fn install_config(text: &str, options: &InstallOptions) -> Result<String> {
    if options.all && !options.include.is_empty() {
        bail!("--all and --include are mutually exclusive");
    }
    let current = UserSyncConfig::parse(text, "config.toml")?;
    if !options.all && options.include.is_empty() && current.sync.include.is_none() {
        bail!(
            "first sync installation requires --include <dir> or --all unless [sync].include is already configured"
        );
    }
    let mut value: toml::Table = toml::from_str(text).context("failed to parse config.toml")?;
    let sync = value
        .entry("sync")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("[sync] must be a table")?;
    sync.insert("enabled".into(), toml::Value::Boolean(true));
    if options.all || !options.include.is_empty() {
        let dirs = if options.all {
            Vec::new()
        } else {
            options
                .include
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect()
        };
        sync.insert("include".into(), toml::Value::Array(dirs));
    }
    if !options.harnesses.is_empty() {
        sync.insert(
            "harnesses".into(),
            toml::Value::Array(
                options
                    .harnesses
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(interval) = &options.interval {
        sync.insert("interval".into(), toml::Value::String(interval.clone()));
    }
    if let Some(remote) = &options.remote {
        sync.insert("remote".into(), toml::Value::String(remote.clone()));
    }
    let result = toml::to_string_pretty(&value)?;
    UserSyncConfig::parse(&result, "config.toml")?;
    Ok(result)
}

/// Disable first, even when other sync settings no longer validate. Preserve
/// project rules, unknown settings and operation records (which live elsewhere).
pub(crate) fn disable_config(text: &str) -> Result<String> {
    let mut value: toml::Table = toml::from_str(text).context("failed to parse config.toml")?;
    let sync = value
        .entry("sync")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("[sync] must be a table")?;
    sync.insert("enabled".into(), toml::Value::Boolean(false));
    Ok(toml::to_string_pretty(&value)?)
}

#[derive(Debug)]
pub(crate) struct SystemdFiles {
    pub(crate) service: String,
    pub(crate) timer: String,
}

fn absolute_utf8(path: &Path) -> Result<&str> {
    if !path.is_absolute() {
        bail!("service paths must be absolute: {}", path.display());
    }
    let value = path.to_str().context("service paths must be valid UTF-8")?;
    if value.chars().any(char::is_control) {
        bail!("service paths cannot contain control characters");
    }
    Ok(value)
}

/// Quoting for a systemd directive word, including specifier expansion.
/// ExecStart additionally needs dollars doubled to suppress variable expansion.
fn systemd_quote(value: &str, exec: bool) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    let escaped = if exec {
        escaped.replace('$', "$$")
    } else {
        escaped
    };
    format!("\"{escaped}\"")
}

pub(crate) fn systemd_files(
    binary: &Path,
    config_dir: &Path,
    interval: &str,
) -> Result<SystemdFiles> {
    let binary = systemd_quote(absolute_utf8(binary)?, true);
    let config_dir = absolute_utf8(config_dir)?;
    let environment = systemd_quote(
        &format!("{}={config_dir}", crate::config::CONFIG_DIR_ENV),
        false,
    );
    let seconds = parse_interval(interval)?;
    Ok(SystemdFiles {
        service: format!(
            "[Unit]\nDescription=Toolpath session sync\n\n[Service]\nType=oneshot\nEnvironment={environment}\nExecStart={binary} sync\n"
        ),
        timer: format!(
            "[Unit]\nDescription=Toolpath session sync schedule\n\n[Timer]\nOnStartupSec=60s\nOnUnitActiveSec={seconds}s\nUnit={SERVICE_NAME}.service\n\n[Install]\nWantedBy=timers.target\n"
        ),
    })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn launchd_plist(binary: &Path, config_dir: &Path, interval: &str) -> Result<String> {
    let binary = xml_escape(absolute_utf8(binary)?);
    let config_dir = xml_escape(absolute_utf8(config_dir)?);
    let seconds = parse_interval(interval)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LAUNCHD_LABEL}</string>
<key>ProgramArguments</key><array><string>{binary}</string><string>sync</string></array>
<key>EnvironmentVariables</key><dict><key>{config_env}</key><string>{config_dir}</string></dict>
<key>StartInterval</key><integer>{seconds}</integer>
<key>RunAtLoad</key><true/>
</dict></plist>
"#,
        config_env = crate::config::CONFIG_DIR_ENV
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_install_requires_scope_and_reinstall_preserves_scope() {
        assert!(install_config("", &InstallOptions::default()).is_err());
        let configured = "title='keep'\n[sync]\ninclude=['/work']\nharnesses=['codex']\ninterval='30m'\n[[project]]\ndir='/work/private'\nsync=false\nremote='me/private'";
        let result = install_config(configured, &InstallOptions::default()).unwrap();
        let parsed = UserSyncConfig::parse(&result, "config.toml").unwrap();
        assert!(parsed.sync.enabled);
        assert_eq!(parsed.sync.include.unwrap(), ["/work"]);
        assert_eq!(parsed.sync.harnesses.unwrap(), ["codex"]);
        assert_eq!(parsed.sync.interval.unwrap(), "30m");
        assert_eq!(parsed.project[0].sync, Some(false));
        let value: toml::Table = toml::from_str(&result).unwrap();
        assert_eq!(value["title"].as_str(), Some("keep"));
    }
    #[test]
    fn global_scope_is_explicit_and_repeatable() {
        let options = InstallOptions {
            all: true,
            ..Default::default()
        };
        let first = install_config("", &options).unwrap();
        let second = install_config(&first, &InstallOptions::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            UserSyncConfig::parse(&first, "c").unwrap().sync.include,
            Some(vec![])
        );
    }
    #[test]
    fn supplied_install_settings_replace_only_their_fields() {
        let options = InstallOptions {
            include: vec!["/new".into()],
            harnesses: vec!["claude".into()],
            interval: Some("2h".into()),
            remote: Some("me/new".into()),
            ..Default::default()
        };
        let result = install_config("[sync]\ninclude=['/old']\nremote='me/old'", &options).unwrap();
        let config = UserSyncConfig::parse(&result, "c").unwrap();
        assert_eq!(config.sync.harnesses.as_deref().unwrap(), ["claude"]);
        assert_eq!(config.sync.interval_seconds().unwrap(), 7200);
        assert_eq!(config.sync.include.unwrap(), ["/new"]);
        assert_eq!(config.sync.remote.unwrap(), "me/new");
    }
    #[test]
    fn disabling_retains_config_and_prevents_next_pass() {
        let enabled = install_config(
            "",
            &InstallOptions {
                all: true,
                ..Default::default()
            },
        )
        .unwrap();
        let disabled = disable_config(&enabled).unwrap();
        assert_eq!(disabled, disable_config(&disabled).unwrap());
        let config = UserSyncConfig::parse(&disabled, "c").unwrap();
        let scope = config
            .sync
            .scope(
                &crate::sync_config::ScopeOverrides {
                    all: true,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert!(!scope.contains(crate::artifact::ArtifactType::Codex, None));
        assert_eq!(config.sync.include, Some(vec![]));
        // A broken interval must not prevent switching off an existing service.
        assert!(
            disable_config("[sync]\nenabled=true\ninterval='broken'")
                .unwrap()
                .contains("enabled = false")
        );
    }
    #[test]
    fn systemd_quotes_paths_and_runs_plain_sync() {
        let files = systemd_files(
            Path::new("/some path/bin $path%test\""),
            Path::new("/cfg path/$config%dir"),
            "15m",
        )
        .unwrap();
        assert!(
            files
                .service
                .contains("ExecStart=\"/some path/bin $$path%%test\\\"\" sync\n")
        );
        assert!(
            files
                .service
                .contains("Environment=\"TOOLPATH_CONFIG_DIR=/cfg path/$config%%dir\"")
        );
        assert!(files.timer.contains("OnUnitActiveSec=900s"));
        assert!(!files.service.contains("--include"));
        assert!(!files.service.contains("--repo"));
    }
    #[test]
    fn launchd_escapes_xml_without_shell_quoting() {
        let plist = launchd_plist(
            Path::new("/some path/<bin>&\"'"),
            Path::new("/cfg path/a&b"),
            "1h",
        )
        .unwrap();
        assert!(plist.contains(
            "<string>/some path/&lt;bin&gt;&amp;&quot;&apos;</string><string>sync</string>"
        ));
        assert!(plist.contains("<string>/cfg path/a&amp;b</string>"));
        assert!(plist.contains("<integer>3600</integer>"));
    }
    #[test]
    fn templates_reject_relative_paths_controls_and_bad_intervals() {
        for (binary, config, interval) in [
            ("relative", "/cfg", "1m"),
            ("/bin/path", "relative", "1m"),
            ("/bin/path\nInjected=yes", "/cfg", "1m"),
            ("/bin/path", "/cfg", "0s"),
        ] {
            assert!(systemd_files(Path::new(binary), Path::new(config), interval).is_err());
            assert!(launchd_plist(Path::new(binary), Path::new(config), interval).is_err());
        }
    }
}
