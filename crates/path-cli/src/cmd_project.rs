//! Deprecation shim for `path project`.
//!
//! `path project claude --input X [--output Y]` became `path export claude
//! --input X [--output Y]`. This shim preserves the old surface for one
//! release.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ProjectTarget {
    /// Project a toolpath document into Claude JSONL format
    Claude {
        /// Input toolpath document (JSON path, or cache id)
        #[arg(short, long)]
        input: String,

        /// Output file (JSONL). Prints to stdout if omitted.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

pub fn run(target: ProjectTarget) -> Result<()> {
    eprintln!("warning: `path project` is deprecated; use `path export` instead");
    match target {
        ProjectTarget::Claude { input, output } => {
            crate::cmd_export::run(crate::cmd_export::ExportTarget::Claude {
                input,
                project: None,
                output,
            })
        }
    }
}
