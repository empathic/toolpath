//! `path p query` — low-level graph traversal on a single document.
//!
//! The porcelain `path query` (jaq over the whole cache) covers filtering,
//! dead-end detection, and aggregation. What lives here is the one query that
//! isn't a per-step predicate: `ancestors`, which walks the parent chain from
//! a step.

use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::{Graph, PathOrRef, query};

#[derive(Subcommand, Debug)]
pub enum PQueryOp {
    /// Walk the parent chain from a step
    Ancestors {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,

        /// Step ID to trace from
        #[arg(long)]
        step_id: String,
    },
}

pub fn run(op: PQueryOp, pretty: bool) -> Result<()> {
    match op {
        PQueryOp::Ancestors { input, step_id } => run_ancestors(input, step_id, pretty),
    }
}

/// Returns the steps from the graph's first inline path.
fn extract_steps(doc: &Graph) -> &[toolpath::v1::Step] {
    for entry in &doc.paths {
        if let PathOrRef::Path(path) = entry {
            return path.steps.as_slice();
        }
    }
    &[]
}

fn run_ancestors(input: PathBuf, step_id: String, pretty: bool) -> Result<()> {
    let doc = crate::io::read_document_auto(&input)?;
    let steps = extract_steps(&doc);
    let ancestor_ids = query::ancestors(steps, &step_id);

    let ancestor_steps: Vec<&toolpath::v1::Step> = steps
        .iter()
        .filter(|s| ancestor_ids.contains(&s.step.id))
        .collect();

    let json = if pretty {
        serde_json::to_string_pretty(&ancestor_steps)?
    } else {
        serde_json::to_string(&ancestor_steps)?
    };
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use toolpath::v1::{Base, Path, PathIdentity, Step};

    fn make_path_doc() -> Graph {
        let s1 = Step::new("s1", "human:alex", "2026-01-01T10:00:00Z")
            .with_raw_change("src/main.rs", "@@");
        let s2 = Step::new("s2", "agent:claude", "2026-01-01T11:00:00Z")
            .with_parent("s1")
            .with_raw_change("src/lib.rs", "@@");
        let s3 = Step::new("s3", "human:alex", "2026-01-01T12:00:00Z")
            .with_parent("s2")
            .with_raw_change("src/main.rs", "@@");
        Graph::from_path(Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc")),
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2, s3],
            meta: None,
        })
    }

    fn write_temp_doc(doc: &Graph) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn extract_steps_reads_first_inline_path() {
        let doc = make_path_doc();
        assert_eq!(extract_steps(&doc).len(), 3);
    }

    #[test]
    fn extract_steps_empty_graph() {
        let doc = Graph::new("g1");
        assert!(extract_steps(&doc).is_empty());
    }

    #[test]
    fn ancestors_runs() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        assert!(run_ancestors(f.path().to_path_buf(), "s3".to_string(), false).is_ok());
        assert!(run_ancestors(f.path().to_path_buf(), "s3".to_string(), true).is_ok());
    }
}
