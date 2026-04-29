use anyhow::{Context, Result};
use std::path::PathBuf;
use toolpath::v1::Graph;

use crate::io::{is_path_jsonl, read_document_auto};
use crate::schema;

pub fn run(input: PathBuf) -> Result<()> {
    let doc = read_document_auto(&input).map_err(|e| anyhow::anyhow!("Invalid: {e}"))?;

    // Schema validation applies to canonical `.path.json` documents. JSONL
    // streams have their own per-line envelope shapes (`{"PathOpen": ...}`,
    // `{"Step": ...}`, …) that aren't graph-shaped at the wire level —
    // strict parsing through `Graph::from_jsonl_*` is the validation hook
    // for that format.
    if !is_path_jsonl(&input) {
        let raw = std::fs::read_to_string(&input)
            .with_context(|| format!("failed to re-read {} for schema check", input.display()))?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {} as JSON", input.display()))?;
        schema::validate(&value).map_err(|e| anyhow::anyhow!("Invalid: {e}"))?;
    }

    println!("Valid: {}", describe(&doc));
    Ok(())
}

fn describe(doc: &Graph) -> String {
    let path_count = doc.paths.len();
    format!(
        "Graph (id: {}, {} {})",
        doc.graph.id,
        path_count,
        if path_count == 1 { "path" } else { "paths" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &str, suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn validate_empty_graph() {
        let f = write_temp(r#"{"graph":{"id":"g1"},"paths":[]}"#, ".json");
        run(f.path().to_path_buf()).expect("empty graph should validate");
    }

    #[test]
    fn validate_single_path_graph() {
        let json = r#"{"graph":{"id":"g1"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[{"step":{"id":"s1","actor":"human:alex","timestamp":"2026-01-01T00:00:00Z"},"change":{"src/main.rs":{"raw":"@@ -1 +1 @@\n-a\n+b"}}}]}]}"#;
        let f = write_temp(json, ".json");
        run(f.path().to_path_buf()).expect("single-path graph should validate");
    }

    #[test]
    fn validate_invalid_json() {
        let f = write_temp("not json", ".json");
        assert!(run(f.path().to_path_buf()).is_err());
    }

    #[test]
    fn validate_missing_required_field() {
        let f = write_temp(r#"{"paths":[]}"#, ".json");
        assert!(run(f.path().to_path_buf()).is_err());
    }

    /// The headline schema-drift catch: a `path.base.commit` field (a stale
    /// holdover the simplification commit forgot to rename to `ref`) must
    /// be rejected. Without schema validation wired in, the type-only
    /// round-trip silently dropped this and called the document Valid.
    #[test]
    fn validate_rejects_stale_path_base_commit_field() {
        let json = r#"{
            "graph": {"id": "g1"},
            "paths": [{
                "path": {
                    "id": "p1",
                    "base": {"uri": "github:org/repo", "ref": "main", "commit": "abc123"},
                    "head": "s1"
                },
                "steps": [{
                    "step": {"id": "s1", "actor": "human:alex", "timestamp": "2026-01-01T00:00:00Z"},
                    "change": {"src/main.rs": {"raw": "@@ -1 +1 @@\n-a\n+b"}}
                }]
            }]
        }"#;
        let f = write_temp(json, ".json");
        let err = run(f.path().to_path_buf())
            .expect_err("base.commit is not in the schema and must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("commit"),
            "error should name the offending field, got: {msg}"
        );
    }

    #[test]
    fn run_nonexistent_file() {
        assert!(run(PathBuf::from("/nonexistent/file.json")).is_err());
    }
}
