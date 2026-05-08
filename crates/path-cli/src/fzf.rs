//! Helpers for interactive session selection via `fzf`.
//!
//! The protocol:
//! - `path list <provider> --format tsv` emits one session per line, tab-delimited.
//!   For single-keyed providers (codex, opencode) col 1 is the session id.
//!   For project-keyed providers (claude, gemini, pi) col 1 is the project path
//!   and col 2 is the session id. Subsequent columns are display-only.
//! - `path show <provider>` emits a markdown summary for one session, suitable
//!   for fzf's `--preview`.
//! - `path derive <provider>` with no session arg auto-launches fzf when stdin
//!   and stderr are TTYs and `fzf` is on `PATH`. Multi-select produces a `Graph`.
//!
//! When fzf is unavailable, callers fall through to the documented manual recipe.
#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

/// Returns true when stdin and stderr are both attached to a TTY *and* `fzf`
/// is on `PATH`. `derive` only auto-launches when this is true.
pub fn available() -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    which("fzf").is_some()
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Outcome of an fzf invocation.
///
/// Distinguishes a deliberate user cancel (Esc / Ctrl-C, fzf exit 130) from
/// the no-match case (fzf exit 1). Callers that want to surface a non-zero
/// exit on cancel can match on `Cancelled`; callers that just want the picked
/// lines treat both `NoMatch` and `Cancelled` as "empty selection".
pub enum PickResult {
    /// fzf exited 0 with at least one selected line.
    Selected(Vec<String>),
    /// fzf exited 1: input was non-empty but nothing matched the query.
    NoMatch,
    /// fzf exited 130: the user pressed Esc / Ctrl-C / Ctrl-D.
    Cancelled,
}

/// Run fzf with the supplied lines on stdin. Returns a `PickResult` so the
/// caller can distinguish a successful selection from no-match vs. an
/// explicit user cancel (which some callers map to a non-zero exit).
pub fn pick(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    let mut args: Vec<String> = vec![
        "--delimiter=\t".into(),
        format!("--with-nth={}", opts.with_nth),
        format!("--prompt={}", opts.prompt),
        format!("--tiebreak={}", opts.tiebreak),
    ];

    if opts.multi {
        args.push("--multi".into());
    }

    if let Some(preview) = opts.preview {
        args.push(format!("--preview={}", preview));
        args.push("--preview-window=right:60%:wrap".into());
    }

    if let Some(header) = opts.header {
        args.push(format!("--header={}", header));
    }

    let mut child = Command::new("fzf")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn fzf")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Failed to open fzf stdin"))?;
        for line in lines {
            stdin.write_all(line.as_bytes())?;
            stdin.write_all(b"\n")?;
        }
    }

    let output = child.wait_with_output().context("Failed to wait on fzf")?;

    // fzf exit codes: 0 = match, 1 = no match, 2 = error, 130 = cancelled.
    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(PickResult::Selected(
                text.lines().map(|s| s.to_string()).collect(),
            ))
        }
        Some(1) => Ok(PickResult::NoMatch),
        Some(130) => Ok(PickResult::Cancelled),
        _ => anyhow::bail!("fzf exited with status {:?}", output.status),
    }
}

/// Options for an fzf invocation.
pub struct PickOptions<'a> {
    /// Visible columns in fzf's notation, e.g. `2..` to hide col 1.
    pub with_nth: &'a str,
    /// Prompt string shown to the left of the input.
    pub prompt: &'a str,
    /// Optional `--preview` command. Use `{1}`, `{2}` ... to substitute fields.
    pub preview: Option<&'a str>,
    /// Optional header line shown above the list.
    pub header: Option<&'a str>,
    /// Tiebreak ordering — `index` preserves input order.
    pub tiebreak: &'a str,
    /// Allow selecting multiple rows.
    pub multi: bool,
}

impl Default for PickOptions<'_> {
    fn default() -> Self {
        Self {
            with_nth: "2..",
            prompt: "> ",
            preview: None,
            header: None,
            tiebreak: "index",
            multi: false,
        }
    }
}

/// Print a manual recipe for users without fzf or piping non-interactively.
/// `provider` is the subcommand name; `project_keyed` toggles the column layout.
pub fn print_recipe(provider: &str, project_keyed: bool) {
    if project_keyed {
        eprintln!(
            "Interactive selection needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path list {provider} --format tsv \\\n    \
             | fzf --delimiter=$'\\t' --with-nth=3.. \\\n          \
             --preview 'path show {provider} --project {{1}} --session {{2}}' \\\n    \
             | awk -F'\\t' '{{print $1 \"\\n\" $2}}' \\\n    \
             | xargs -L2 sh -c 'path derive {provider} --project \"$1\" --session \"$2\"' --\n"
        );
    } else {
        eprintln!(
            "Interactive selection needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path list {provider} --format tsv \\\n    \
             | fzf --delimiter=$'\\t' --with-nth=2.. \\\n          \
             --preview 'path show {provider} --session {{1}}' \\\n    \
             | cut -f1 \\\n    \
             | xargs -I{{}} path derive {provider} --session {{}}\n"
        );
    }
}
