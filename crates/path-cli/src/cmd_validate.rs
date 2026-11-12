use anyhow::{Context, Result};
use std::path::PathBuf;
use toolpath::v1::{
    Graph, PATH_KIND_AGENT_CODING_SESSION, PATH_KIND_AGENT_CODING_SESSION_V1_0_0, PathOrRef,
};

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

    for warning in legacy_kind_warnings(&doc) {
        eprintln!("warning: {warning}");
    }
    println!("Valid: {}", describe(&doc));
    Ok(())
}

/// One line per path whose `meta.kind` is the superseded
/// `agent-coding-session/v1.0.0`.
///
/// The document still validates: kind URIs are immutable and a document
/// written against an old one stays valid, and the v1.0.0 schema is still
/// bundled for it. But the version is read-only legacy — no producer writes
/// it, and summing its per-step `token_usage` over-counts (the erratum on its
/// page) — so a reader is told rather than left to find out from the totals.
/// `$ref` entries are not followed; only inline paths are inspected.
pub(crate) fn legacy_kind_warnings(doc: &Graph) -> Vec<String> {
    doc.paths
        .iter()
        .filter_map(|entry| match entry {
            PathOrRef::Path(path) => Some(path),
            PathOrRef::Ref(_) => None,
        })
        .filter_map(|path| {
            let kind = path.meta.as_ref()?.kind.as_deref()?;
            (kind == PATH_KIND_AGENT_CODING_SESSION_V1_0_0).then(|| {
                format!(
                    "path {} has kind {kind}: a read-only legacy version, not supported for \
                     writing; summing its per-step token_usage over-counts (see the erratum at \
                     that URI). Current producers write {PATH_KIND_AGENT_CODING_SESSION}.",
                    path.path.id
                )
            })
        })
        .collect()
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

    fn graph_with_kind(kind: Option<&str>) -> String {
        let meta = match kind {
            Some(k) => format!(r#","meta":{{"kind":"{k}"}}"#),
            None => String::new(),
        };
        format!(
            r#"{{"graph":{{"id":"g1"}},"paths":[{{"path":{{"id":"p-legacy","head":"s1"}}{meta},"steps":[{{"step":{{"id":"s1","actor":"human:alex","timestamp":"2026-01-01T00:00:00Z"}},"change":{{"src/main.rs":{{"raw":"@@ -1 +1 @@\n-a\n+b"}}}}}}]}}]}}"#
        )
    }

    /// A v1.0.0 document is still valid (the URI is immutable), but the
    /// reader is warned once per legacy path, by path id, naming the current
    /// kind to write instead.
    #[test]
    fn legacy_kind_warns_once_per_path_and_still_validates() {
        let json = graph_with_kind(Some(PATH_KIND_AGENT_CODING_SESSION_V1_0_0));
        let doc: Graph = serde_json::from_str(&json).unwrap();
        let warnings = legacy_kind_warnings(&doc);
        assert_eq!(
            warnings.len(),
            1,
            "one warning per legacy path: {warnings:?}"
        );
        assert!(
            warnings[0].contains("p-legacy"),
            "names the path: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains(PATH_KIND_AGENT_CODING_SESSION_V1_0_0),
            "names the legacy kind: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains(PATH_KIND_AGENT_CODING_SESSION),
            "names the current kind: {}",
            warnings[0]
        );

        let f = write_temp(&json, ".json");
        run(f.path().to_path_buf())
            .expect("a v1.0.0 document validates; it is warned about, not rejected");
    }

    #[test]
    fn current_kind_and_kindless_paths_draw_no_warning() {
        for kind in [Some(PATH_KIND_AGENT_CODING_SESSION), None] {
            let doc: Graph = serde_json::from_str(&graph_with_kind(kind)).unwrap();
            assert!(
                legacy_kind_warnings(&doc).is_empty(),
                "no warning for kind {kind:?}"
            );
        }
    }
}
