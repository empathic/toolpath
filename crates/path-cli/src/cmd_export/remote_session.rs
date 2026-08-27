//! `p export claude --cwd`: the session's cwd on the host that resumes
//! it.

use anyhow::Result;

/// The `p export claude` flags that rewrite the projected session
/// before it is written.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote session")]
pub struct RemoteSessionArgs {
    /// Root the session at this directory: it becomes the `cwd` of
    /// every line that carries one. Absolute POSIX path in
    /// normalized form; it does not have to exist on this machine.
    /// Mutually exclusive with --project.
    // `--project` files the session under the slug of the project
    // directory, and Claude Code reads every entry's `cwd` as that
    // directory. A second directory value can only repeat it or
    // contradict it.
    #[arg(long, value_name = "DIR", conflicts_with = "project", value_parser = parse_cwd_arg)]
    pub(super) cwd: Option<String>,
}

/// Claude Code keys a session on the exact `cwd` string, so the value
/// must be an absolute POSIX path in normalized form: no `.`, `..`, or
/// empty component. One trailing `/` is dropped. The directory may be
/// on another machine, so it is not required to exist.
fn parse_cwd_arg(raw: &str) -> Result<String> {
    let Some(rest) = raw.strip_prefix('/') else {
        anyhow::bail!("--cwd must be an absolute POSIX path (got {raw:?})");
    };
    if rest.is_empty() {
        return Ok("/".to_string());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        anyhow::bail!("--cwd must not contain an empty, `.`, or `..` component (got {raw:?})");
    }
    Ok(format!("/{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_export::run_claude;
    use crate::cmd_export::tests::make_path_doc;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, Step, StepIdentity, StructuralChange};

    /// `make_path_doc` with `cwd` recorded on every step, plus one
    /// headerless line that carries a `cwd`.
    fn make_path_doc_with_cwd(cwd: &str) -> toolpath::v1::Graph {
        let mut path = make_path_doc().into_single_path().unwrap();
        for step in &mut path.steps {
            for change in step.change.values_mut() {
                if let Some(structural) = change.structural.as_mut() {
                    structural
                        .extra
                        .insert("cwd".to_string(), serde_json::json!(cwd));
                }
            }
        }
        let artifact_key = path.steps[0].change.keys().next().unwrap().clone();
        let mut extra = HashMap::new();
        extra.insert("entry_type".to_string(), serde_json::json!("custom-title"));
        extra.insert(
            "raw".to_string(),
            serde_json::json!({"type": "custom-title", "cwd": cwd, "customTitle": "x"}),
        );
        path.steps.push(Step {
            step: StepIdentity {
                id: "step-003".to_string(),
                parents: vec!["step-002".to_string()],
                actor: "tool:claude-code".to_string(),
                timestamp: "2024-01-01T00:00:02Z".to_string(),
            },
            change: HashMap::from([(
                artifact_key,
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: "conversation.event".to_string(),
                        extra,
                    }),
                },
            )]),
            meta: None,
        });
        path.path.head = "step-003".to_string();
        toolpath::v1::Graph::from_path(path)
    }

    /// Runs `p export claude --output` on `doc` and parses the lines.
    fn export_claude_lines(doc: &toolpath::v1::Graph, cwd: Option<&str>) -> Vec<serde_json::Value> {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let output_path = temp.path().join("out.jsonl");
        std::fs::write(&input_path, serde_json::to_string(doc).unwrap()).unwrap();
        run_claude(
            input_path.to_string_lossy().to_string(),
            None,
            Some(output_path.clone()),
            false,
            RemoteSessionArgs {
                cwd: cwd.map(str::to_string),
            },
        )
        .unwrap();
        std::fs::read_to_string(&output_path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn values_of<'a>(lines: &'a [serde_json::Value], key: &str) -> Vec<&'a str> {
        lines.iter().filter_map(|v| v.get(key)?.as_str()).collect()
    }

    #[test]
    fn cwd_flag_rewrites_every_cwd() {
        let doc = make_path_doc_with_cwd("/old/project");
        let plain = export_claude_lines(&doc, None);
        let old = values_of(&plain, "cwd");
        assert!(!old.is_empty());
        assert!(old.iter().all(|c| *c == "/old/project"));

        let rooted = export_claude_lines(&doc, Some("/new/dir"));
        assert_eq!(rooted.len(), plain.len());
        let new = values_of(&rooted, "cwd");
        assert_eq!(new.len(), old.len());
        assert!(new.iter().all(|c| *c == "/new/dir"));
        let preamble = rooted
            .iter()
            .find(|v| v["type"] == "custom-title")
            .expect("the headerless line survives export");
        assert_eq!(preamble["cwd"], "/new/dir");
    }

    #[test]
    fn cwd_flag_leaves_session_ids_alone() {
        let doc = make_path_doc_with_cwd("/old/project");
        let plain = export_claude_lines(&doc, None);
        let rooted = export_claude_lines(&doc, Some("/new/dir"));
        assert_eq!(
            values_of(&plain, "sessionId"),
            values_of(&rooted, "sessionId")
        );
    }

    #[test]
    fn cwd_flag_rejects_unnormalized_paths() {
        for bad in ["relative/dir", "/a/../b", "/a/./b", "/a//b", "//", ""] {
            assert!(parse_cwd_arg(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_cwd_arg("/a/b/").unwrap(), "/a/b");
        assert_eq!(parse_cwd_arg("/").unwrap(), "/");
    }
}
