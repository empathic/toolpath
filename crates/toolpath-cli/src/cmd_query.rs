use crate::io::{self as cli_io, InputSpec};
use anyhow::Result;
use clap::Args;
use std::collections::HashSet;
use std::path::PathBuf;
use toolpath::v1::{Document, Step, query};

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

/// A view into one inline Path: its steps and the head step id.
struct PathView<'a> {
    steps: &'a [Step],
    head: &'a str,
}

pub fn run(args: QueryArgs, pretty: bool) -> Result<()> {
    let doc = cli_io::read_document(&InputSpec::from_opt(args.input))?;
    let views = collect_paths(&doc);

    let selected: Vec<&Step> = if let Some(step_id) = args.ancestors_of {
        collect_ancestors(&views, &step_id)
    } else if args.dead_ends {
        collect_dead_ends(&views)?
    } else {
        collect_filtered(&views, &args.actor, &args.artifact, &args.after, &args.before)
    };

    print_steps(&selected, pretty)
}

/// Collect every inline Path in the document. Graphs contribute every inline
/// path (not just the first); Path docs contribute themselves; Step docs
/// contribute nothing.
fn collect_paths(doc: &Document) -> Vec<PathView<'_>> {
    match doc {
        Document::Path(p) => vec![PathView {
            steps: p.steps.as_slice(),
            head: p.path.head.as_str(),
        }],
        Document::Graph(g) => g
            .paths
            .iter()
            .filter_map(|p| match p {
                toolpath::v1::PathOrRef::Path(path) => Some(PathView {
                    steps: path.steps.as_slice(),
                    head: path.path.head.as_str(),
                }),
                toolpath::v1::PathOrRef::Ref(_) => None,
            })
            .collect(),
        Document::Step(_) => Vec::new(),
    }
}

fn collect_ancestors<'a>(views: &[PathView<'a>], step_id: &str) -> Vec<&'a Step> {
    let mut ancestor_ids: HashSet<String> = HashSet::new();
    for v in views {
        for id in query::ancestors(v.steps, step_id) {
            ancestor_ids.insert(id);
        }
    }
    dedup_refs(views, |s| ancestor_ids.contains(&s.step.id))
}

fn collect_dead_ends<'a>(views: &[PathView<'a>]) -> Result<Vec<&'a Step>> {
    if views.is_empty() {
        anyhow::bail!("Document has no head step");
    }
    let mut dead_ids: HashSet<String> = HashSet::new();
    for v in views {
        for s in query::dead_ends(v.steps, v.head) {
            dead_ids.insert(s.step.id.clone());
        }
    }
    Ok(dedup_refs(views, |s| dead_ids.contains(&s.step.id)))
}

fn collect_filtered<'a>(
    views: &[PathView<'a>],
    actor: &Option<String>,
    artifact: &Option<String>,
    after: &Option<String>,
    before: &Option<String>,
) -> Vec<&'a Step> {
    dedup_refs(views, |s| {
        if let Some(prefix) = actor
            && !s.step.actor.starts_with(prefix)
        {
            return false;
        }
        if let Some(art) = artifact
            && !s.change.contains_key(art)
        {
            return false;
        }
        if after.is_some() || before.is_some() {
            let ts = s.step.timestamp.as_str();
            let start = after.as_deref().unwrap_or("");
            let end = before.as_deref().unwrap_or("9999-12-31T23:59:59Z");
            if ts < start || ts > end {
                return false;
            }
        }
        true
    })
}

/// Iterate steps across all views and collect references matching `pred`,
/// deduplicating by step id.
fn dedup_refs<'a, F>(views: &[PathView<'a>], pred: F) -> Vec<&'a Step>
where
    F: Fn(&Step) -> bool,
{
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<&Step> = Vec::new();
    for v in views {
        for s in v.steps {
            if pred(s) && seen.insert(s.step.id.as_str()) {
                out.push(s);
            }
        }
    }
    out
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

    fn make_graph_two_paths() -> Document {
        // path-A: s1 → s2 (head=s2), with abandoned s1a off s1
        let s1 = Step::new("s1", "human:alex", "2026-01-01T10:00:00Z")
            .with_raw_change("src/main.rs", "@@");
        let s1a = Step::new("s1a", "agent:claude", "2026-01-01T10:15:00Z")
            .with_parent("s1")
            .with_raw_change("src/main.rs", "@@");
        let s2 = Step::new("s2", "human:alex", "2026-01-01T11:00:00Z")
            .with_parent("s1")
            .with_raw_change("src/main.rs", "@@");
        let path_a = Path {
            path: PathIdentity {
                id: "pA".into(),
                base: None,
                head: "s2".into(),
            },
            steps: vec![s1, s1a, s2],
            meta: None,
        };

        // path-B: t1 → t2 (head=t2), with abandoned t1a off t1
        let t1 = Step::new("t1", "tool:rustfmt", "2026-01-02T10:00:00Z")
            .with_raw_change("src/lib.rs", "@@");
        let t1a = Step::new("t1a", "agent:claude", "2026-01-02T10:15:00Z")
            .with_parent("t1")
            .with_raw_change("src/lib.rs", "@@");
        let t2 = Step::new("t2", "tool:rustfmt", "2026-01-02T11:00:00Z")
            .with_parent("t1")
            .with_raw_change("src/lib.rs", "@@");
        let path_b = Path {
            path: PathIdentity {
                id: "pB".into(),
                base: None,
                head: "t2".into(),
            },
            steps: vec![t1, t1a, t2],
            meta: None,
        };

        Document::Graph(toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g1".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(path_a)),
                toolpath::v1::PathOrRef::Path(Box::new(path_b)),
            ],
            meta: None,
        })
    }

    #[test]
    fn test_collect_paths_from_path_doc() {
        let doc = make_path_doc();
        let views = collect_paths(&doc);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].steps.len(), 4);
        assert_eq!(views[0].head, "s3");
    }

    #[test]
    fn test_collect_paths_from_step_doc() {
        let doc = Document::Step(Step::new("s1", "human:alex", "2026-01-01T00:00:00Z"));
        let views = collect_paths(&doc);
        assert!(views.is_empty());
    }

    #[test]
    fn test_collect_paths_from_graph_visits_every_inline_path() {
        // Regression: previously only the first inline path was inspected.
        let doc = make_graph_two_paths();
        let views = collect_paths(&doc);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].head, "s2");
        assert_eq!(views[1].head, "t2");
    }

    #[test]
    fn test_collect_paths_skips_refs() {
        let s = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z")
            .with_raw_change("f.rs", "@@");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: None,
                head: "s1".into(),
            },
            steps: vec![s],
            meta: None,
        };
        let graph = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g1".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(path)),
                toolpath::v1::PathOrRef::Ref(toolpath::v1::PathRef {
                    ref_url: "https://example.com/p.json".into(),
                }),
            ],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let views = collect_paths(&doc);
        assert_eq!(views.len(), 1);
    }

    #[test]
    fn test_collect_dead_ends_graph_unions_across_paths() {
        // Each inline path has one abandoned branch; expect both in output.
        let doc = make_graph_two_paths();
        let views = collect_paths(&doc);
        let dead = collect_dead_ends(&views).unwrap();
        let ids: HashSet<&str> = dead.iter().map(|s| s.step.id.as_str()).collect();
        assert!(ids.contains("s1a"), "expected dead-end from path A");
        assert!(ids.contains("t1a"), "expected dead-end from path B");
    }

    #[test]
    fn test_collect_filtered_graph_unions_across_paths() {
        // Filter by "agent:" — one match per inline path.
        let doc = make_graph_two_paths();
        let views = collect_paths(&doc);
        let filtered = collect_filtered(&views, &Some("agent:".into()), &None, &None, &None);
        let ids: HashSet<&str> = filtered.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("s1a"));
        assert!(ids.contains("t1a"));
    }

    #[test]
    fn test_collect_ancestors_graph_finds_step_in_any_path() {
        // `t2` lives only in path B — ancestors should walk that path.
        let doc = make_graph_two_paths();
        let views = collect_paths(&doc);
        let anc = collect_ancestors(&views, "t2");
        let ids: HashSet<&str> = anc.iter().map(|s| s.step.id.as_str()).collect();
        assert!(ids.contains("t1"));
        assert!(ids.contains("t2"));
        assert!(!ids.contains("t1a"), "abandoned branch shouldn't be an ancestor");
    }

    #[test]
    fn test_collect_filtered_dedups_duplicate_step_ids_across_paths() {
        // Same step id appearing in two inline paths should only show once.
        let s = Step::new("shared", "human:alex", "2026-01-01T10:00:00Z")
            .with_raw_change("f.rs", "@@");
        let make = |id: &str| Path {
            path: PathIdentity {
                id: id.into(),
                base: None,
                head: "shared".into(),
            },
            steps: vec![s.clone()],
            meta: None,
        };
        let graph = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(make("p1"))),
                toolpath::v1::PathOrRef::Path(Box::new(make("p2"))),
            ],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let views = collect_paths(&doc);
        let filtered = collect_filtered(&views, &Some("human:".into()), &None, &None, &None);
        assert_eq!(filtered.len(), 1, "duplicate step ids should collapse");
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
