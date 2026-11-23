//! Run an agent so that it sees only what the caller passes in.
//!
//! The child gets an EMPTY environment except: `HOME` and `TMPDIR` (fresh temporary directories,
//! removed when the [`Hermetic`] is dropped), `PATH=/usr/bin:/bin` (system tools only: never the
//! developer's PATH, Homebrew or a version manager) and `LANG=C.UTF-8`. Its working directory is
//! `/`. Anything else (a fixture's config, a webmock URL, a fake credential) must be added with
//! [`Hermetic::env`] or written under [`Hermetic::home`] by the test, so the inputs of a run are
//! exactly what the test names. Real credentials are never inherited.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

pub const PATH: &str = "/usr/bin:/bin";

pub struct Hermetic {
    exe: PathBuf,
    home: tempfile::TempDir,
    tmp: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Hermetic {
    /// `exe` is an installed agent (from [`crate::Cache::install`]), never a name looked up on PATH.
    pub fn new(exe: impl Into<PathBuf>) -> Result<Hermetic> {
        Ok(Hermetic {
            exe: exe.into(),
            home: tempfile::Builder::new().prefix("agent-home.").tempdir()?,
            tmp: tempfile::Builder::new().prefix("agent-tmp.").tempdir()?,
            env: Vec::new(),
        })
    }

    /// The child's HOME, for the test to seed config or fixtures into before running.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Add one variable to the child's otherwise empty environment.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Hermetic {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The command, ready for args; the [`Hermetic`] must outlive the child (it owns HOME/TMPDIR).
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut c = Command::new(&self.exe);
        c.args(args)
            .env_clear()
            .env("HOME", self.home.path())
            .env("TMPDIR", self.tmp.path())
            .env("PATH", PATH)
            .env("LANG", "C.UTF-8")
            .current_dir("/");
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in agent that reports what it sees: its environment and working directory.
    fn reporter(dir: &Path) -> PathBuf {
        let p = dir.join("reporter");
        std::fs::write(&p, "#!/bin/sh\npwd\nenv | LC_ALL=C sort\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn the_child_sees_only_a_temp_home_system_path_and_what_the_test_adds() {
        // The test process has HOME, PATH, CARGO_* and more; the exact key list below proves
        // none of them reach the child.
        let dir = tempfile::tempdir().unwrap();
        let h = Hermetic::new(reporter(dir.path()))
            .unwrap()
            .env("WEBMOCK_URL", "http://127.0.0.1:1");
        let out = h.command(Vec::<&str>::new()).output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("/"), "cwd is /");
        let keys: Vec<&str> = lines
            .map(|l| l.split('=').next().unwrap())
            .filter(|k| !["PWD", "SHLVL", "_", "OLDPWD"].contains(k)) // set by sh itself
            .collect();
        assert_eq!(
            keys,
            ["HOME", "LANG", "PATH", "TMPDIR", "WEBMOCK_URL"],
            "{text}"
        );
        assert!(text.contains(&format!("HOME={}", h.home().display())));
        assert!(text.contains("PATH=/usr/bin:/bin\n"));
    }

    #[test]
    fn home_and_tmpdir_are_fresh_per_run_and_removed_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let exe = reporter(dir.path());
        let a = Hermetic::new(&exe).unwrap();
        let b = Hermetic::new(&exe).unwrap();
        assert_ne!(a.home(), b.home());
        let home = a.home().to_path_buf();
        assert!(home.is_dir());
        drop(a);
        assert!(!home.exists());
    }
}
