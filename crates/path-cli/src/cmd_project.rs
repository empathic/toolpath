//! `path p project` — narrower, file-shaped sibling of `export`.
//!
//! `project claude --input X [--output Y]` writes a toolpath document
//! as a Claude JSONL session, either to a file (`--output`) or to
//! stdout. It's the older, simpler surface; `export claude` is its
//! superset and also accepts `--project <dir>` to overwrite an
//! on-disk session layout.

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
    match target {
        ProjectTarget::Claude { input, output } => {
            crate::cmd_export::run(crate::cmd_export::ExportTarget::Claude {
                input,
                project: None,
                output,
                force: false,
                #[cfg(all(feature = "resume-remote", not(target_os = "emscripten")))]
                remote: crate::cmd_export::RemoteSessionArgs::default(),
            })
        }
    }
}
