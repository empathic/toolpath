use anyhow::{Context, Result};
use toolpath::v1::{Graph, GraphIdentity, GraphMeta, PathOrRef};

/// Merge multiple Toolpath graphs into a single combined Graph.
///
/// Accepts file paths as arguments. Use `-` to read one document from stdin.
/// Each input is a Graph; their `paths` collections are concatenated into one.
pub fn run(
    inputs: Vec<String>,
    title: Option<String>,
    description: Option<String>,
    pretty: bool,
) -> Result<()> {
    let mut all_paths = Vec::new();

    for input in &inputs {
        let doc = if input == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("Failed to read from stdin")?;
            Graph::from_json(&buf).with_context(|| format!("Failed to parse {:?}", input))?
        } else {
            crate::io::read_document_auto(std::path::Path::new(input))?
        };

        all_paths.extend(doc.paths);
    }

    let doc = merge_into_graph(all_paths, title, description);

    let json = if pretty {
        doc.to_json_pretty()?
    } else {
        doc.to_json()?
    };
    println!("{}", json);

    Ok(())
}

/// Merge collected paths into a single Graph document.
fn merge_into_graph(
    paths: Vec<PathOrRef>,
    title: Option<String>,
    description: Option<String>,
) -> Graph {
    let graph_id = format!("graph-merged-{}", paths.len());

    let meta = (title.is_some() || description.is_some()).then(|| GraphMeta {
        title,
        description,
        ..Default::default()
    });

    Graph {
        graph: GraphIdentity { id: graph_id },
        paths,
        meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{Base, Path, PathIdentity, PathMeta, PathRef, Step};

    fn make_step(id: &str, actor: &str) -> Step {
        Step::new(id, actor, "2026-01-01T00:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-old\n+new")
    }

    fn make_path(id: &str, steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        Path {
            path: PathIdentity {
                id: id.to_string(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head,
                graph_ref: None,
            },
            steps,
            meta: Some(PathMeta {
                title: Some(format!("Path: {}", id)),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn test_merge_into_graph_no_title() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![PathOrRef::Path(Box::new(p1))];
        let doc = merge_into_graph(paths, None, None);
        assert_eq!(doc.graph.id, "graph-merged-1");
        assert_eq!(doc.paths.len(), 1);
        assert!(doc.meta.is_none());
    }

    #[test]
    fn test_merge_into_graph_with_title() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let p2 = make_path("p2", vec![make_step("s2", "agent:claude")]);
        let paths = vec![PathOrRef::Path(Box::new(p1)), PathOrRef::Path(Box::new(p2))];
        let doc = merge_into_graph(paths, Some("My Graph".to_string()), None);
        assert_eq!(doc.graph.id, "graph-merged-2");
        assert_eq!(doc.paths.len(), 2);
        assert_eq!(doc.meta.unwrap().title.unwrap(), "My Graph");
    }

    #[test]
    fn test_merge_into_graph_with_description_only() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![PathOrRef::Path(Box::new(p1))];
        let doc = merge_into_graph(paths, None, Some("Two attempts at the fix".to_string()));
        let meta = doc.meta.unwrap();
        assert!(meta.title.is_none());
        assert_eq!(meta.description.unwrap(), "Two attempts at the fix");
    }

    #[test]
    fn test_merge_into_graph_preserves_refs() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![
            PathOrRef::Path(Box::new(p1)),
            PathOrRef::Ref(PathRef {
                ref_url: "https://example.com/path.json".to_string(),
            }),
        ];
        let doc = merge_into_graph(paths, None, None);
        assert_eq!(doc.paths.len(), 2);
        assert!(matches!(&doc.paths[1], PathOrRef::Ref(_)));
    }

    #[test]
    fn test_merge_empty() {
        let doc = merge_into_graph(Vec::new(), None, None);
        assert_eq!(doc.graph.id, "graph-merged-0");
        assert!(doc.paths.is_empty());
    }

    #[test]
    fn test_merge_roundtrip_json() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![PathOrRef::Path(Box::new(p1))];
        let doc = merge_into_graph(paths, Some("Test".to_string()), None);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        assert_eq!(parsed.paths.len(), 1);
    }

    #[test]
    fn test_run_with_temp_files_pretty() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let f1 = dir.path().join("doc1.json");
        let mut file1 = std::fs::File::create(&f1).unwrap();
        write!(file1, "{}", Graph::from_path(p1).to_json().unwrap()).unwrap();

        let result = run(
            vec![f1.to_str().unwrap().to_string()],
            Some("Pretty Test".to_string()),
            None,
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_temp_files() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let p2 = make_path("p2", vec![make_step("s2", "tool:rustfmt")]);

        let f1 = dir.path().join("doc1.json");
        let f2 = dir.path().join("doc2.json");
        let mut file1 = std::fs::File::create(&f1).unwrap();
        let mut file2 = std::fs::File::create(&f2).unwrap();
        write!(file1, "{}", Graph::from_path(p1).to_json().unwrap()).unwrap();
        write!(file2, "{}", Graph::from_path(p2).to_json().unwrap()).unwrap();

        let result = run(
            vec![
                f1.to_str().unwrap().to_string(),
                f2.to_str().unwrap().to_string(),
            ],
            Some("Combined".to_string()),
            None,
            false,
        );
        assert!(result.is_ok());
    }
}
