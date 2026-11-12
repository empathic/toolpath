//! `path d` (alias `path develop`): the tools for working on toolpath itself, built only with the
//! `develop` feature. `p` is plumbing, `d` is develop.
//!
//! * `path d baseline …`: capture, check and diff the baseline, the versioned set of snapshots
//!   each recording produces at every stage ([`crate::goldens`]). `goldens` is a hidden alias
//!   while scripts and docs written before the rename are still in flight.
//! * `path d webmock …`: the record/replay server for the agents' model APIs, and
//!   `webmock run <agent> record|replay`, which points one agent at it.
//! * `path d model …`: regenerate the goldens object model (`docs/model/model.json`).
//!
//! `webmock` and `model` live in unpublished workspace crates (`toolpath-webmock`,
//! `toolpath-model`), so these run their binaries rather than linking them: path-cli is published
//! and cannot depend on a crate crates.io does not have.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, anyhow, bail};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum DevelopCommand {
    /// Capture, check and diff the baseline: every recording's snapshots, stage by stage
    #[command(name = "baseline", alias = "goldens")]
    Goldens {
        #[command(flatten)]
        args: crate::goldens::GoldensArgs,
    },
    /// The record/replay server: `run <agent> record|replay …` or `serve` (its own options: `path d webmock -- --help`)
    Webmock {
        /// Arguments for the `webmock` binary (toolpath-webmock)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The agent CLIs tests and replay run: fetched only as agents.lock names them, hash-verified, cached by hash, never on PATH
    Agents {
        #[command(flatten)]
        args: crate::agents::AgentsArgs,
    },
    /// Regenerate the goldens object model, docs/model/model.json (its own options: `path d model -- --help`)
    Model {
        /// Arguments for the `toolpath-model` binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

pub fn run(cmd: DevelopCommand) -> Result<()> {
    match cmd {
        DevelopCommand::Goldens { args } => crate::goldens::run(args),
        DevelopCommand::Agents { args } => crate::agents::run(args),
        DevelopCommand::Webmock { args } => exec(
            &tool_bin("webmock", "WEBMOCK_BIN", "toolpath-webmock")?,
            &args,
        ),
        DevelopCommand::Model { args } => exec(
            &tool_bin("toolpath-model", "TOOLPATH_MODEL_BIN", "toolpath-model")?,
            &args,
        ),
    }
}

fn exec(bin: &PathBuf, args: &[String]) -> Result<()> {
    let status = Command::new(bin).args(args).status()?;
    if !status.success() {
        bail!("{} exited with {status}", bin.display());
    }
    Ok(())
}

/// `$<env>`, else `<name>` beside this executable (a workspace build puts them in the same
/// target directory), else `<name>` on `PATH`.
fn tool_bin(name: &str, env: &str, package: &str) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(env).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file())
    {
        return Ok(p);
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        })
        .ok_or_else(|| {
            anyhow!(
                "no `{name}` binary: build it with `cargo build -p {package}` (it lands beside \
                 `path`), put it on PATH, or set {env}"
            )
        })
}
