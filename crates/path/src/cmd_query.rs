use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::{Document, query};

#[derive(Subcommand, Debug)]
pub enum QueryOp {
    /// Walk the parent chain from a step
    Ancestors {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,

        /// Step ID to trace from
        #[arg(long)]
        step_id: String,
    },
    /// Find steps not on the path to head
    DeadEnds {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,
    },
    /// Filter steps by criteria
    Filter {
        /// Input file
        #[arg(short, long)]
        input: PathBuf,

        /// Actor prefix (e.g., "human:", "agent:claude")
        #[arg(long)]
        actor: Option<String>,

        /// Artifact path
        #[arg(long)]
        artifact: Option<String>,

        /// Start time (ISO 8601)
        #[arg(long)]
        after: Option<String>,

        /// End time (ISO 8601)
        #[arg(long)]
        before: Option<String>,
    },
}

pub fn run(op: QueryOp, pretty: bool) -> Result<()> {
    match op {
        QueryOp::Ancestors { input, step_id } => run_ancestors(input, step_id, pretty),
        QueryOp::DeadEnds { input } => run_dead_ends(input, pretty),
        QueryOp::Filter {
            input,
            actor,
            artifact,
            after,
            before,
        } => run_filter(input, actor, artifact, after, before, pretty),
    }
}

fn read_doc(path: &PathBuf) -> Result<Document> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {:?}", path))?;
    Document::from_json(&content).with_context(|| format!("Failed to parse {:?}", path))
}

fn extract_steps(doc: &Document) -> (&[toolpath::v1::Step], Option<&str>) {
    match doc {
        Document::Path(p) => (p.steps.as_slice(), Some(p.path.head.as_str())),
        Document::Graph(g) => {
            // For graphs, use the first inline path
            for p in &g.paths {
                if let toolpath::v1::PathOrRef::Path(path) = p {
                    return (path.steps.as_slice(), Some(path.path.head.as_str()));
                }
            }
            (&[], None)
        }
        Document::Step(_) => (&[], None),
    }
}

fn print_steps(steps: &[&toolpath::v1::Step], pretty: bool) -> Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(&steps)?
    } else {
        serde_json::to_string(&steps)?
    };
    println!("{}", json);
    Ok(())
}

fn run_ancestors(input: PathBuf, step_id: String, pretty: bool) -> Result<()> {
    let doc = read_doc(&input)?;
    let (steps, _) = extract_steps(&doc);
    let ancestor_ids = query::ancestors(steps, &step_id);

    let ancestor_steps: Vec<&toolpath::v1::Step> = steps
        .iter()
        .filter(|s| ancestor_ids.contains(&s.step.id))
        .collect();

    print_steps(&ancestor_steps, pretty)
}

fn run_dead_ends(input: PathBuf, pretty: bool) -> Result<()> {
    let doc = read_doc(&input)?;
    let (steps, head) = extract_steps(&doc);
    let head = head.ok_or_else(|| anyhow::anyhow!("Document has no head step"))?;

    let dead = query::dead_ends(steps, head);
    print_steps(&dead, pretty)
}

fn run_filter(
    input: PathBuf,
    actor: Option<String>,
    artifact: Option<String>,
    after: Option<String>,
    before: Option<String>,
    pretty: bool,
) -> Result<()> {
    let doc = read_doc(&input)?;
    let (steps, _) = extract_steps(&doc);

    let mut result: Vec<&toolpath::v1::Step> = steps.iter().collect();

    if let Some(ref actor_prefix) = actor {
        let filtered = query::filter_by_actor(steps, actor_prefix);
        let ids: std::collections::HashSet<&str> =
            filtered.iter().map(|s| s.step.id.as_str()).collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    if let Some(ref art) = artifact {
        let filtered = query::filter_by_artifact(steps, art);
        let ids: std::collections::HashSet<&str> =
            filtered.iter().map(|s| s.step.id.as_str()).collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    if after.is_some() || before.is_some() {
        let start = after.as_deref().unwrap_or("");
        let end = before.as_deref().unwrap_or("9999-12-31T23:59:59Z");
        let filtered = query::filter_by_time_range(steps, start, end);
        let ids: std::collections::HashSet<&str> =
            filtered.iter().map(|s| s.step.id.as_str()).collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    print_steps(&result, pretty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use toolpath::v1::{Base, Path, PathIdentity, Step};

    fn make_path_doc() -> Document {
        let s1 = Step::new("s1", "human:alex", "2026-01-01T10:00:00Z")
            .with_raw_change("src/main.rs", "@@");
        let s2 = Step::new("s2", "agent:claude", "2026-01-01T11:00:00Z")
            .with_parent("s1")
            .with_raw_change("src/lib.rs", "@@");
        let s2a = Step::new("s2a", "agent:claude", "2026-01-01T11:30:00Z")
            .with_parent("s1")
            .with_raw_change("src/main.rs", "@@");
        let s3 = Step::new("s3", "human:alex", "2026-01-01T12:00:00Z")
            .with_parent("s2")
            .with_raw_change("src/main.rs", "@@");
        Document::Path(Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc")),
                head: "s3".into(),
            },
            steps: vec![s1, s2, s2a, s3],
            meta: None,
        })
    }

    fn write_temp_doc(doc: &Document) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{}", doc.to_json().unwrap()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_extract_steps_from_path() {
        let doc = make_path_doc();
        let (steps, head) = extract_steps(&doc);
        assert_eq!(steps.len(), 4);
        assert_eq!(head, Some("s3"));
    }

    #[test]
    fn test_extract_steps_from_step() {
        let doc = Document::Step(Step::new("s1", "human:alex", "2026-01-01T00:00:00Z"));
        let (steps, head) = extract_steps(&doc);
        assert!(steps.is_empty());
        assert!(head.is_none());
    }

    #[test]
    fn test_extract_steps_from_graph() {
        let s1 =
            Step::new("s1", "human:alex", "2026-01-01T00:00:00Z").with_raw_change("f.rs", "@@");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s1],
            meta: None,
        };
        let graph = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g1".into() },
            paths: vec![toolpath::v1::PathOrRef::Path(Box::new(path))],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let (steps, head) = extract_steps(&doc);
        assert_eq!(steps.len(), 1);
        assert_eq!(head, Some("s1"));
    }

    #[test]
    fn test_run_ancestors() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_ancestors(f.path().to_path_buf(), "s3".to_string(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dead_ends() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_dead_ends(f.path().to_path_buf(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_filter_by_actor() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_filter(
            f.path().to_path_buf(),
            Some("human:".to_string()),
            None,
            None,
            None,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_filter_by_artifact() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_filter(
            f.path().to_path_buf(),
            None,
            Some("src/main.rs".to_string()),
            None,
            None,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_filter_by_time_range() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_filter(
            f.path().to_path_buf(),
            None,
            None,
            Some("2026-01-01T10:30:00Z".to_string()),
            Some("2026-01-01T11:30:00Z".to_string()),
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_filter_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_filter(f.path().to_path_buf(), None, None, None, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_filter_after_only() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_filter(
            f.path().to_path_buf(),
            None,
            None,
            Some("2026-01-01T11:00:00Z".to_string()),
            None,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dead_ends_on_step_doc() {
        let doc = Document::Step(Step::new("s1", "human:alex", "2026-01-01T00:00:00Z"));
        let f = write_temp_doc(&doc);
        let result = run_dead_ends(f.path().to_path_buf(), false);
        // Should fail because Step has no head
        assert!(result.is_err());
    }

    #[test]
    fn test_run_ancestors_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_ancestors(f.path().to_path_buf(), "s3".to_string(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dead_ends_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let result = run_dead_ends(f.path().to_path_buf(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_read_doc_invalid_path() {
        let result = read_doc(&PathBuf::from("/nonexistent/file.json"));
        assert!(result.is_err());
    }
}
