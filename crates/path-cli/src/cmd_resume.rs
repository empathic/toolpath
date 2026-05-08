//! `path resume` — fetch / load a Toolpath document and exec a coding
//! agent's resume command after projecting the session into the
//! harness's on-disk layout.
//!
//! See `docs/superpowers/specs/2026-05-08-path-resume-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

#[allow(unused_imports)]
use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

use crate::cmd_share::HarnessArg;

#[derive(Args, Debug)]
pub struct ResumeArgs {
    /// Toolpath document to resume from. Accepted shapes: a Pathbase
    /// URL (`https://host/owner/repo/slug`), a bare Pathbase shorthand
    /// (`owner/repo/slug`), a path to a local toolpath JSON file, or a
    /// cache id (e.g. `claude-abc`, `pathbase-foo-bar-baz`).
    pub input: String,

    /// Working directory to run the resumed harness from. Defaults to
    /// the current shell cwd. The on-disk projection is keyed on this
    /// directory and the harness will be exec'd with cwd set to it.
    #[arg(short = 'C', long)]
    pub cwd: Option<PathBuf>,

    /// Pin the resume target. Skips the interactive picker.
    #[arg(long, value_enum)]
    pub harness: Option<HarnessArg>,

    /// Skip writing the cache when fetching from Pathbase.
    #[arg(long)]
    pub no_cache: bool,

    /// Overwrite an existing cache entry when fetching from Pathbase.
    #[arg(long)]
    pub force: bool,

    /// Pathbase server URL. Falls back to the stored session's URL,
    /// then `$PATHBASE_URL`, then `https://pathbase.dev`.
    #[arg(long)]
    pub url: Option<String>,
}

pub fn run(_args: ResumeArgs) -> Result<()> {
    anyhow::bail!("path resume: not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_not_implemented_until_wired() {
        let args = ResumeArgs {
            input: "irrelevant".to_string(),
            cwd: None,
            harness: None,
            no_cache: false,
            force: false,
            url: None,
        };
        let err = run(args).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
