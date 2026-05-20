//! `path p incept` — file/stdin-shaped sibling of `export claude`.
//!
//! `incept --input X --project Y` writes a toolpath document into a
//! Claude session layout under `<project>`. `--input` defaults to
//! stdin so `path p import claude … --no-cache | path p incept
//! --project /tmp/sandbox` composes cleanly; `--project` defaults to
//! the current directory. It's the older surface kept for shell
//! pipelines that prefer that ergonomic over `export claude --input X
//! --project Y`.

use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct InceptArgs {
    /// Input toolpath document (JSON path, or cache id). Reads stdin when omitted.
    #[arg(short, long)]
    input: Option<String>,

    /// Target project directory. Defaults to the current directory.
    #[arg(short, long)]
    project: Option<PathBuf>,
}

pub fn run(args: InceptArgs) -> Result<()> {
    let input = match args.input {
        Some(s) => s,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("Failed to read stdin: {}", e))?;
            let mut f = tempfile::NamedTempFile::new()?;
            std::io::Write::write_all(&mut f, buf.as_bytes())?;
            let (_file, path) = f.keep()?;
            path.to_string_lossy().to_string()
        }
    };

    let project = args
        .project
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    crate::cmd_export::run(crate::cmd_export::ExportTarget::Claude {
        input,
        project: Some(project),
        output: None,
    })
}
