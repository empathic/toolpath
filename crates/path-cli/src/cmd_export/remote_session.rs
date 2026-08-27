//! `p export claude --derive-session-id` and `--cwd`: the session's ID
//! and cwd on the host that resumes it.

use anyhow::Result;

/// The `p export claude` flags that rewrite the projected session
/// before it is written.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote session")]
pub struct RemoteSessionArgs {
    /// Rename the session to an ID derived from the document: a
    /// v4-shaped UUID from the first 128 bits of the SHA-256 of its
    /// RFC 8785 (JCS) form. The same document yields the same ID
    /// on every run, so a second export of it into the same project
    /// is refused instead of duplicated. --cwd does not change the
    /// ID.
    #[arg(long)]
    pub(super) derive_session_id: bool,

    /// Root the session at this directory: it becomes the `cwd` of
    /// every line that carries one. Absolute POSIX path in
    /// normalized form; it does not have to exist on this machine.
    /// Mutually exclusive with --project.
    // `--project` files the session under the slug of the project
    // directory, and Claude Code reads every entry's `cwd` as that
    // directory. A second directory value can only repeat it or
    // contradict it.
    #[arg(long, value_name = "DIR", conflicts_with = "project", value_parser = parse_cwd_arg)]
    pub(super) cwd: Option<String>,
}

/// Claude Code keys a session on the exact `cwd` string, so the value
/// must be an absolute POSIX path in normalized form: no `.`, `..`, or
/// empty component. One trailing `/` is dropped. The directory may be
/// on another machine, so it is not required to exist.
fn parse_cwd_arg(raw: &str) -> Result<String> {
    let Some(rest) = raw.strip_prefix('/') else {
        anyhow::bail!("--cwd must be an absolute POSIX path (got {raw:?})");
    };
    if rest.is_empty() {
        return Ok("/".to_string());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        anyhow::bail!("--cwd must not contain an empty, `.`, or `..` component (got {raw:?})");
    }
    Ok(format!("/{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_flag_rejects_unnormalized_paths() {
        for bad in ["relative/dir", "/a/../b", "/a/./b", "/a//b", "//", ""] {
            assert!(parse_cwd_arg(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_cwd_arg("/a/b/").unwrap(), "/a/b");
        assert_eq!(parse_cwd_arg("/").unwrap(), "/");
    }
}
