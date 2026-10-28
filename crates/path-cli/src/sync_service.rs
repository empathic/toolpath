//! The timer that runs `path sync`: a launchd agent on macOS, a systemd
//! user timer and service on Linux. Rendering the files and naming the
//! commands that load and unload them is pure; `cmd_sync` writes the
//! files and runs the commands.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub(crate) const LAUNCHD_LABEL: &str = "net.toolpath.path-sync";
pub(crate) const SYSTEMD_UNIT: &str = "path-sync";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheduler {
    Launchd,
    Systemd,
}

/// What the timer runs and how often.
pub(crate) struct Job<'a> {
    /// The `path` binary, absolute.
    pub(crate) binary: &'a Path,
    pub(crate) config_dir: &'a Path,
    pub(crate) interval_secs: u64,
}

impl Scheduler {
    /// This host's scheduler, when it has one `path sync` can use.
    pub(crate) fn host() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Scheduler::Launchd)
        } else if cfg!(target_os = "linux") {
            Some(Scheduler::Systemd)
        } else {
            None
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Scheduler::Launchd => "launchd agent",
            Scheduler::Systemd => "systemd user timer",
        }
    }

    /// The files this scheduler reads the job from, under `home`.
    pub(crate) fn files(self, home: &Path) -> Vec<PathBuf> {
        match self {
            Scheduler::Launchd => vec![
                home.join("Library/LaunchAgents")
                    .join(format!("{LAUNCHD_LABEL}.plist")),
            ],
            Scheduler::Systemd => {
                let dir = home.join(".config/systemd/user");
                vec![
                    dir.join(format!("{SYSTEMD_UNIT}.timer")),
                    dir.join(format!("{SYSTEMD_UNIT}.service")),
                ]
            }
        }
    }

    /// Each of [`Self::files`] with its content for `job`.
    pub(crate) fn render(self, home: &Path, job: &Job) -> Result<Vec<(PathBuf, String)>> {
        let contents = match self {
            Scheduler::Launchd => vec![launchd_plist(job)?],
            Scheduler::Systemd => vec![systemd_timer(job.interval_secs), systemd_service(job)?],
        };
        Ok(self.files(home).into_iter().zip(contents).collect())
    }

    /// The commands that start the job once its files are written.
    pub(crate) fn load_commands(self, home: &Path, uid: u32) -> Vec<Vec<String>> {
        match self {
            Scheduler::Launchd => vec![argv(&[
                "launchctl",
                "bootstrap",
                &format!("gui/{uid}"),
                &self.files(home)[0].display().to_string(),
            ])],
            Scheduler::Systemd => vec![
                argv(&["systemctl", "--user", "daemon-reload"]),
                argv(&[
                    "systemctl",
                    "--user",
                    "enable",
                    "--now",
                    &format!("{SYSTEMD_UNIT}.timer"),
                ]),
            ],
        }
    }

    /// The commands that stop the job before its files are removed.
    /// They fail harmlessly when it is not loaded.
    pub(crate) fn unload_commands(self, uid: u32) -> Vec<Vec<String>> {
        match self {
            Scheduler::Launchd => vec![argv(&[
                "launchctl",
                "bootout",
                &format!("gui/{uid}/{LAUNCHD_LABEL}"),
            ])],
            Scheduler::Systemd => vec![argv(&[
                "systemctl",
                "--user",
                "disable",
                "--now",
                &format!("{SYSTEMD_UNIT}.timer"),
            ])],
        }
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn absolute_utf8(path: &Path) -> Result<&str> {
    if !path.is_absolute() {
        bail!("the timer needs an absolute path: {}", path.display());
    }
    let value = path
        .to_str()
        .with_context(|| format!("the timer needs a UTF-8 path: {}", path.display()))?;
    if value.chars().any(char::is_control) {
        bail!("the timer cannot run a path with control characters: {value:?}");
    }
    Ok(value)
}

fn log_file(job: &Job) -> PathBuf {
    job.config_dir.join(crate::config::SYNC_LOG_FILE_NAME)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn launchd_plist(job: &Job) -> Result<String> {
    let binary = xml_escape(absolute_utf8(job.binary)?);
    let config_dir = xml_escape(absolute_utf8(job.config_dir)?);
    let log = xml_escape(absolute_utf8(&log_file(job))?);
    let env = crate::config::CONFIG_DIR_ENV;
    let seconds = job.interval_secs;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCHD_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{binary}</string>
		<string>sync</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>{env}</key>
		<string>{config_dir}</string>
	</dict>
	<key>StartInterval</key>
	<integer>{seconds}</integer>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    ))
}

/// A systemd directive value, quoted, with `%` specifiers escaped. In
/// `ExecStart` `$` is escaped too, so a path is never expanded as a
/// variable.
fn systemd_quote(value: &str, exec: bool) -> String {
    let mut escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if exec {
        escaped = escaped.replace('$', "$$");
    }
    format!("\"{escaped}\"")
}

pub(crate) fn systemd_service(job: &Job) -> Result<String> {
    let binary = systemd_quote(absolute_utf8(job.binary)?, true);
    let env = systemd_quote(
        &format!(
            "{}={}",
            crate::config::CONFIG_DIR_ENV,
            absolute_utf8(job.config_dir)?
        ),
        false,
    );
    // `append:` takes the path unquoted and expands specifiers.
    let log = absolute_utf8(&log_file(job))?.replace('%', "%%");
    if log.contains(char::is_whitespace) {
        bail!("systemd cannot append to a log path with whitespace: {log:?}");
    }
    Ok(format!(
        "[Unit]\n\
         Description=Upload agent sessions to Pathbase (path sync)\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         Environment={env}\n\
         ExecStart={binary} sync\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n"
    ))
}

pub(crate) fn systemd_timer(interval_secs: u64) -> String {
    format!(
        "[Unit]\n\
         Description=Run path sync every {interval_secs}s\n\
         \n\
         [Timer]\n\
         OnStartupSec=60s\n\
         OnUnitActiveSec={interval_secs}s\n\
         Unit={SYSTEMD_UNIT}.service\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

/// The interval an installed job runs at, read back from its files.
pub(crate) fn installed_interval(scheduler: Scheduler, home: &Path) -> Option<u64> {
    let files = scheduler.files(home);
    let text = std::fs::read_to_string(&files[0]).ok()?;
    match scheduler {
        Scheduler::Launchd => {
            let after = text.split("<key>StartInterval</key>").nth(1)?;
            let value = after
                .split("<integer>")
                .nth(1)?
                .split("</integer>")
                .next()?;
            value.trim().parse().ok()
        }
        Scheduler::Systemd => text
            .lines()
            .find_map(|l| l.strip_prefix("OnUnitActiveSec="))?
            .strip_suffix('s')?
            .parse()
            .ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job<'a>(binary: &'a str, config_dir: &'a str) -> Job<'a> {
        Job {
            binary: Path::new(binary),
            config_dir: Path::new(config_dir),
            interval_secs: 300,
        }
    }

    #[test]
    fn launchd_plist_runs_sync_by_absolute_path_and_logs_under_the_config_dir() {
        let plist = launchd_plist(&job("/opt/bin/path", "/Users/me/.toolpath")).unwrap();
        assert!(plist.contains("<string>net.toolpath.path-sync</string>"));
        assert!(plist.contains(
            "<array>\n\t\t<string>/opt/bin/path</string>\n\t\t<string>sync</string>\n\t</array>"
        ));
        assert!(
            plist.contains(
                "<key>TOOLPATH_CONFIG_DIR</key>\n\t\t<string>/Users/me/.toolpath</string>"
            )
        );
        assert!(plist.contains("<key>StartInterval</key>\n\t<integer>300</integer>"));
        assert!(plist.contains(
            "<key>StandardOutPath</key>\n\t<string>/Users/me/.toolpath/sync.log</string>"
        ));
        assert!(plist.contains(
            "<key>StandardErrorPath</key>\n\t<string>/Users/me/.toolpath/sync.log</string>"
        ));
    }

    #[test]
    fn launchd_plist_escapes_xml() {
        let plist = launchd_plist(&job("/a b/<p>&\"'", "/c&d")).unwrap();
        assert!(plist.contains("<string>/a b/&lt;p&gt;&amp;&quot;&apos;</string>"));
        assert!(plist.contains("<string>/c&amp;d</string>"));
    }

    #[test]
    fn systemd_units_run_sync_by_absolute_path_every_interval() {
        let j = job("/usr/local/bin/path", "/home/me/.toolpath");
        assert_eq!(
            systemd_service(&j).unwrap(),
            "[Unit]\n\
             Description=Upload agent sessions to Pathbase (path sync)\n\
             \n\
             [Service]\n\
             Type=oneshot\n\
             Environment=\"TOOLPATH_CONFIG_DIR=/home/me/.toolpath\"\n\
             ExecStart=\"/usr/local/bin/path\" sync\n\
             StandardOutput=append:/home/me/.toolpath/sync.log\n\
             StandardError=append:/home/me/.toolpath/sync.log\n"
        );
        assert_eq!(
            systemd_timer(300),
            "[Unit]\n\
             Description=Run path sync every 300s\n\
             \n\
             [Timer]\n\
             OnStartupSec=60s\n\
             OnUnitActiveSec=300s\n\
             Unit=path-sync.service\n\
             \n\
             [Install]\n\
             WantedBy=timers.target\n"
        );
    }

    #[test]
    fn systemd_quotes_specifiers_and_variables() {
        let service = systemd_service(&job("/a b/$x%y\"", "/c%d")).unwrap();
        assert!(
            service.contains("ExecStart=\"/a b/$$x%%y\\\"\" sync\n"),
            "{service}"
        );
        assert!(service.contains("Environment=\"TOOLPATH_CONFIG_DIR=/c%%d\"\n"));
        assert!(service.contains("StandardOutput=append:/c%%d/sync.log\n"));
        assert!(systemd_service(&job("/bin/path", "/has space")).is_err());
    }

    #[test]
    fn templates_refuse_relative_and_control_character_paths() {
        for (binary, config) in [
            ("path", "/cfg"),
            ("/bin/path", "cfg"),
            ("/bin/path\nExecStartPre=/bin/evil", "/cfg"),
        ] {
            assert!(launchd_plist(&job(binary, config)).is_err());
            assert!(systemd_service(&job(binary, config)).is_err());
        }
    }

    #[test]
    fn each_scheduler_names_its_files_and_commands() {
        let home = Path::new("/home/me");
        assert_eq!(
            Scheduler::Launchd.files(home),
            [PathBuf::from(
                "/home/me/Library/LaunchAgents/net.toolpath.path-sync.plist"
            )]
        );
        assert_eq!(
            Scheduler::Systemd.files(home),
            [
                PathBuf::from("/home/me/.config/systemd/user/path-sync.timer"),
                PathBuf::from("/home/me/.config/systemd/user/path-sync.service"),
            ]
        );
        assert_eq!(
            Scheduler::Launchd.load_commands(home, 501),
            [[
                "launchctl",
                "bootstrap",
                "gui/501",
                "/home/me/Library/LaunchAgents/net.toolpath.path-sync.plist"
            ]]
        );
        assert_eq!(
            Scheduler::Launchd.unload_commands(501),
            [["launchctl", "bootout", "gui/501/net.toolpath.path-sync"]]
        );
        assert_eq!(
            Scheduler::Systemd.load_commands(home, 1000),
            [
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "enable", "--now", "path-sync.timer"],
            ]
        );
        assert_eq!(
            Scheduler::Systemd.unload_commands(1000),
            [["systemctl", "--user", "disable", "--now", "path-sync.timer"]]
        );
    }

    #[test]
    fn the_interval_reads_back_from_the_rendered_files() {
        for scheduler in [Scheduler::Launchd, Scheduler::Systemd] {
            let home = tempfile::TempDir::new().unwrap();
            let j = job("/bin/path", "/cfg");
            for (path, content) in scheduler.render(home.path(), &j).unwrap() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, content).unwrap();
            }
            assert_eq!(installed_interval(scheduler, home.path()), Some(300));
        }
    }
}
