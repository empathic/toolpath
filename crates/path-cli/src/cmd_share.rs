//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
use chrono::{DateTime, Utc};
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

/// Which agent harness a session was produced by.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Harness {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

#[allow(dead_code)] // wired up by gather_sessions in a follow-up task
impl Harness {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Gemini => "gemini",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
        }
    }

    /// Padded so all five symbols line up in the fzf column.
    pub(crate) fn symbol(&self) -> &'static str {
        match self {
            Harness::Claude => "claude  ",
            Harness::Gemini => "gemini  ",
            Harness::Codex => "codex   ",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi      ",
        }
    }

    /// True when the underlying provider keys sessions by project path.
    /// claude/gemini/pi: true. codex/opencode: false (sessions store cwd
    /// per-row, not as a directory key).
    pub(crate) fn project_keyed(&self) -> bool {
        matches!(self, Harness::Claude | Harness::Gemini | Harness::Pi)
    }

    pub(crate) fn from_arg(arg: HarnessArg) -> Self {
        match arg {
            HarnessArg::Claude => Harness::Claude,
            HarnessArg::Gemini => Harness::Gemini,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Opencode => Harness::Opencode,
            HarnessArg::Pi => Harness::Pi,
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Harness::Claude),
            "gemini" => Some(Harness::Gemini),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::Opencode),
            "pi" => Some(Harness::Pi),
            _ => None,
        }
    }
}

/// One row in the unified session picker.
#[allow(dead_code)] // wired up by gather_sessions in a follow-up task
#[derive(Debug, Clone)]
pub(crate) struct SessionRow {
    pub(crate) harness: Harness,
    /// Project path for keyed providers; `None` for codex/opencode.
    pub(crate) project: Option<String>,
    /// Recorded cwd from the session (codex/opencode only).
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) last_activity: Option<DateTime<Utc>>,
    pub(crate) message_count: usize,
    pub(crate) matches_cwd: bool,
}

/// Bundle of provider managers used during aggregation. Production code
/// builds this from real `$HOME` via `from_environment`; tests construct
/// it directly with provider-specific resolvers.
#[allow(dead_code)] // wired up by gather_sessions in a follow-up task
#[derive(Default)]
pub(crate) struct HarnessBundle {
    pub(crate) claude: Option<toolpath_claude::ClaudeConvo>,
    pub(crate) gemini: Option<toolpath_gemini::GeminiConvo>,
    pub(crate) codex: Option<toolpath_codex::CodexConvo>,
    pub(crate) opencode: Option<toolpath_opencode::OpencodeConvo>,
    pub(crate) pi: Option<toolpath_pi::PiConvo>,
}

impl HarnessBundle {
    /// Build the production bundle. Each provider is included
    /// unconditionally (its `new()` doesn't fail on a missing home dir);
    /// `gather_sessions` skips the ones whose listing returns empty/NotFound.
    #[allow(dead_code)] // wired up by gather_sessions in a follow-up task
    pub(crate) fn from_environment() -> Self {
        Self {
            claude: Some(toolpath_claude::ClaudeConvo::new()),
            gemini: Some(toolpath_gemini::GeminiConvo::new()),
            codex: Some(toolpath_codex::CodexConvo::new()),
            opencode: Some(toolpath_opencode::OpencodeConvo::new()),
            pi: Some(toolpath_pi::PiConvo::new()),
        }
    }
}

pub fn run(args: ShareArgs) -> Result<()> {
    let _ = args;
    anyhow::bail!("`path share` is not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_name_and_symbol_are_distinct() {
        let all = [
            Harness::Claude,
            Harness::Gemini,
            Harness::Codex,
            Harness::Opencode,
            Harness::Pi,
        ];
        let names: Vec<&str> = all.iter().map(|h| h.name()).collect();
        let symbols: Vec<&str> = all.iter().map(|h| h.symbol()).collect();
        assert_eq!(names.len(), 5);
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            5,
            "names must be unique"
        );
        assert_eq!(
            symbols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5,
            "symbols must be unique"
        );
    }

    #[test]
    fn harness_project_keyed_matches_design() {
        assert!(Harness::Claude.project_keyed());
        assert!(Harness::Gemini.project_keyed());
        assert!(Harness::Pi.project_keyed());
        assert!(!Harness::Codex.project_keyed());
        assert!(!Harness::Opencode.project_keyed());
    }

    #[test]
    fn harness_from_arg_roundtrips() {
        for (arg, harness) in [
            (HarnessArg::Claude, Harness::Claude),
            (HarnessArg::Gemini, Harness::Gemini),
            (HarnessArg::Codex, Harness::Codex),
            (HarnessArg::Opencode, Harness::Opencode),
            (HarnessArg::Pi, Harness::Pi),
        ] {
            assert_eq!(Harness::from_arg(arg), harness);
        }
    }
}
