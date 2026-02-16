mod cmd_derive;
mod cmd_haiku;
mod cmd_list;
mod cmd_merge;
mod cmd_query;
mod cmd_render;
mod cmd_track;
mod cmd_validate;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "path")]
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

        /// Output as JSON
        #[arg(long, global = true)]
        json: bool,
    },
    /// Derive Toolpath documents from source systems
    Derive {
        #[command(subcommand)]
        source: cmd_derive::DeriveSource,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List { source, json } => cmd_list::run(source, json),
        Commands::Derive { source } => cmd_derive::run(source, cli.pretty),
        Commands::Query { op } => cmd_query::run(op, cli.pretty),
        Commands::Render { format } => cmd_render::run(format),
        Commands::Merge { inputs, title } => cmd_merge::run(inputs, title, cli.pretty),
        Commands::Track { op } => cmd_track::run(op, cli.pretty),
        Commands::Validate { input } => cmd_validate::run(input),
        Commands::Haiku => {
            cmd_haiku::run();
            Ok(())
        }
    }
}
