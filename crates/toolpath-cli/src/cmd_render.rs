use crate::io::{self as cli_io, InputSpec, OutputSpec};
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum RenderFormat {
    /// Render as Graphviz DOT
    Dot {
        /// Input file (use `-` or omit to read from stdin)
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Output file (use `-` or omit to write to stdout)
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
    /// Render as Markdown (for LLM consumption)
    Md {
        /// Input file (use `-` or omit to read from stdin)
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Output file (use `-` or omit to write to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Detail level: summary (file-level diffstats) or full (inline diffs)
        #[arg(long, default_value = "summary")]
        detail: String,

        /// Include YAML front matter with machine-readable metadata
        #[arg(long)]
        front_matter: bool,
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
        RenderFormat::Md {
            input,
            output,
            detail,
            front_matter,
        } => run_md(input, output, &detail, front_matter),
    }
}

fn run_dot(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    show_files: bool,
    show_timestamps: bool,
    highlight_dead_ends: bool,
) -> Result<()> {
    let doc = cli_io::read_document(&InputSpec::from_opt(input))?;

    let options = toolpath_dot::RenderOptions {
        show_files,
        show_timestamps,
        highlight_dead_ends,
    };

    let dot = toolpath_dot::render(&doc, &options);
    OutputSpec::from_opt(output).write_str(&dot)
}

fn run_md(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    detail: &str,
    front_matter: bool,
) -> Result<()> {
    let doc = cli_io::read_document(&InputSpec::from_opt(input))?;

    let detail = match detail {
        "full" => toolpath_md::Detail::Full,
        _ => toolpath_md::Detail::Summary,
    };

    let options = toolpath_md::RenderOptions {
        detail,
        front_matter,
    };

    let md = toolpath_md::render(&doc, &options);
    OutputSpec::from_opt(output).write_str(&md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use toolpath::v1::{Document, Path, PathIdentity, Step};

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

    // ── run_md ───────────────────────────────────────────────────────────

    #[test]
    fn test_run_md_with_input_file() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let result = run_md(Some(f.path().to_path_buf()), None, "summary", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_md_with_output_file() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = run_md(
            Some(f.path().to_path_buf()),
            Some(out.path().to_path_buf()),
            "summary",
            false,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).unwrap();
        assert!(content.contains("# p1"));
        assert!(content.contains("## Timeline"));
    }

    #[test]
    fn test_run_md_full_detail() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = run_md(
            Some(f.path().to_path_buf()),
            Some(out.path().to_path_buf()),
            "full",
            false,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).unwrap();
        assert!(content.contains("```diff"));
    }

    #[test]
    fn test_run_md_with_front_matter() {
        let doc = make_doc();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();

        let out = tempfile::NamedTempFile::new().unwrap();
        let result = run_md(
            Some(f.path().to_path_buf()),
            Some(out.path().to_path_buf()),
            "summary",
            true,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).unwrap();
        assert!(content.starts_with("---\n"));
        assert!(content.contains("type: path"));
    }

    #[test]
    fn test_run_md_invalid_input() {
        let result = run_md(Some(PathBuf::from("/nonexistent")), None, "summary", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_run_md_invalid_json() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "not valid json").unwrap();
        f.flush().unwrap();

        let result = run_md(Some(f.path().to_path_buf()), None, "summary", false);
        assert!(result.is_err());
    }
}
