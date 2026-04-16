use crate::io::{self as cli_io, InputSpec};
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;
use toolpath::v1::{Document, query};

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Input file (use `-` or omit to read from stdin)
    #[arg(short, long)]
    pub input: Option<PathBuf>,

    /// Walk the parent chain from this step id
    #[arg(long, value_name = "STEP_ID", conflicts_with = "dead_ends")]
    pub ancestors_of: Option<String>,

    /// Show steps not on the path to head
    #[arg(long)]
    pub dead_ends: bool,

    /// Filter by actor prefix (e.g., "human:", "agent:claude")
    #[arg(long)]
    pub actor: Option<String>,

    /// Filter by artifact path
    #[arg(long)]
    pub artifact: Option<String>,

    /// Filter: only steps at or after this ISO-8601 timestamp
    #[arg(long)]
    pub after: Option<String>,

    /// Filter: only steps at or before this ISO-8601 timestamp
    #[arg(long)]
    pub before: Option<String>,
}

pub fn run(args: QueryArgs, pretty: bool) -> Result<()> {
    let doc = cli_io::read_document(&InputSpec::from_opt(args.input))?;
    let (steps, head) = extract_steps(&doc);

    let selected: Vec<&toolpath::v1::Step> = if let Some(step_id) = args.ancestors_of {
        let ancestor_ids = query::ancestors(steps, &step_id);
        steps
            .iter()
            .filter(|s| ancestor_ids.contains(&s.step.id))
            .collect()
    } else if args.dead_ends {
        let head = head.ok_or_else(|| anyhow::anyhow!("Document has no head step"))?;
        query::dead_ends(steps, head)
    } else {
        apply_filters(steps, &args.actor, &args.artifact, &args.after, &args.before)
    };

    print_steps(&selected, pretty)
}

fn extract_steps(doc: &Document) -> (&[toolpath::v1::Step], Option<&str>) {
    match doc {
        Document::Path(p) => (p.steps.as_slice(), Some(p.path.head.as_str())),
        Document::Graph(g) => {
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

fn apply_filters<'a>(
    steps: &'a [toolpath::v1::Step],
    actor: &Option<String>,
    artifact: &Option<String>,
    after: &Option<String>,
    before: &Option<String>,
) -> Vec<&'a toolpath::v1::Step> {
    let mut result: Vec<&toolpath::v1::Step> = steps.iter().collect();

    if let Some(actor_prefix) = actor {
        let ids: std::collections::HashSet<&str> = query::filter_by_actor(steps, actor_prefix)
            .iter()
            .map(|s| s.step.id.as_str())
            .collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    if let Some(art) = artifact {
        let ids: std::collections::HashSet<&str> = query::filter_by_artifact(steps, art)
            .iter()
            .map(|s| s.step.id.as_str())
            .collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    if after.is_some() || before.is_some() {
        let start = after.as_deref().unwrap_or("");
        let end = before.as_deref().unwrap_or("9999-12-31T23:59:59Z");
        let ids: std::collections::HashSet<&str> = query::filter_by_time_range(steps, start, end)
            .iter()
            .map(|s| s.step.id.as_str())
            .collect();
        result.retain(|s| ids.contains(s.step.id.as_str()));
    }

    result
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

    fn args_with_input(path: PathBuf) -> QueryArgs {
        QueryArgs {
            input: Some(path),
            ancestors_of: None,
            dead_ends: false,
            actor: None,
            artifact: None,
            after: None,
            before: None,
        }
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
        let mut args = args_with_input(f.path().to_path_buf());
        args.ancestors_of = Some("s3".to_string());
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_dead_ends() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.dead_ends = true;
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_filter_by_actor() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.actor = Some("human:".to_string());
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_filter_by_artifact() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.artifact = Some("src/main.rs".to_string());
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_filter_by_time_range() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.after = Some("2026-01-01T10:30:00Z".to_string());
        args.before = Some("2026-01-01T11:30:00Z".to_string());
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_filter_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let args = args_with_input(f.path().to_path_buf());
        assert!(run(args, true).is_ok());
    }

    #[test]
    fn test_run_filter_after_only() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.after = Some("2026-01-01T11:00:00Z".to_string());
        assert!(run(args, false).is_ok());
    }

    #[test]
    fn test_run_dead_ends_on_step_doc() {
        let doc = Document::Step(Step::new("s1", "human:alex", "2026-01-01T00:00:00Z"));
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.dead_ends = true;
        // Should fail because Step has no head
        assert!(run(args, false).is_err());
    }

    #[test]
    fn test_run_ancestors_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.ancestors_of = Some("s3".to_string());
        assert!(run(args, true).is_ok());
    }

    #[test]
    fn test_run_dead_ends_pretty() {
        let doc = make_path_doc();
        let f = write_temp_doc(&doc);
        let mut args = args_with_input(f.path().to_path_buf());
        args.dead_ends = true;
        assert!(run(args, true).is_ok());
    }

    #[test]
    fn test_run_nonexistent_input() {
        let mut args = args_with_input(PathBuf::from("/nonexistent/file.json"));
        args.dead_ends = true;
        assert!(run(args, false).is_err());
    }
}
