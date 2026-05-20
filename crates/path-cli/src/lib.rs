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
mod cmd_p;
#[cfg(not(target_os = "emscripten"))]
mod cmd_pathbase;
mod cmd_project;
mod cmd_query;
mod cmd_render;
#[cfg(not(target_os = "emscripten"))]
pub mod cmd_resume;
#[cfg(not(target_os = "emscripten"))]
mod cmd_share;
#[cfg(not(target_os = "emscripten"))]
mod cmd_show;
mod cmd_track;
mod cmd_validate;
mod config;
#[cfg(not(target_os = "emscripten"))]
mod fzf;
mod io;
mod schema;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Show a single session as a markdown summary (used by fzf preview)
    #[cfg(not(target_os = "emscripten"))]
    Show {
        #[command(subcommand)]
        source: cmd_show::ShowSource,
    },
    /// Share an agent session to Pathbase via an interactive picker
    #[cfg(not(target_os = "emscripten"))]
    Share {
        #[command(flatten)]
        args: cmd_share::ShareArgs,
    },
    /// Resume an agent session into the chosen harness, projecting the
    /// document and exec'ing the harness's resume command.
    #[cfg(not(target_os = "emscripten"))]
    Resume {
        #[command(flatten)]
        args: cmd_resume::ResumeArgs,
    },
    /// Query Toolpath documents
    Query {
        #[command(subcommand)]
        op: cmd_query::QueryOp,
    },
    /// Manage Pathbase credentials for trace uploads
    #[cfg(not(target_os = "emscripten"))]
    Auth {
        #[command(subcommand)]
        op: cmd_auth::AuthOp,
    },
    /// Plumbing: lower-level operations on documents and sources
    /// (import, export, cache, list, render, merge, validate, derive,
    /// project, incept, track)
    P {
        #[command(subcommand)]
        command: cmd_p::PCommand,
    },
    /// Print a random Toolpath haiku
    Haiku,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Haiku => {
            cmd_haiku::run();
            Ok(())
        }
        #[cfg(not(target_os = "emscripten"))]
        Commands::Show { source } => cmd_show::run(source),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Share { args } => cmd_share::run(args),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Resume { args } => cmd_resume::run(args),
        Commands::Query { op } => cmd_query::run(op, cli.pretty),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Auth { op } => cmd_auth::run(op),
        Commands::P { command } => cmd_p::run(command, cli.pretty),
    }
}
