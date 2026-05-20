//! Plumbing subcommand (`path p …`).
//!
//! Groups the lower-level building blocks — import, export, cache, list,
//! render, merge, validate — under one namespace so the top-level
//! `path --help` lists only the porcelain (share, resume, query, show,
//! track, auth). The handlers themselves live in the per-command modules
//! (`cmd_import`, `cmd_export`, …); this module just defines the clap
//! enum and dispatches.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum PCommand {
    /// List available sources (branches, projects, sessions)
    List {
        #[command(subcommand)]
        source: crate::cmd_list::ListSource,

        /// Output format (default: pretty when stdout is a TTY, tsv when piped)
        #[arg(long, global = true, value_enum)]
        format: Option<crate::cmd_list::ListFormat>,

        /// Output as JSON (deprecated alias for --format json)
        #[arg(long, global = true)]
        json: bool,
    },
    /// Import from external formats into the toolpath cache
    Import {
        #[command(flatten)]
        args: crate::cmd_import::ImportArgs,
    },
    /// Export toolpath documents into external formats
    Export {
        #[command(subcommand)]
        target: crate::cmd_export::ExportTarget,
    },
    /// Manage the on-disk document cache (~/.toolpath/documents/)
    Cache {
        #[command(subcommand)]
        op: crate::cmd_cache::CacheOp,
    },
    /// Render Toolpath documents to other formats
    Render {
        #[command(subcommand)]
        format: crate::cmd_render::RenderFormat,
    },
    /// Merge multiple Toolpath documents into a single Graph
    Merge {
        /// Input files (use - for stdin)
        #[arg(required = true)]
        inputs: Vec<String>,

        /// Title for the merged graph
        #[arg(long)]
        title: Option<String>,
    },
    /// Validate a Toolpath document
    Validate {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,
    },
    /// Derive a Toolpath document from a source and stream it to stdout
    /// (the stdout-only sibling of `import`; equivalent to `import …
    /// --no-cache`).
    Derive {
        #[command(subcommand)]
        source: crate::cmd_derive::DeriveSource,
    },
    /// Project a Toolpath document into a harness's session format
    /// (file or stdout — the narrower, file-shaped sibling of `export`).
    Project {
        #[command(subcommand)]
        target: crate::cmd_project::ProjectTarget,
    },
    /// Project a Toolpath document into a Claude session layout under
    /// `--project` (file/stdin-shaped sibling of `export claude
    /// --project`).
    Incept {
        #[command(flatten)]
        args: crate::cmd_incept::InceptArgs,
    },
    /// Incrementally build a Toolpath Path document
    Track {
        #[command(subcommand)]
        op: crate::cmd_track::TrackOp,
    },
}

pub fn run(command: PCommand, pretty: bool) -> Result<()> {
    match command {
        PCommand::List {
            source,
            format,
            json,
        } => crate::cmd_list::run(source, format, json),
        PCommand::Import { args } => crate::cmd_import::run(args, pretty),
        PCommand::Export { target } => crate::cmd_export::run(target),
        PCommand::Cache { op } => crate::cmd_cache::run(op),
        PCommand::Render { format } => crate::cmd_render::run(format),
        PCommand::Merge { inputs, title } => crate::cmd_merge::run(inputs, title, pretty),
        PCommand::Validate { input } => crate::cmd_validate::run(input),
        PCommand::Derive { source } => crate::cmd_derive::run(source, pretty),
        PCommand::Project { target } => crate::cmd_project::run(target),
        PCommand::Incept { args } => crate::cmd_incept::run(args),
        PCommand::Track { op } => crate::cmd_track::run(op, pretty),
    }
}
