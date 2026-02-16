use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::Document;

#[derive(Subcommand, Debug)]
pub enum RenderFormat {
    /// Render as Graphviz DOT
    Dot {
        /// Input file (reads from stdin if not provided)
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Output file (writes to stdout if not provided)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Show file changes in step labels
        #[arg(long)]
        show_files: bool,

        /// Show timestamps in step labels
        #[arg(long)]
        show_timestamps: bool,

        /// Highlight dead ends in red
        #[arg(long, default_value = "true")]
        highlight_dead_ends: bool,
    },
}

pub fn run(format: RenderFormat) -> Result<()> {
    match format {
        RenderFormat::Dot {
            input,
            output,
            show_files,
            show_timestamps,
            highlight_dead_ends,
        } => run_dot(
            input,
            output,
            show_files,
            show_timestamps,
            highlight_dead_ends,
        ),
    }
}

fn run_dot(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    show_files: bool,
    show_timestamps: bool,
    highlight_dead_ends: bool,
) -> Result<()> {
    let content = if let Some(path) = &input {
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {:?}", path))?
    } else {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("Failed to read from stdin")?;
        buf
    };

    let doc = Document::from_json(&content).context("Failed to parse Toolpath document")?;

    let options = toolpath_dot::RenderOptions {
        show_files,
        show_timestamps,
        highlight_dead_ends,
    };

    let dot = toolpath_dot::render(&doc, &options);

    if let Some(path) = &output {
        std::fs::write(path, &dot).with_context(|| format!("Failed to write {:?}", path))?;
    } else {
        print!("{}", dot);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use toolpath::v1::{Path, PathIdentity, Step};

    fn make_doc() -> Document {
        let s1 =
            Step::new("s1", "human:alex", "2026-01-01T00:00:00Z").with_raw_change("f.rs", "@@");
        Document::Path(Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s1],
            meta: None,
        })
    }

    #[test]
    fn test_run_dot_with_input_file() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let result = run_dot(Some(f.path().to_path_buf()), None, false, false, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dot_with_output_file() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = run_dot(
            Some(f.path().to_path_buf()),
            Some(out.path().to_path_buf()),
            false,
            false,
            true,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).unwrap();
        assert!(content.contains("digraph"));
    }

    #[test]
    fn test_run_dot_with_options() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let result = run_dot(Some(f.path().to_path_buf()), None, true, true, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dot_invalid_input() {
        let result = run_dot(
            Some(PathBuf::from("/nonexistent")),
            None,
            false,
            false,
            true,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_run_dot_invalid_json() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "not valid json").unwrap();
        f.flush().unwrap();

        let result = run_dot(Some(f.path().to_path_buf()), None, false, false, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_run_dot_no_dead_ends() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let result = run_dot(Some(f.path().to_path_buf()), None, false, false, false);
        assert!(result.is_ok());
    }
}
