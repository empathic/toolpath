//! `path resume --remote`: plan a Claude session resume on a remote
//! host. Read-only: the command stops after the plan.
//!
//! The local host does all toolpath work. The remote runs two
//! constant `sh` scripts (`probe_host.sh` and `probe_project_dir.sh`,
//! next to this module) that print `TP_<NAME>=<value>` fact lines;
//! [`crate::ssh::parse_facts`] rejects any other output, so a login
//! banner cannot become a path component.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

use crate::ssh::{Destination, RemoteCommand, Transport, fail_unless_success, parse_facts};

/// Wall-clock bound on one probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a run would do, decided by the two remote facts of call 2.
/// The remote wins once it exists: nothing overwrites a remote
/// session file, and a live session is attached to as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunAction {
    /// tmux session live: attach.
    Attach,
    /// Session file present, no live session: launch, attach.
    Launch,
    /// Session file absent: ship, launch, attach.
    Ship,
}

/// The plan for one remote resume, assembled from the document and
/// the two probe calls.
struct RemotePlan {
    remote_home: String,
    claude_path: String,
    project_dir: String,
    session_id: String,
    session_file: String,
    tmux_name: String,
    action: RunAction,
}

/// Entry point. `local_home` and `local_cwd` are inputs so tests
/// control them; production passes the real ones.
pub(super) fn run_remote(
    document_json: &str,
    dest: &Destination,
    remote_dir_flag: Option<&str>,
    dry_run: bool,
    transport: &dyn Transport,
    local_home: Option<&Path>,
    local_cwd: &Path,
) -> Result<()> {
    let session_id = crate::claude_session::session_id_from_document_hash(document_json)?;
    let tmux_name = tmux_session_name(&session_id);
    if let Some(dir) = remote_dir_flag {
        crate::claude_session::parse_cwd_arg(dir)?;
    }

    let facts = probe_host(transport, dest)?;
    let project_dir = remote_project_dir(remote_dir_flag, &facts.home, local_home, local_cwd)?;

    let slug_dir = remote_slug_dir(&facts.home, &project_dir);
    let session_file = format!("{slug_dir}/{session_id}.jsonl");

    let dir_facts = probe_project_dir(transport, dest, &project_dir, &tmux_name, &session_file)?;
    match dir_facts.physical_dir.as_deref() {
        None => {
            bail!("project directory {project_dir} does not exist on {dest}; create it or pass -C")
        }
        Some(physical) if physical != project_dir => bail!(
            "project directory {project_dir} is not physical on {dest} \
             (it resolves to {physical}); pass the physical path: -C {physical}"
        ),
        Some(_) => {}
    }

    let action = if dir_facts.tmux_session_live {
        RunAction::Attach
    } else if dir_facts.session_file_exists {
        RunAction::Launch
    } else {
        RunAction::Ship
    };

    let plan = RemotePlan {
        remote_home: facts.home,
        claude_path: facts.claude,
        project_dir,
        session_id,
        session_file,
        tmux_name,
        action,
    };
    print_plan(&plan, dest);

    if dry_run {
        eprintln!("Dry run: nothing was written or launched.");
        return Ok(());
    }
    bail!(
        "`path resume --remote` stops after the plan for now; \
         ship, launch, and attach are not implemented yet. \
         Use scripts/resume-remote.sh to run the plan."
    );
}

fn print_plan(plan: &RemotePlan, dest: &Destination) {
    let action = match plan.action {
        RunAction::Attach => "attach to the live session. The remote tree and turns are kept.",
        RunAction::Launch => {
            "launch on the remote file, attach. The remote tree and turns are kept."
        }
        RunAction::Ship => "ship, launch, attach.",
    };
    eprintln!("Remote resume plan for {dest}:");
    eprintln!("  remote home:   {}", plan.remote_home);
    eprintln!("  claude:        {}", plan.claude_path);
    eprintln!("  project dir:   {}", plan.project_dir);
    eprintln!("  session ID:    {}", plan.session_id);
    eprintln!("  session file:  {}", plan.session_file);
    eprintln!("  tmux session:  {}", plan.tmux_name);
    eprintln!("  run:           {action}");
}

// ── Call 1: host facts ───────────────────────────────────────────────

/// Locations probed for `claude` when `command -v` finds nothing,
/// relative to the remote home. An ssh exec channel runs a non-login
/// shell whose PATH lacks the user's profile additions.
const CLAUDE_PROBE_LOCATIONS: [&str; 3] = [
    ".local/bin/claude",
    ".claude/local/claude",
    ".npm-global/bin/claude",
];

struct HostFacts {
    home: String,
    claude: String,
}

/// Remote home, claude path, and tmux presence, in one read-only call.
fn probe_host(transport: &dyn Transport, dest: &Destination) -> Result<HostFacts> {
    let command = RemoteCommand::script(include_str!("probe_host.sh")).args(CLAUDE_PROBE_LOCATIONS);
    let output = transport.run(dest, &command, None, PROBE_TIMEOUT)?;
    fail_unless_success(&output, "host probe", dest)?;
    let [home, claude, tmux] = parse_facts(&output, ["TP_HOME", "TP_CLAUDE", "TP_TMUX"])?;

    let home = captured_absolute_path(&home, "remote $HOME", dest)?;
    if claude.is_empty() {
        let probed: Vec<String> = CLAUDE_PROBE_LOCATIONS
            .iter()
            .map(|p| format!("~/{p}"))
            .collect();
        bail!(
            "claude not found on {dest}; probed PATH, {}",
            probed.join(", ")
        );
    }
    let claude = captured_absolute_path(&claude, "remote claude path", dest)?;
    if tmux != "ok" {
        bail!("tmux not found on {dest}");
    }
    Ok(HostFacts { home, claude })
}

// ── Call 2: project directory facts ──────────────────────────────────

struct ProjectDirFacts {
    /// `pwd -P` inside the directory, `None` when it is missing.
    physical_dir: Option<String>,
    tmux_session_live: bool,
    session_file_exists: bool,
}

/// The directory's physical path, the tmux session state, and the
/// session file's existence, in one read-only call.
fn probe_project_dir(
    transport: &dyn Transport,
    dest: &Destination,
    project_dir: &str,
    tmux_name: &str,
    session_file: &str,
) -> Result<ProjectDirFacts> {
    let command = RemoteCommand::script(include_str!("probe_project_dir.sh"))
        .arg(project_dir)
        .arg(tmux_name)
        .arg(session_file);
    let output = transport.run(dest, &command, None, PROBE_TIMEOUT)?;
    fail_unless_success(&output, "project directory probe", dest)?;
    let [pwd, session, target] = parse_facts(&output, ["TP_PWD", "TP_SESSION", "TP_TARGET"])?;
    Ok(ProjectDirFacts {
        physical_dir: if pwd.is_empty() { None } else { Some(pwd) },
        tmux_session_live: session == "live",
        session_file_exists: target == "yes",
    })
}

// ── Remote project directory ─────────────────────────────────────────

/// `-C`, else the local cwd with the local home swapped for the
/// remote home. Both go through
/// [`crate::claude_session::parse_cwd_arg`].
fn remote_project_dir(
    remote_dir_flag: Option<&str>,
    remote_home: &str,
    local_home: Option<&Path>,
    local_cwd: &Path,
) -> Result<String> {
    if let Some(dir) = remote_dir_flag {
        return crate::claude_session::parse_cwd_arg(dir);
    }
    let local_home =
        local_home.context("cannot determine the local home directory; pass -C <remote-dir>")?;
    let suffix = local_cwd
        .strip_prefix(local_home)
        .ok()
        .and_then(slash_joined)
        .with_context(|| {
            format!(
                "the local cwd {} is not under the local home {}; pass -C <remote-dir>",
                local_cwd.display(),
                local_home.display()
            )
        })?;
    let dir = if suffix.is_empty() {
        remote_home.to_string()
    } else {
        format!("{}/{}", remote_home.trim_end_matches('/'), suffix)
    };
    crate::claude_session::parse_cwd_arg(&dir)
}

// ── Small pure helpers ───────────────────────────────────────────────

/// `<home>/.claude/projects/<slug>` with `/` separators, whatever the
/// local separator is.
fn remote_slug_dir(remote_home: &str, project_dir: &str) -> String {
    format!(
        "{}/.claude/projects/{}",
        remote_home.trim_end_matches('/'),
        toolpath_claude::sanitize_project_path(project_dir)
    )
}

/// A value captured from the remote may only become a path component
/// if it starts with `/`.
fn captured_absolute_path(value: &str, what: &str, dest: &Destination) -> Result<String> {
    if !value.starts_with('/') {
        bail!("{what} from {dest} is not an absolute path (got {value:?})");
    }
    Ok(value.to_string())
}

/// `path-<first 8 characters of the session ID>`. The ID is a
/// hyphenated UUID, so the name is always a valid tmux session name.
fn tmux_session_name(session_id: &str) -> String {
    format!("path-{}", &session_id[..8])
}

/// The components of a relative path joined with `/`, `None` when a
/// component is not valid UTF-8. `Path::to_str` would keep the local
/// separator, which is `\` on a Windows local host.
fn slash_joined(rel: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::fake::FakeSsh;

    /// One valid single-path document with an agent actor, as text.
    fn doc_json() -> String {
        r#"{"graph":{"id":"g1"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[{"step":{"id":"s1","actor":"agent:claude-code","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#
            .to_string()
    }

    fn dest() -> Destination {
        Destination::parse("user@host").unwrap()
    }

    const HOME: &str = "/home/remote";
    const DIR: &str = "/home/remote/work";

    /// Queues the call-1 reply: home, claude on PATH, tmux ok.
    fn reply_host_ok(fake: &FakeSsh) {
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/usr/local/bin/claude\nTP_TMUX=ok\n"),
        );
    }

    /// Queues the call-2 reply from the three facts.
    fn reply_dir(fake: &FakeSsh, physical: &str, session: &str, target: &str) {
        fake.reply(
            0,
            &format!("TP_PWD={physical}\nTP_SESSION={session}\nTP_TARGET={target}\n"),
        );
    }

    fn run(fake: &FakeSsh, dry_run: bool) -> Result<()> {
        run_remote(
            &doc_json(),
            &dest(),
            Some(DIR),
            dry_run,
            fake,
            Some(Path::new("/home/local")),
            Path::new("/home/local/work"),
        )
    }

    #[test]
    fn file_absent_plans_a_ship_and_stops_without_dry_run() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "none", "no");
        let err = run(&fake, false).unwrap_err();
        assert!(err.to_string().contains("not implemented yet"), "{err:#}");
        // Read-only: both calls ran, neither fed stdin.
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        for call in &calls {
            assert!(call.input.is_none());
        }
    }

    #[test]
    fn dry_run_stops_cleanly_for_each_action() {
        for (session, target) in [("none", "no"), ("none", "yes"), ("live", "yes")] {
            let fake = FakeSsh::new();
            reply_host_ok(&fake);
            reply_dir(&fake, DIR, session, target);
            run(&fake, true).unwrap();
            assert_eq!(fake.calls().len(), 2);
        }
    }

    #[test]
    fn call_2_carries_the_dir_the_exact_tmux_name_and_the_target() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "none", "no");
        run(&fake, true).unwrap();
        let session_id = crate::claude_session::session_id_from_document_hash(&doc_json()).unwrap();
        let command = &fake.calls()[1].command;
        assert!(command.contains(DIR), "{command}");
        assert!(
            command.contains(&format!("path-{}", &session_id[..8])),
            "{command}"
        );
        assert!(
            command.contains(&format!("{session_id}.jsonl")),
            "{command}"
        );
    }

    #[test]
    fn an_invalid_c_flag_errors_before_any_remote_call() {
        let fake = FakeSsh::new();
        let err = run_remote(
            &doc_json(),
            &dest(),
            Some("relative/dir"),
            true,
            &fake,
            None,
            Path::new("/"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("absolute POSIX path"), "{err:#}");
        assert!(
            fake.calls().is_empty(),
            "no remote call before the -C check"
        );
    }

    #[test]
    fn missing_dir_errors_and_names_it() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, "", "none", "no");
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err:#}");
        assert!(err.to_string().contains(DIR), "{err:#}");
    }

    #[test]
    fn non_physical_dir_errors_with_the_c_hint() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, "/private/home/remote/work", "none", "no");
        let err = run(&fake, true).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("not physical"), "{err:#}");
        assert!(text.contains("-C /private/home/remote/work"), "{err:#}");
    }

    #[test]
    fn a_login_banner_errors_and_quotes_the_reply() {
        let fake = FakeSsh::new();
        fake.reply(0, "Welcome to the machine!\nTP_HOME=/home/remote\n");
        let err = run(&fake, true).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("login banner"), "{text}");
        assert!(text.contains("Welcome to the machine!"), "{text}");
    }

    #[test]
    fn missing_claude_and_missing_tmux_error() {
        let fake = FakeSsh::new();
        fake.reply(0, &format!("TP_HOME={HOME}\nTP_CLAUDE=\nTP_TMUX=ok\n"));
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("claude not found"), "{err:#}");
        assert!(err.to_string().contains(".local/bin/claude"), "{err:#}");

        let fake = FakeSsh::new();
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/usr/bin/claude\nTP_TMUX=missing\n"),
        );
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("tmux not found"), "{err:#}");
    }

    #[test]
    fn remote_dir_defaults_to_home_swap_and_validates() {
        assert_eq!(
            remote_project_dir(
                None,
                HOME,
                Some(Path::new("/home/local")),
                Path::new("/home/local/a/b"),
            )
            .unwrap(),
            "/home/remote/a/b"
        );
        assert_eq!(
            remote_project_dir(
                None,
                HOME,
                Some(Path::new("/home/local")),
                Path::new("/home/local"),
            )
            .unwrap(),
            HOME
        );
        let err = remote_project_dir(
            None,
            HOME,
            Some(Path::new("/home/local")),
            Path::new("/elsewhere"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("pass -C"), "{err:#}");
        assert_eq!(
            remote_project_dir(Some("/x/y/"), HOME, None, Path::new("/")).unwrap(),
            "/x/y"
        );
        for bad in ["relative", "/a/../b", "/a/./b", "/a//b", "/a\nb"] {
            assert!(remote_project_dir(Some(bad), HOME, None, Path::new("/")).is_err());
        }
    }

    #[test]
    fn tmux_name_is_path_plus_the_first_8_of_the_id() {
        assert_eq!(
            tmux_session_name("b7e1c0de-0000-4000-8000-000000000001"),
            "path-b7e1c0de"
        );
    }

    #[test]
    fn slug_dir_uses_forward_slashes() {
        assert_eq!(
            remote_slug_dir("/home/remote", "/home/remote/a/b"),
            "/home/remote/.claude/projects/-home-remote-a-b"
        );
        assert_eq!(
            remote_slug_dir("/home/remote/", "/home/remote"),
            "/home/remote/.claude/projects/-home-remote"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_cwd_yields_a_slash_suffix() {
        assert_eq!(
            remote_project_dir(
                None,
                "/home/remote",
                Some(Path::new(r"C:\Users\alex")),
                Path::new(r"C:\Users\alex\proj\sub"),
            )
            .unwrap(),
            "/home/remote/proj/sub"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_slug_dir_matches_the_unix_slug() {
        assert_eq!(
            remote_slug_dir("/home/remote", "/home/remote/proj/sub"),
            "/home/remote/.claude/projects/-home-remote-proj-sub"
        );
    }
}
