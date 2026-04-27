#[cfg(not(target_os = "emscripten"))]
mod cmd_auth;
mod cmd_cache;
mod cmd_derive;
mod cmd_export;
mod cmd_haiku;
mod cmd_import;
mod cmd_incept;
mod cmd_list;
mod cmd_merge;
#[cfg(not(target_os = "emscripten"))]
mod cmd_pathbase;
mod cmd_project;
mod cmd_query;
mod cmd_render;
#[cfg(not(target_os = "emscripten"))]
mod cmd_show;
mod cmd_track;
mod cmd_validate;
mod config;
#[cfg(not(target_os = "emscripten"))]
mod fzf;
mod io;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "path", version)]
#[command(about = "Derive, query, and visualize Toolpath provenance documents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pretty: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List available sources (branches, projects, sessions)
    List {
        #[command(subcommand)]
        source: cmd_list::ListSource,

        /// Output format (default: pretty when stdout is a TTY, tsv when piped)
        #[arg(long, global = true, value_enum)]
        format: Option<cmd_list::ListFormat>,

        /// Output as JSON (deprecated alias for --format json)
        #[arg(long, global = true)]
        json: bool,
    },
    /// Import from external formats into the toolpath cache
    Import {
        #[command(flatten)]
        args: cmd_import::ImportArgs,
    },
    /// Export toolpath documents into external formats
    Export {
        #[command(subcommand)]
        target: cmd_export::ExportTarget,
    },
    /// Manage the on-disk document cache (~/.toolpath/documents/)
    Cache {
        #[command(subcommand)]
        op: cmd_cache::CacheOp,
    },
    /// Query Toolpath documents
    Query {
        #[command(subcommand)]
        op: cmd_query::QueryOp,
    },
    /// Render Toolpath documents to other formats
    Render {
        #[command(subcommand)]
        format: cmd_render::RenderFormat,
    },
    /// Show a single session as a markdown summary (used by fzf preview)
    #[cfg(not(target_os = "emscripten"))]
    Show {
        #[command(subcommand)]
        source: cmd_show::ShowSource,
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
    /// Incrementally build a Toolpath Path document
    Track {
        #[command(subcommand)]
        op: cmd_track::TrackOp,
    },
    /// Validate a Toolpath document
    Validate {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,
    },
    /// Print a random Toolpath haiku
    Haiku,
    /// Manage Pathbase credentials for trace uploads
    #[cfg(not(target_os = "emscripten"))]
    Auth {
        #[command(subcommand)]
        op: cmd_auth::AuthOp,
    },

    // ── Deprecated aliases ────────────────────────────────────────────
    #[command(hide = true, about = "[deprecated] Use `path import`")]
    Derive {
        #[command(subcommand)]
        source: cmd_derive::DeriveSource,
    },
    #[command(hide = true, about = "[deprecated] Use `path export claude`")]
    Incept {
        #[command(flatten)]
        args: cmd_incept::InceptArgs,
    },
    #[command(hide = true, about = "[deprecated] Use `path export`")]
    Project {
        #[command(subcommand)]
        target: cmd_project::ProjectTarget,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List {
            source,
            format,
            json,
        } => cmd_list::run(source, format, json),
        Commands::Import { args } => cmd_import::run(args, cli.pretty),
        Commands::Export { target } => cmd_export::run(target),
        Commands::Cache { op } => cmd_cache::run(op),
        Commands::Query { op } => cmd_query::run(op, cli.pretty),
        Commands::Render { format } => cmd_render::run(format),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Show { source } => cmd_show::run(source),
        Commands::Merge { inputs, title } => cmd_merge::run(inputs, title, cli.pretty),
        Commands::Track { op } => cmd_track::run(op, cli.pretty),
        Commands::Validate { input } => cmd_validate::run(input),
        Commands::Haiku => {
            cmd_haiku::run();
            Ok(())
        }
        #[cfg(not(target_os = "emscripten"))]
        Commands::Auth { op } => cmd_auth::run(op),

        Commands::Derive { source } => cmd_derive::run(source, cli.pretty),
        Commands::Incept { args } => cmd_incept::run(args),
        Commands::Project { target } => cmd_project::run(target),
    }
}
