//! `p export claude --content-addressed-session-id` and `--cwd`: the session's ID
//! and cwd on the host that resumes it.

use crate::claude_session::parse_posix_dir;

/// The `p export claude` flags that rewrite the projected session
/// before it is written.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote session")]
pub struct RemoteSessionArgs {
    /// Rename the session to a content-addressed ID: a v4-shaped UUID
    /// from the first 128 bits of the SHA-256 of the document's
    /// RFC 8785 (JCS) form. The same document yields the same ID
    /// on every run, so a second export of it into the same project
    /// is refused instead of duplicated. --cwd does not change the
    /// ID. Mutually exclusive with --session-id and --new-session-id.
    #[arg(long, conflicts_with_all = ["session_id", "new_session_id"])]
    pub(super) content_addressed_session_id: bool,

    /// Root the session at this directory: it becomes the `cwd` of
    /// every line that carries one. Absolute POSIX path in
    /// normalized form; it does not have to exist on this machine.
    /// Mutually exclusive with --project.
    // `--project` files the session under the slug of the project
    // directory, and Claude Code reads every entry's `cwd` as that
    // directory. A second directory value can only repeat it or
    // contradict it.
    #[arg(long, value_name = "DIR", conflicts_with = "project", value_parser = parse_posix_dir)]
    pub(super) cwd: Option<String>,
}
