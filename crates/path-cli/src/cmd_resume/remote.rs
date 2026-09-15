//! `path resume --remote`: resume a Claude session on a remote host
//! under tmux, with this terminal attached to it.
//!
//! The local host does all toolpath work: it projects the
//! conversation in memory, renames it to the content-addressed
//! session ID, roots it at the remote project directory, and uploads
//! the JSONL over ssh stdin. The remote runs no `path`.
//!
//! The remote wins once it exists: a live tmux session is attached
//! to as is, a present session file is launched as is, and only an
//! absent file is shipped. Two read-only probes decide which; the
//! first remote write is the ship.
//!
//! The remote runs constant `sh` scripts next to this module. The
//! probes print `TP_<NAME>=<value>` fact lines; [`crate::ssh::parse_facts`]
//! rejects any other output, so a login banner cannot become a path
//! component.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

use crate::harness::Harness;
use crate::ssh::{
    DEAD_PEER_TIMEOUT, Destination, RemoteCommand, Transport, fail_unless_success, parse_facts,
};

/// Wall-clock bound on a probe, a kill, or the launch, so a timeout
/// means a live remote that is stuck.
const COMMAND_TIMEOUT: Duration = DEAD_PEER_TIMEOUT;

/// Wall-clock bound on the upload: `COMMAND_TIMEOUT` plus one second per
/// 64 KiB, the time a 512 kbit/s uplink needs.
fn upload_timeout(bytes: usize) -> Duration {
    COMMAND_TIMEOUT + Duration::from_secs((bytes / (64 * 1024)) as u64)
}

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

/// The facts `probe_project_dir.sh` prints, in order.
const DIR_FACT_TAGS: [&str; 4] = ["TP_PWD", "TP_SESSION", "TP_PANE_DEAD", "TP_TARGET"];

/// The `path resume` flags for a remote resume.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote resume")]
pub struct RemoteArgs {
    /// Resume on this ssh destination instead of this machine
    /// (`user@host` or `user@host:port`; Claude only). With `--remote`,
    /// `-C` names the remote project directory; default: the local cwd
    /// with the local home swapped for the remote home. The session is
    /// shipped when the remote lacks it, `claude -r` starts under tmux,
    /// and this terminal attaches; a live tmux session or a present
    /// session file on the remote is used as is. Detach with ctrl-b d.
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

/// Error unless stdin and stdout are terminals, which the attach
/// needs; the caller says whether each is one.
pub(super) fn require_a_terminal(stdin_is_tty: bool, stdout_is_tty: bool) -> Result<()> {
    let not_a_tty = if !stdin_is_tty {
        Some("stdin")
    } else if !stdout_is_tty {
        Some("stdout")
    } else {
        None
    };
    if let Some(stream) = not_a_tty {
        bail!(
            "`path resume --remote` needs an interactive terminal for the \
             tmux attach: {stream} is not a TTY (pass --dry-run to stop at the plan)"
        );
    }
    Ok(())
}

/// One remote resume: the document, where it goes, and the local
/// context the default remote directory and the attach are derived
/// from.
pub(super) struct RemoteResume<'a> {
    pub(super) document: &'a toolpath::v1::Path,
    /// The text `document` was parsed from; the session ID hashes it.
    pub(super) document_json: &'a str,
    pub(super) dest: &'a Destination,
    /// The `-C` value.
    pub(super) remote_dir: Option<&'a Path>,
    pub(super) dry_run: bool,
    pub(super) local_home: &'a Path,
    pub(super) local_cwd: &'a Path,
    /// This terminal's type, for the PTY the attach requests.
    pub(super) term: Option<&'a str>,
}

/// The first step a run takes, decided by the project directory
/// facts. Each variant implies the steps after it: a launch ends in
/// an attach, an upload ends in a launch and an attach. The remote
/// wins once it exists: nothing overwrites a remote session file, and
/// a live session is attached to as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunAction {
    /// The tmux session is live: attach to it.
    AttachTmux,
    /// The session file is present and no tmux session is live: start
    /// claude on that file in a new tmux session, then attach.
    LaunchClaude,
    /// The session file is absent: upload the local projection to the
    /// session file over ssh, then launch claude, then attach.
    UploadSession,
}

/// Where a remote resume lands, resolved from the document and the
/// host probe. Every value is fixed before the project directory is
/// probed.
struct RemoteTarget {
    remote_home: String,
    claude_path: String,
    project_dir: String,
    session_id: String,
    session_file: String,
    tmux_name: String,
}

/// The plan for one remote resume: the target and what the project
/// directory probe decided.
struct RemotePlan {
    target: RemoteTarget,
    /// The tmux session exists with a dead pane (a previous launch
    /// exited non-zero); it is killed before the launch.
    dead_session: bool,
    action: RunAction,
}

/// Entry point: probes, plan, then ship, launch, and attach as the
/// remote state requires. Returns the exit status of the attach, 0
/// for a dry run.
pub(super) fn resume(request: &RemoteResume, transport: &dyn Transport) -> Result<u32> {
    let RemoteResume {
        document,
        document_json,
        dest,
        remote_dir,
        dry_run,
        local_home,
        local_cwd,
        term,
    } = *request;
    let remote_dir = remote_dir
        .map(|p| p.to_str().context("-C must be valid UTF-8"))
        .transpose()?
        .map(crate::claude_session::parse_posix_dir)
        .transpose()?;
    let session_id = crate::claude_session::generate_content_addressed_session_id(document_json)?;
    let tmux_name = format_tmux_session_name(&session_id);

    let facts = probe_host(transport, dest)?;
    let project_dir = match remote_dir {
        Some(dir) => dir,
        None => swap_home(local_cwd, local_home, &facts.home)?,
    };
    let session_file = toolpath_claude::PathResolver::new()
        .with_home(&facts.home)
        .conversation_file(&project_dir, &session_id)
        .context("build the remote session file path")?;
    let session_file = session_file
        .to_str()
        .context("the remote session file path is not valid UTF-8")?
        .to_string();
    let target = RemoteTarget {
        remote_home: facts.home,
        claude_path: facts.claude,
        project_dir,
        session_id,
        session_file,
        tmux_name,
    };

    let dir_facts = probe_project_dir(transport, dest, &target)?;
    let project_dir = &target.project_dir;
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

    let dead_session = dir_facts.tmux_session && dir_facts.pane_dead;
    let action = if dir_facts.tmux_session && !dir_facts.pane_dead {
        RunAction::AttachTmux
    } else if dir_facts.session_file_exists {
        RunAction::LaunchClaude
    } else {
        RunAction::UploadSession
    };

    let plan = RemotePlan {
        target,
        dead_session,
        action,
    };
    print_plan(&plan, dest);

    if dry_run {
        eprintln!("Dry run: nothing was written or launched.");
        return Ok(0);
    }

    if plan.action == RunAction::UploadSession {
        upload(document, &plan.target, dest, transport)?;
    }
    if plan.action != RunAction::AttachTmux {
        if plan.dead_session {
            kill_dead_session(&plan.target, dest, transport)?;
        }
        launch(&plan.target, dest, transport)?;
    }

    eprintln!(
        "Attaching to {} on {dest} (detach with ctrl-b d)",
        plan.target.tmux_name
    );
    // `-u` forces UTF-8 output: the PTY channel carries no locale.
    // `=` pins the exact session name; `-d` detaches a stale client.
    let attach_command = RemoteCommand::new([
        "tmux",
        "-u",
        "attach-session",
        "-d",
        "-t",
        &format!("={}", plan.target.tmux_name),
    ]);
    transport.attach(dest, &attach_command, term)
}

/// Project the conversation under the plan's session ID and project
/// directory, and write it to the remote session file over stdin.
fn upload(
    document: &toolpath::v1::Path,
    target: &RemoteTarget,
    dest: &Destination,
    transport: &dyn Transport,
) -> Result<()> {
    let mut conversation = crate::cmd_export::build_claude_conversation(document)?;
    conversation.rename_session(&target.session_id);
    conversation.reroot(&target.project_dir);
    let jsonl = crate::cmd_export::serialize_jsonl(&conversation)?.into_bytes();

    eprintln!(
        "Uploading session {} to {dest}:{}",
        target.session_id, target.session_file
    );
    let bytes = jsonl.len();
    let command = RemoteCommand::from_script(
        include_str!("upload_session.sh"),
        [target.session_file.as_str(), &bytes.to_string()],
    )
    .stdin(jsonl);
    let output = transport.run(dest, &command, upload_timeout(bytes))?;
    fail_unless_success(&output, "uploading the session", dest)
}

fn kill_dead_session(
    target: &RemoteTarget,
    dest: &Destination,
    transport: &dyn Transport,
) -> Result<()> {
    eprintln!("Killing the dead tmux session {}", target.tmux_name);
    let command = RemoteCommand::new([
        "tmux",
        "kill-session",
        "-t",
        &format!("={}", target.tmux_name),
    ]);
    let output = transport.run(dest, &command, COMMAND_TIMEOUT)?;
    fail_unless_success(&output, "killing the dead tmux session", dest)
}

/// Start `claude -r <id>` in a detached tmux session.
fn launch(target: &RemoteTarget, dest: &Destination, transport: &dyn Transport) -> Result<()> {
    eprintln!("Launching {} in {}", target.tmux_name, target.project_dir);
    // tmux hands the command to `sh -c`, so it is quoted for that
    // shell here, not in the script.
    let claude_command = shlex::try_join([
        "env",
        "LANG=C.UTF-8",
        &target.claude_path,
        "-r",
        &target.session_id,
    ])
    .context("quote the claude command for the remote shell")?;
    let command = RemoteCommand::from_script(
        include_str!("launch_session.sh"),
        [
            target.tmux_name.as_str(),
            target.project_dir.as_str(),
            claude_command.as_str(),
        ],
    );
    let output = transport.run(dest, &command, COMMAND_TIMEOUT)?;
    fail_unless_success(&output, "launching the tmux session", dest)
}

fn print_plan(plan: &RemotePlan, dest: &Destination) {
    let action = match (plan.action, plan.dead_session) {
        (RunAction::AttachTmux, _) => {
            "attach to the live session. The remote tree and turns are kept."
        }
        (RunAction::LaunchClaude, false) => {
            "launch on the remote file, attach. The remote tree and turns are kept."
        }
        (RunAction::LaunchClaude, true) => {
            "kill the dead tmux session, launch on the remote file, attach. \
             The remote tree and turns are kept."
        }
        (RunAction::UploadSession, false) => "ship, launch, attach.",
        (RunAction::UploadSession, true) => "ship, kill the dead tmux session, launch, attach.",
    };
    let target = &plan.target;
    eprintln!("Remote resume plan for {dest}:");
    eprintln!("  remote home:   {}", target.remote_home);
    eprintln!("  claude:        {}", target.claude_path);
    eprintln!("  project dir:   {}", target.project_dir);
    eprintln!("  session ID:    {}", target.session_id);
    eprintln!("  session file:  {}", target.session_file);
    eprintln!("  tmux session:  {}", target.tmux_name);
    eprintln!("  run:           {action}");
}

struct HostFacts {
    home: String,
    claude: String,
}

/// Remote home, claude path, and tmux presence, in one read-only call.
fn probe_host(transport: &dyn Transport, dest: &Destination) -> Result<HostFacts> {
    let command = RemoteCommand::from_script(include_str!("probe_host.sh"), CLAUDE_PROBE_LOCATIONS);
    let output = transport.run(dest, &command, COMMAND_TIMEOUT)?;
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

struct ProjectDirFacts {
    /// `pwd -P` inside the directory, `None` when it is missing.
    physical_dir: Option<String>,
    /// A tmux session with the derived name exists.
    tmux_session: bool,
    /// That session's pane is dead: kept by `remain-on-exit` after a
    /// non-zero exit.
    pane_dead: bool,
    session_file_exists: bool,
}

/// The directory's physical path, the tmux session state, and the
/// session file's existence, in one read-only call.
fn probe_project_dir(
    transport: &dyn Transport,
    dest: &Destination,
    target: &RemoteTarget,
) -> Result<ProjectDirFacts> {
    let command = RemoteCommand::from_script(
        include_str!("probe_project_dir.sh"),
        [
            target.project_dir.as_str(),
            target.tmux_name.as_str(),
            target.session_file.as_str(),
        ],
    );
    let output = transport.run(dest, &command, COMMAND_TIMEOUT)?;
    fail_unless_success(&output, "project directory probe", dest)?;
    let [pwd, session, pane_dead, target] = parse_facts(&output, DIR_FACT_TAGS)?;
    Ok(ProjectDirFacts {
        physical_dir: if pwd.is_empty() { None } else { Some(pwd) },
        tmux_session: parse_flag(&session, "tmux session exists", dest)?,
        pane_dead: parse_flag(&pane_dead, "tmux pane dead", dest)?,
        session_file_exists: parse_flag(&target, "session file present", dest)?,
    })
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

/// `path-<first 8 characters of the session ID>`. The ID is a
/// hyphenated UUID, so the name is always a valid tmux session name.
fn format_tmux_session_name(session_id: &str) -> String {
    format!("path-{}", &session_id[..8])
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
    use crate::ssh::fake::{Call, FakeSsh};

    /// The rendered command of a call, run or attach.
    fn command_of(call: &Call) -> &str {
        match call {
            Call::Run { command, .. } | Call::Attach { command, .. } => command,
        }
    }

    /// The stdin bytes of a run; `None` for an attach or a closed stdin.
    fn input_of(call: &Call) -> Option<&[u8]> {
        match call {
            Call::Run { input, .. } => input.as_deref(),
            Call::Attach { .. } => None,
        }
    }

    /// The call attaches to the exact tmux session of [`doc_json`].
    fn assert_attaches(call: &Call) {
        assert!(matches!(call, Call::Attach { .. }), "{call:?}");
        let command = command_of(call);
        assert!(
            command.contains(&format!("=path-{}", &session_id()[..8])),
            "{command}"
        );
    }

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

    /// Queues the host probe reply: home, claude on PATH, tmux found.
    fn reply_host_ok(fake: &FakeSsh) {
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/usr/local/bin/claude\nTP_TMUX=yes\n"),
        );
    }

    /// Queues the project directory probe reply from the four facts.
    fn reply_dir(fake: &FakeSsh, physical: &str, session: &str, dead: &str, target: &str) {
        fake.reply(
            0,
            &format!(
                "TP_PWD={physical}\nTP_SESSION={session}\nTP_PANE_DEAD={dead}\nTP_TARGET={target}\n"
            ),
        );
    }

    /// Runs the resume for [`doc_json`] with `-C DIR`.
    fn run(fake: &FakeSsh, dry_run: bool) -> Result<u32> {
        run_with_dir(fake, dry_run, Path::new(DIR))
    }

    fn run_with_dir(fake: &FakeSsh, dry_run: bool, remote_dir: &Path) -> Result<u32> {
        let json = doc_json();
        let graph = toolpath::v1::Graph::from_json(&json).unwrap();
        resume(
            &RemoteResume {
                document: graph.single_path().unwrap(),
                document_json: &json,
                dest: &dest(),
                remote_dir: Some(remote_dir),
                dry_run,
                local_home: Path::new("/home/local"),
                local_cwd: Path::new("/home/local/work"),
                term: Some("xterm-test"),
            },
            fake,
        )
    }

    /// The content-addressed session ID of [`doc_json`].
    fn session_id() -> String {
        crate::claude_session::generate_content_addressed_session_id(&doc_json()).unwrap()
    }

    /// The tags and the marker values the Rust side reads are the
    /// ones the scripts print.
    #[test]
    fn the_probe_scripts_print_every_fact_tag_and_marker() {
        let host = include_str!("probe_host.sh");
        let dir = include_str!("probe_project_dir.sh");
        for (script, tags) in [(host, &HOST_FACT_TAGS[..]), (dir, &DIR_FACT_TAGS[..])] {
            for tag in tags {
                assert!(script.contains(&format!("{tag}=")), "{tag}");
            }
            for flag in [FLAG_YES, FLAG_NO] {
                assert!(script.contains(&format!("={flag}")), "{flag}");
            }
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
    fn require_a_terminal_names_the_stream_that_is_not_a_tty() {
        assert!(require_a_terminal(true, true).is_ok());
        let err = require_a_terminal(false, true).unwrap_err();
        assert!(err.to_string().contains("stdin is not a TTY"), "{err:#}");
        let err = require_a_terminal(true, false).unwrap_err();
        assert!(err.to_string().contains("stdout is not a TTY"), "{err:#}");
    }

    #[test]
    fn a_non_zero_attach_status_is_returned() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "yes", "no", "yes");
        fake.reply(2, ""); // attach
        assert_eq!(run(&fake, false).unwrap(), 2);
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
    fn file_absent_ships_launches_and_attaches() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "no", "no", "no");
        fake.reply(0, ""); // upload
        fake.reply(0, ""); // launch
        assert_eq!(run(&fake, false).unwrap(), 0);

        let calls = fake.calls();
        assert_eq!(calls.len(), 5);
        // Probes are read-only.
        for call in &calls[..2] {
            assert!(matches!(call, Call::Run { input: None, .. }), "{call:?}");
        }
        let id = session_id();
        let command = command_of(&calls[2]);
        assert!(command.contains("umask 077"), "{command}");
        assert!(
            command.contains(&format!(
                "{HOME}/.claude/projects/-home-remote-work/{id}.jsonl"
            )),
            "{command}"
        );
        let jsonl =
            String::from_utf8(input_of(&calls[2]).expect("ship feeds stdin").to_vec()).unwrap();
        assert!(command.contains(&jsonl.len().to_string()), "{command}");
        let last = jsonl.lines().last().unwrap();
        let line: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(line["sessionId"], id.as_str());
        let command = command_of(&calls[3]);
        assert!(command.contains("tmux new-session"), "{command}");
        assert!(command.contains("remain-on-exit failed"), "{command}");
        assert!(command.contains(&id), "{command}");
        assert!(command.contains("/usr/local/bin/claude"), "{command}");
        assert!(input_of(&calls[3]).is_none());
        assert_attaches(&calls[4]);
    }

    #[test]
    fn file_present_launches_and_attaches_without_shipping() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "no", "no", "yes");
        fake.reply(0, ""); // launch
        run(&fake, false).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 4);
        let command = command_of(&calls[2]);
        assert!(command.contains("tmux new-session"), "{command}");
        assert_attaches(&calls[3]);
    }

    #[test]
    fn live_session_attaches_without_shipping_or_launching() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "yes", "no", "yes");
        run(&fake, false).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_attaches(&calls[2]);
    }

    #[test]
    fn dead_session_is_killed_before_the_launch() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "yes", "yes", "yes");
        fake.reply(0, ""); // kill
        fake.reply(0, ""); // launch
        run(&fake, false).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 5);
        let id = session_id();
        let command = command_of(&calls[2]);
        assert!(command.contains("kill-session"), "{command}");
        assert!(
            command.contains(&format!("=path-{}", &id[..8])),
            "{command}"
        );
        let command = command_of(&calls[3]);
        assert!(command.contains("tmux new-session"), "{command}");
        assert_attaches(&calls[4]);
    }

    #[test]
    fn a_failed_ship_stops_before_launch_and_attach() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "no", "no", "no");
        fake.reply_with_stderr(1, "", "disk full");
        let err = run(&fake, false).unwrap_err();
        assert!(err.to_string().contains("uploading the session"), "{err:#}");
        assert!(err.to_string().contains("disk full"), "{err:#}");
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn the_claude_path_is_quoted_for_the_shell_tmux_starts() {
        let fake = FakeSsh::new();
        fake.reply(
            0,
            &format!("TP_HOME={HOME}\nTP_CLAUDE=/opt/twi'lek/claude\nTP_TMUX=yes\n"),
        );
        reply_dir(&fake, DIR, "no", "no", "yes");
        fake.reply(0, ""); // launch
        run(&fake, false).unwrap();
        // The claude command is one positional parameter of the launch
        // script, so it appears quoted once for the inner shell and
        // once more for the outer.
        let inner = shlex::try_join([
            "env",
            "LANG=C.UTF-8",
            "/opt/twi'lek/claude",
            "-r",
            &session_id(),
        ])
        .unwrap();
        let outer = shlex::try_quote(&inner).unwrap();
        let calls = fake.calls();
        let command = command_of(&calls[2]);
        assert!(command.contains(outer.as_ref()), "{command}");
    }

    #[test]
    fn dry_run_stops_cleanly_for_each_action() {
        for (session, dead, target) in [
            ("no", "no", "no"),
            ("no", "no", "yes"),
            ("yes", "no", "yes"),
            ("yes", "yes", "yes"),
        ] {
            let fake = FakeSsh::new();
            reply_host_ok(&fake);
            reply_dir(&fake, DIR, session, dead, target);
            run(&fake, true).unwrap();
            assert_eq!(fake.calls().len(), 2);
        }
    }

    #[test]
    fn the_dir_probe_carries_the_dir_the_exact_tmux_name_and_the_session_file() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, DIR, "no", "no", "no");
        run(&fake, true).unwrap();
        let session_id = session_id();
        let calls = fake.calls();
        let command = command_of(&calls[1]);
        assert!(command.contains(DIR), "{command}");
        assert!(
            command.contains(&format!("path-{}", &session_id[..8])),
            "{command}"
        );
        assert!(
            command.contains(&format!(
                "{HOME}/.claude/projects/-home-remote-work/{session_id}.jsonl"
            )),
            "{command}"
        );
    }

    #[test]
    fn an_invalid_c_flag_errors_before_any_remote_call() {
        let fake = FakeSsh::new();
        let err = run_with_dir(&fake, true, Path::new("relative/dir")).unwrap_err();
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
        reply_dir(&fake, "", "no", "no", "no");
        let err = run(&fake, true).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err:#}");
        assert!(err.to_string().contains(DIR), "{err:#}");
    }

    #[test]
    fn non_physical_dir_errors_with_the_c_hint() {
        let fake = FakeSsh::new();
        reply_host_ok(&fake);
        reply_dir(&fake, "/private/home/remote/work", "no", "no", "no");
        let err = run(&fake, true).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("not physical"), "{err:#}");
        assert!(text.contains("-C /private/home/remote/work"), "{err:#}");
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

    #[test]
    fn tmux_name_is_path_plus_the_first_8_of_the_id() {
        assert_eq!(
            format_tmux_session_name("b7e1c0de-0000-4000-8000-000000000001"),
            "path-b7e1c0de"
        );
    }
}
