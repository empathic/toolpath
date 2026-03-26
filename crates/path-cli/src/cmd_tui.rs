use std::path::PathBuf;

use anyhow::{Context, Result};
use toolpath::v1::{Graph, Path, PathOrRef};
use toolpath_tui::{TuiConfig, TuiMode};

pub fn run_view(input: Option<PathBuf>) -> Result<()> {
    run_tui(input, TuiMode::View)
}

pub fn run_redact(input: Option<PathBuf>) -> Result<()> {
    run_tui(input, TuiMode::Redact)
}

fn run_tui(input: Option<PathBuf>, mode: TuiMode) -> Result<()> {
    let graph = if let Some(path) = &input {
        crate::io::read_document_auto(path)?
    } else {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("Failed to read from stdin")?;
        Graph::from_json(&buf).context("Failed to parse Toolpath document")?
    };

    let path = single_path(graph)?;

    let config = TuiConfig {
        app_name: "path".to_string(),
    };

    if let Some(json) = toolpath_tui::run(path, mode, config)? {
        println!("{json}");
    }
    Ok(())
}

/// Extract the single inline path from a graph. view/redact operate on one
/// path at a time, so anything other than exactly one inline path is an error.
fn single_path(graph: Graph) -> Result<Path> {
    let mut paths = graph.paths.into_iter();
    match (paths.next(), paths.next()) {
        (Some(PathOrRef::Path(p)), None) => Ok(*p),
        (Some(PathOrRef::Ref(_)), None) => {
            anyhow::bail!(
                "view/redact requires an inline Path, but the document is an external $ref"
            )
        }
        (None, _) => anyhow::bail!("document contains no paths"),
        (Some(_), Some(_)) => {
            anyhow::bail!("view/redact requires a single Path; this document contains multiple")
        }
    }
}
