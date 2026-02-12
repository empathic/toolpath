use anyhow::{Context, Result};
use toolpath::v1::{Document, Graph, GraphIdentity, GraphMeta, PathOrRef};

/// Merge multiple Toolpath documents into a single Graph.
///
/// Accepts file paths as arguments. Use `-` to read one document from stdin.
/// Each input can be a Step, Path, or Graph — paths are extracted and combined.
pub fn run(inputs: Vec<String>, title: Option<String>, pretty: bool) -> Result<()> {
    let mut all_paths = Vec::new();

    for input in &inputs {
        let content = if input == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("Failed to read from stdin")?;
            buf
        } else {
            std::fs::read_to_string(input).with_context(|| format!("Failed to read {:?}", input))?
        };

        let doc = Document::from_json(&content)
            .with_context(|| format!("Failed to parse {:?}", input))?;

        extract_paths(doc, &mut all_paths);
    }

    let doc = merge_into_graph(all_paths, title);

    let json = if pretty {
        doc.to_json_pretty()?
    } else {
        doc.to_json()?
    };
    println!("{}", json);

    Ok(())
}

/// Extract paths from a document and append them to the collector.
fn extract_paths(doc: Document, paths: &mut Vec<PathOrRef>) {
    match doc {
        Document::Graph(g) => {
            paths.extend(g.paths);
        }
        Document::Path(p) => {
            paths.push(PathOrRef::Path(Box::new(p)));
        }
        Document::Step(s) => {
            // Wrap a bare step in a minimal path
            let step_id = s.step.id.clone();
            let path = toolpath::v1::Path {
                path: toolpath::v1::PathIdentity {
                    id: format!("path-{}", step_id),
                    base: None,
                    head: step_id,
                },
                steps: vec![s],
                meta: None,
            };
            paths.push(PathOrRef::Path(Box::new(path)));
        }
    }
}

/// Merge collected paths into a Graph document.
fn merge_into_graph(paths: Vec<PathOrRef>, title: Option<String>) -> Document {
    let graph_id = format!("graph-merged-{}", paths.len());

    Document::Graph(Graph {
        graph: GraphIdentity { id: graph_id },
        paths,
        meta: title.map(|t| GraphMeta {
            title: Some(t),
            ..Default::default()
        }),
    })
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
            },
            steps,
            meta: Some(PathMeta {
                title: Some(format!("Path: {}", id)),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn test_extract_paths_from_path_doc() {
        let path = make_path("p1", vec![make_step("s1", "human:alex")]);
        let doc = Document::Path(path);
        let mut paths = Vec::new();
        extract_paths(doc, &mut paths);
        assert_eq!(paths.len(), 1);
        if let PathOrRef::Path(p) = &paths[0] {
            assert_eq!(p.path.id, "p1");
        } else {
            panic!("Expected Path, got Ref");
        }
    }

    #[test]
    fn test_extract_paths_from_graph_doc() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let p2 = make_path("p2", vec![make_step("s2", "agent:claude")]);
        let graph = Graph {
            graph: GraphIdentity {
                id: "g1".to_string(),
            },
            paths: vec![PathOrRef::Path(Box::new(p1)), PathOrRef::Path(Box::new(p2))],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let mut paths = Vec::new();
        extract_paths(doc, &mut paths);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_extract_paths_from_step_doc() {
        let step = make_step("s1", "human:alex");
        let doc = Document::Step(step);
        let mut paths = Vec::new();
        extract_paths(doc, &mut paths);
        assert_eq!(paths.len(), 1);
        if let PathOrRef::Path(p) = &paths[0] {
            assert_eq!(p.path.id, "path-s1");
            assert_eq!(p.steps.len(), 1);
            assert_eq!(p.path.head, "s1");
        } else {
            panic!("Expected Path, got Ref");
        }
    }

    #[test]
    fn test_extract_paths_from_graph_with_refs() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let graph = Graph {
            graph: GraphIdentity {
                id: "g1".to_string(),
            },
            paths: vec![
                PathOrRef::Path(Box::new(p1)),
                PathOrRef::Ref(PathRef {
                    ref_url: "https://example.com/path.json".to_string(),
                }),
            ],
            meta: None,
        };
        let doc = Document::Graph(graph);
        let mut paths = Vec::new();
        extract_paths(doc, &mut paths);
        assert_eq!(paths.len(), 2);
        assert!(matches!(&paths[1], PathOrRef::Ref(_)));
    }

    #[test]
    fn test_merge_into_graph_no_title() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![PathOrRef::Path(Box::new(p1))];
        let doc = merge_into_graph(paths, None);
        if let Document::Graph(g) = doc {
            assert_eq!(g.graph.id, "graph-merged-1");
            assert_eq!(g.paths.len(), 1);
            assert!(g.meta.is_none());
        } else {
            panic!("Expected Graph");
        }
    }

    #[test]
    fn test_merge_into_graph_with_title() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let p2 = make_path("p2", vec![make_step("s2", "agent:claude")]);
        let paths = vec![PathOrRef::Path(Box::new(p1)), PathOrRef::Path(Box::new(p2))];
        let doc = merge_into_graph(paths, Some("My Graph".to_string()));
        if let Document::Graph(g) = doc {
            assert_eq!(g.graph.id, "graph-merged-2");
            assert_eq!(g.paths.len(), 2);
            assert_eq!(g.meta.unwrap().title.unwrap(), "My Graph");
        } else {
            panic!("Expected Graph");
        }
    }

    #[test]
    fn test_merge_empty() {
        let doc = merge_into_graph(Vec::new(), None);
        if let Document::Graph(g) = doc {
            assert_eq!(g.graph.id, "graph-merged-0");
            assert!(g.paths.is_empty());
        } else {
            panic!("Expected Graph");
        }
    }

    #[test]
    fn test_merge_roundtrip_json() {
        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let paths = vec![PathOrRef::Path(Box::new(p1))];
        let doc = merge_into_graph(paths, Some("Test".to_string()));
        let json = doc.to_json().unwrap();
        let parsed = Document::from_json(&json).unwrap();
        if let Document::Graph(g) = parsed {
            assert_eq!(g.paths.len(), 1);
        } else {
            panic!("Expected Graph after roundtrip");
        }
    }

    #[test]
    fn test_run_with_temp_files_pretty() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        let p1 = make_path("p1", vec![make_step("s1", "human:alex")]);
        let f1 = dir.path().join("doc1.json");
        let mut file1 = std::fs::File::create(&f1).unwrap();
        write!(file1, "{}", Document::Path(p1).to_json().unwrap()).unwrap();

        let result = run(
            vec![f1.to_str().unwrap().to_string()],
            Some("Pretty Test".to_string()),
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
        write!(file1, "{}", Document::Path(p1).to_json().unwrap()).unwrap();
        write!(file2, "{}", Document::Path(p2).to_json().unwrap()).unwrap();

        // run() prints to stdout — just verify it doesn't error
        let result = run(
            vec![
                f1.to_str().unwrap().to_string(),
                f2.to_str().unwrap().to_string(),
            ],
            Some("Combined".to_string()),
            false,
        );
        assert!(result.is_ok());
    }
}
