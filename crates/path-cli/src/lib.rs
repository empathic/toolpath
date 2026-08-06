pub mod artifact;
mod cache;
#[cfg(not(target_os = "emscripten"))]
mod cmd_auth;
mod cmd_cache;
mod cmd_derive;
mod cmd_export;
mod cmd_haiku;
mod cmd_import;
mod cmd_incept;
mod cmd_kind;
mod cmd_list;
mod cmd_merge;
mod cmd_p;
mod cmd_p_query;
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
mod derive;
#[cfg(not(target_os = "emscripten"))]
mod fuzzy;
#[cfg(not(target_os = "emscripten"))]
pub mod harness;
mod io;
pub mod kinds;
mod query;
mod schema;
#[cfg(all(not(target_os = "emscripten"), feature = "embedded-picker"))]
mod skim_picker;
mod sync;
mod term;

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

    /// Backend for the interactive fuzzy picker used by `share`,
    /// `resume`, and `p import <provider>`. `auto` (default) uses the
    /// embedded skim picker and falls back to external `fzf` only when
    /// skim isn't compiled in; a hint is printed if `fzf` is also on
    /// PATH. `fzf`/`skim` force one backend and error if it isn't
    /// available.
    #[cfg(not(target_os = "emscripten"))]
    #[arg(long, global = true, value_enum, default_value_t = fuzzy::Picker::Auto)]
    picker: fuzzy::Picker,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show a single session as a markdown summary (used by fzf preview)
    #[cfg(not(target_os = "emscripten"))]
    Show {
        #[command(subcommand)]
        source: cmd_show::ShowSource,
        /// Emit ANSI-styled terminal output instead of raw markdown
        /// (bold speakers, dim metadata, colored diffs). Used by the
        /// fzf preview panes.
        #[arg(long)]
        ansi: bool,
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
    /// Query the local cache: load every step into one JSON array and
    /// transform it with a jaq (jq) filter
    Query {
        #[command(flatten)]
        args: cmd_query::QueryArgs,
    },
    /// List bundled document kinds, or print a kind's schema (the field
    /// reference for `path query`)
    Kind {
        #[command(flatten)]
        args: cmd_kind::KindArgs,
    },
    /// Manage Pathbase credentials for trace uploads
    #[cfg(not(target_os = "emscripten"))]
    Auth {
        #[command(subcommand)]
        op: cmd_auth::AuthOp,
    },
    /// Plumbing: lower-level operations on documents and sources
    /// (import, export, cache, list, render, merge, validate, derive,
    /// project, incept, track, query)
    P {
        #[command(subcommand)]
        command: cmd_p::PCommand,
    },
    /// Print a random Toolpath haiku
    Haiku,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    #[cfg(not(target_os = "emscripten"))]
    fuzzy::set_picker_override(cli.picker);

    match cli.command {
        Commands::Haiku => {
            cmd_haiku::run();
            Ok(())
        }
        #[cfg(not(target_os = "emscripten"))]
        Commands::Show { source, ansi } => cmd_show::run(source, ansi),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Share { args } => cmd_share::run(args),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Resume { args } => cmd_resume::run(args),
        Commands::Query { args } => cmd_query::run(args, cli.pretty),
        Commands::Kind { args } => cmd_kind::run(args),
        #[cfg(not(target_os = "emscripten"))]
        Commands::Auth { op } => cmd_auth::run(op),
        Commands::P { command } => cmd_p::run(command, cli.pretty),
    }
}
