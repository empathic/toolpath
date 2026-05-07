//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::cmd_export::RepoSpec;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum HarnessArg {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

#[derive(Args, Debug)]
pub struct ShareArgs {
    /// Pathbase server URL (defaults to the stored session's server)
    #[arg(long)]
    pub url: Option<String>,

    /// Force the anonymous endpoint, ignoring any stored credentials
    #[arg(long, conflicts_with_all = ["repo", "public"])]
    pub anon: bool,

    /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
    #[arg(long, value_parser = crate::cmd_export::parse_repo_spec)]
    pub repo: Option<RepoSpec>,

    /// Override the auto-derived slug (defaults to the toolpath document id)
    #[arg(long)]
    pub slug: Option<String>,

    /// Make the uploaded path publicly listable (default: secret/unlisted)
    #[arg(long)]
    pub public: bool,

    /// Narrow the picker to one harness, or skip the picker entirely
    /// when used with --session.
    #[arg(long, value_enum)]
    pub harness: Option<HarnessArg>,

    /// Skip the picker. Requires --harness; requires --project for
    /// claude/gemini/pi.
    #[arg(long, requires = "harness")]
    pub session: Option<String>,

    /// Override cwd-as-project. Filters the picker to sessions tied to
    /// this project across all harnesses.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Overwrite the cache entry if it already exists
    #[arg(long)]
    pub force: bool,

    /// Skip writing the cache; derive in-memory only
    #[arg(long)]
    pub no_cache: bool,
}

pub fn run(args: ShareArgs) -> Result<()> {
    let _ = args;
    anyhow::bail!("`path share` is not yet implemented")
}
