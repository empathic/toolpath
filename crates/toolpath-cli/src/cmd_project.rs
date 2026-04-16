use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ProjectTarget {
    /// Project a toolpath document into Claude JSONL format
    Claude {
        /// Input toolpath document (JSON)
        #[arg(short, long)]
        input: PathBuf,

        /// Output file (JSONL). Prints to stdout if omitted.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

pub fn run(target: ProjectTarget) -> Result<()> {
    match target {
        ProjectTarget::Claude { input, output } => run_claude(input, output),
    }
}

fn run_claude(input: PathBuf, output: Option<PathBuf>) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, output);
        anyhow::bail!("'path project claude' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use toolpath_convo::ConversationProjector;

        // Read and parse the input document.
        let json = std::fs::read_to_string(&input)
            .map_err(|e| anyhow::anyhow!("Failed to read {:?}: {}", input, e))?;

        let doc: toolpath::v1::Document = serde_json::from_str(&json)
            .map_err(|e| anyhow::anyhow!("Failed to parse toolpath document: {}", e))?;

        let path = match doc {
            toolpath::v1::Document::Path(p) => p,
            toolpath::v1::Document::Step(_) => {
                anyhow::bail!("Expected a Path document, got a Step")
            }
            toolpath::v1::Document::Graph(_) => {
                anyhow::bail!("Expected a Path document, got a Graph")
            }
        };

        // Extract conversation view from the path.
        let view = toolpath_convo::extract_conversation(&path);

        // Project to Claude Conversation.
        let projector = toolpath_claude::ClaudeProjector;
        let conversation = projector
            .project(&view)
            .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

        // Serialize each entry as a JSONL line.
        let mut lines: Vec<String> = Vec::with_capacity(conversation.entries.len());
        for entry in &conversation.entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| anyhow::anyhow!("Failed to serialize entry: {}", e))?;
            lines.push(line);
        }
        let jsonl = lines.join("\n");

        // Write to output file or stdout.
        match output {
            Some(path) => {
                std::fs::write(&path, &jsonl)
                    .map_err(|e| anyhow::anyhow!("Failed to write {:?}: {}", path, e))?;
            }
            None => {
                println!("{}", jsonl);
            }
        }

        Ok(())
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, Step, StepIdentity, StructuralChange};

    fn make_path_doc() -> toolpath::v1::Document {
        let artifact_key = "agent://claude/test-session";

        let init_step = Step {
            step: StepIdentity {
                id: "step-001".to_string(),
                parents: vec![],
                actor: "tool:claude-code".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.init".to_string(),
                            extra: HashMap::new(),
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let append_step = Step {
            step: StepIdentity {
                id: "step-002".to_string(),
                parents: vec!["step-001".to_string()],
                actor: "human:user".to_string(),
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                let mut extra = HashMap::new();
                extra.insert("role".to_string(), serde_json::json!("user"));
                extra.insert("text".to_string(), serde_json::json!("Hello"));
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let path = toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head: "step-002".to_string(),
            },
            steps: vec![init_step, append_step],
            meta: None,
        };

        toolpath::v1::Document::Path(path)
    }

    #[test]
    fn test_run_claude_to_stdout() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");

        let doc = make_path_doc();
        let json = serde_json::to_string(&doc).unwrap();
        std::fs::write(&input_path, &json).unwrap();

        let result = run_claude(input_path, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_claude_to_file() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let output_path = temp.path().join("output.jsonl");

        let doc = make_path_doc();
        let json = serde_json::to_string(&doc).unwrap();
        std::fs::write(&input_path, &json).unwrap();

        let result = run_claude(input_path, Some(output_path.clone()));
        assert!(result.is_ok());

        let output = std::fs::read_to_string(&output_path).unwrap();
        // Should have at least one JSONL entry
        assert!(!output.is_empty());
        // Each non-empty line should be valid JSON
        for line in output.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.is_object());
        }
    }

    #[test]
    fn test_run_claude_rejects_step_doc() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");

        let step = toolpath::v1::Step {
            step: StepIdentity {
                id: "s1".to_string(),
                parents: vec![],
                actor: "human:alex".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            change: HashMap::new(),
            meta: None,
        };
        let doc = toolpath::v1::Document::Step(step);
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        let result = run_claude(input_path, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Step"));
    }

    #[test]
    fn test_run_claude_invalid_json() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(&input_path, "not valid json").unwrap();

        let result = run_claude(input_path, None);
        assert!(result.is_err());
    }
}
