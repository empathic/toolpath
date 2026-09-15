//! `path resume --remote`: plan a Claude session resume on a remote
//! host. Read-only: the command stops after the plan.
//!
//! The local host does all toolpath work. The remote runs a constant
//! `sh` script (`probe_host.sh`, next to this module) that prints
//! `TP_<NAME>=<value>` fact lines; [`crate::ssh::parse_facts`] rejects
//! any other output, so a login banner cannot become a path component.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

use crate::harness::Harness;
use crate::ssh::{Destination, RemoteCommand, Transport, fail_unless_success, parse_facts};

/// Wall-clock bound on one probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Locations probed for `claude` when `command -v` finds nothing,
/// relative to the remote home. An ssh exec channel runs a non-login
/// shell whose PATH lacks the user's profile additions.
const CLAUDE_PROBE_LOCATIONS: [&str; 3] = [
    ".local/bin/claude",
    ".claude/local/claude",
    ".npm-global/bin/claude",
];

/// The facts `probe_host.sh` prints, in order.
const HOST_FACT_TAGS: [&str; 3] = ["TP_HOME", "TP_CLAUDE", "TP_TMUX"];

/// The two values a probe script prints for a yes-or-no fact.
const FLAG_YES: &str = "yes";
const FLAG_NO: &str = "no";

/// The `path resume` flags for a remote resume.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote resume")]
pub struct RemoteArgs {
    /// Plan the resume on this ssh destination instead of this
    /// machine (`user@host` or `user@host:port`; Claude only). With
    /// `--remote`, `-C` names the remote project directory; default:
    /// the local cwd with the local home swapped for the remote home.
    /// Read-only: the command stops after the plan.
    #[arg(long = "remote", value_name = "DEST", value_parser = Destination::parse)]
    pub dest: Option<Destination>,

    /// Stop after printing the plan. Only with --remote.
    #[arg(long, requires = "dest")]
    pub dry_run: bool,
}

/// Error unless the harness being resumed into is Claude, the one the
/// remote projection exists for. `harness` is the `--harness` flag and
/// `source` the document's source harness.
pub(super) fn require_harness_is_claude(
    harness: Option<Harness>,
    source: Option<Harness>,
) -> Result<()> {
    match (harness, source) {
        (Some(Harness::Claude), _) | (None, Some(Harness::Claude)) => Ok(()),
        (Some(h), _) => bail!(
            "remote resume supports claude only (got --harness {})",
            h.name()
        ),
        (None, source) => bail!(
            "remote resume supports claude only; the document's source is {}. \
             Pass `--harness claude` to force a Claude projection.",
            source.map_or("unknown", |h| h.name())
        ),
    }
}

/// Entry point. `remote_dir` is the `-C` value.
pub(super) fn resume(
    dest: &Destination,
    remote_dir: Option<&Path>,
    dry_run: bool,
    transport: &dyn Transport,
    local_home: &Path,
    local_cwd: &Path,
) -> Result<()> {
    let remote_dir = remote_dir
        .map(|p| p.to_str().context("-C must be valid UTF-8"))
        .transpose()?
        .map(crate::claude_session::parse_posix_dir)
        .transpose()?;

    let facts = probe_host(transport, dest)?;
    let project_dir = match remote_dir {
        Some(dir) => dir,
        None => swap_home(local_cwd, local_home, &facts.home)?,
    };
    print_plan(&facts, &project_dir, dest);

    if dry_run {
        eprintln!("Dry run: nothing was written or launched.");
        return Ok(());
    }
    bail!(
        "`path resume --remote` stops after the plan; \
         ship, launch, and attach are not implemented yet. \
         Use scripts/resume-remote.sh to run the plan."
    );
}

fn print_plan(facts: &HostFacts, project_dir: &str, dest: &Destination) {
    eprintln!("Remote resume plan for {dest}:");
    eprintln!("  remote home:   {}", facts.home);
    eprintln!("  claude:        {}", facts.claude);
    eprintln!("  project dir:   {project_dir}");
}

struct HostFacts {
    home: String,
    claude: String,
}

/// Remote home, claude path, and tmux presence, in one read-only call.
fn probe_host(transport: &dyn Transport, dest: &Destination) -> Result<HostFacts> {
    let command = RemoteCommand::from_script(include_str!("probe_host.sh"), CLAUDE_PROBE_LOCATIONS);
    let output = transport.run(dest, &command, PROBE_TIMEOUT)?;
    fail_unless_success(&output, "host probe", dest)?;
    let [home, claude, tmux] = parse_facts(&output, HOST_FACT_TAGS)?;

    let home = require_absolute_path(&home, "remote $HOME", dest)?;
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
    let claude = require_absolute_path(&claude, "remote claude path", dest)?;
    if !parse_flag(&tmux, "tmux on PATH", dest)? {
        bail!("tmux not found on {dest}");
    }
    Ok(HostFacts { home, claude })
}

/// The local cwd with the local home swapped for the remote home,
/// checked by [`crate::claude_session::parse_posix_dir`].
fn swap_home(local_cwd: &Path, local_home: &Path, remote_home: &str) -> Result<String> {
    let suffix = local_cwd
        .strip_prefix(local_home)
        .ok()
        .and_then(Path::to_str)
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
    crate::claude_session::parse_posix_dir(&dir)
}

/// A value captured from the remote may only become a path component
/// if it starts with `/`.
fn require_absolute_path(value: &str, what: &str, dest: &Destination) -> Result<String> {
    if !value.starts_with('/') {
        bail!("{what} from {dest} is not an absolute path (got {value:?})");
    }
    Ok(value.to_string())
}

/// A yes-or-no fact from a probe script. Any other value is an
/// error, not `false`.
fn parse_flag(value: &str, what: &str, dest: &Destination) -> Result<bool> {
    match value {
        FLAG_YES => Ok(true),
        FLAG_NO => Ok(false),
        other => bail!("{what} from {dest} is not {FLAG_YES} or {FLAG_NO} (got {other:?})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::fake::FakeSsh;

    fn dest() -> Destination {
        Destination::parse("user@host").unwrap()
    }

    const HOME: &str = "/home/remote";
    const DIR: &str = "/home/remote/work";

    /// Queues the host probe reply: home, claude on PATH, tmux found.
    fn reply_host_ok(fake: &FakeSsh) {
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/usr/local/bin/claude\nTP_TMUX=yes\n"),
        );
    }

    fn run(fake: &FakeSsh, dry_run: bool) -> Result<()> {
        resume(
            &dest(),
            Some(Path::new(DIR)),
            dry_run,
            fake,
            Path::new("/home/local"),
            Path::new("/home/local/work"),
        )
    }

    #[test]
    fn the_probe_script_prints_every_fact_tag_and_both_flag_values() {
        let script = include_str!("probe_host.sh");
        for tag in HOST_FACT_TAGS {
            assert!(script.contains(&format!("{tag}=")), "{tag}");
        }
        for flag in [FLAG_YES, FLAG_NO] {
            assert!(script.contains(&format!("={flag}")), "{flag}");
        }
    }

    #[test]
    fn a_flag_that_is_neither_yes_nor_no_errors() {
        assert!(parse_flag("yes", "x", &dest()).unwrap());
        assert!(!parse_flag("no", "x", &dest()).unwrap());
        let err = parse_flag("ok", "tmux on PATH", &dest()).unwrap_err();
        assert!(err.to_string().contains("tmux on PATH"), "{err:#}");
        assert!(err.to_string().contains("\"ok\""), "{err:#}");
    }

    #[test]
    fn require_harness_is_claude_rejects_other_harnesses_and_names_the_fix() {
        assert!(require_harness_is_claude(None, Some(Harness::Claude)).is_ok());
        assert!(require_harness_is_claude(Some(Harness::Claude), Some(Harness::Codex)).is_ok());
        let err = require_harness_is_claude(None, Some(Harness::Codex)).unwrap_err();
        assert!(err.to_string().contains("supports claude only"), "{err:#}");
        assert!(err.to_string().contains("--harness claude"), "{err:#}");
        let err = require_harness_is_claude(Some(Harness::Codex), None).unwrap_err();
        assert!(err.to_string().contains("got --harness codex"), "{err:#}");
    }

    #[test]
    fn the_plan_stops_without_dry_run() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        let err = run(&fake, false).unwrap_err();
        assert!(err.to_string().contains("not implemented yet"), "{err:#}");
        // Read-only: one call ran, and it fed no stdin.
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].input.is_none());
    }

    #[test]
    fn dry_run_stops_cleanly() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        run(&fake, true).unwrap();
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn an_invalid_c_flag_errors_before_any_remote_call() {
        let fake = FakeSsh::new();
        let err = resume(
            &dest(),
            Some(Path::new("relative/dir")),
            true,
            &fake,
            Path::new("/"),
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
    fn missing_claude_and_missing_tmux_error() {
        let fake = FakeSsh::new();
        fake.reply(0, &format!("TP_HOME={HOME}\nTP_CLAUDE=\nTP_TMUX=yes\n"));
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("claude not found"), "{err:#}");
        assert!(err.to_string().contains(".local/bin/claude"), "{err:#}");

        let fake = FakeSsh::new();
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/usr/bin/claude\nTP_TMUX=no\n"),
        );
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("tmux not found"), "{err:#}");
    }

    #[test]
    fn the_default_remote_dir_swaps_the_home() {
        let local_home = Path::new("/home/local");
        assert_eq!(
            swap_home(Path::new("/home/local/a/b"), local_home, HOME).unwrap(),
            "/home/remote/a/b"
        );
        assert_eq!(
            swap_home(Path::new("/home/local"), local_home, HOME).unwrap(),
            HOME
        );
        let err = swap_home(Path::new("/elsewhere"), local_home, HOME).unwrap_err();
        assert!(err.to_string().contains("pass -C"), "{err:#}");
    }
}
