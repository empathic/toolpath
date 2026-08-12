//! `path p incept <provider>` — file/stdin-shaped sibling of `path
//! p export <provider>`. Forwards to [`crate::cmd_export::run`] after
//! resolving the stdin/file/cache-id input.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum InceptTarget {
    /// Incept a Toolpath document into a Claude Code session layout.
    Claude {
        /// Input toolpath document (JSON path, cache id, or `-` for
        /// stdin). Reads stdin when omitted.
        #[arg(short, long)]
        input: Option<String>,

        /// Target project directory. Defaults to the current
        /// directory when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with
        /// --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Incept a Toolpath document into a Cursor (IDE) composer in
    /// `state.vscdb`.
    Cursor {
        /// Input toolpath document (JSON path, cache id, or `-` for
        /// stdin). Reads stdin when omitted.
        #[arg(short, long)]
        input: Option<String>,

        /// Target workspace folder. Defaults to the current
        /// directory when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `CursorSession` as pretty JSON to
        /// this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
}

pub fn run(target: InceptTarget) -> Result<()> {
    match target {
        InceptTarget::Claude {
            input,
            project,
            output,
        } => {
            let input = resolve_input(input)?;
            let (project, output) = default_project(project, output);
            crate::cmd_export::run(crate::cmd_export::ExportTarget::Claude {
                input,
                project,
                output,
                force: false,
            })
        }
        InceptTarget::Cursor {
            input,
            project,
            output,
        } => {
            let input = resolve_input(input)?;
            let (project, output) = default_project(project, output);
            crate::cmd_export::run(crate::cmd_export::ExportTarget::Cursor {
                input,
                project,
                output,
            })
        }
    }
}

fn resolve_input(input: Option<String>) -> Result<String> {
    match input.as_deref() {
        None | Some("-") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("Failed to read stdin: {}", e))?;
            let mut f = tempfile::NamedTempFile::new()?;
            std::io::Write::write_all(&mut f, buf.as_bytes())?;
            let (_file, path) = f.keep()?;
            Ok(path.to_string_lossy().to_string())
        }
        Some(s) => Ok(s.to_string()),
    }
}

fn default_project(
    project: Option<PathBuf>,
    output: Option<PathBuf>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    if output.is_none() && project.is_none() {
        let cwd = std::env::current_dir().expect("cwd");
        (Some(cwd), None)
    } else {
        (project, output)
    }
}
