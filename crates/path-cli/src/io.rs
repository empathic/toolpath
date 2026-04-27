//! Input/output helpers shared across CLI subcommands.
//!
//! Extension-based format routing: `*.path.jsonl` files are parsed through
//! the JSONL streaming reader; everything else is parsed as canonical JSON.

use anyhow::{Context, Result};
use std::io::BufReader;
use std::path::Path as FsPath;
use toolpath::v1::{Document, Path};

/// Read a Toolpath document from a file, auto-detecting the format.
///
/// - Files whose name ends with `.path.jsonl` are parsed as JSONL `Path`
///   streams and returned as `Document::Path`.
/// - All other files are parsed as canonical `{"Step"|"Path"|"Graph": ...}`
///   JSON.
pub fn read_document_auto(path: &FsPath) -> Result<Document> {
    if is_path_jsonl(path) {
        let file = std::fs::File::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let reader = BufReader::new(file);
        let p = Path::from_jsonl_reader(reader)
            .with_context(|| format!("failed to parse JSONL {}", path.display()))?;
        Ok(Document::Path(p))
    } else {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Document::from_json(&content).with_context(|| format!("failed to parse {}", path.display()))
    }
}

/// Whether `path`'s filename ends with `.path.jsonl`.
pub fn is_path_jsonl(path: &FsPath) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.ends_with(".path.jsonl"))
}
