//! Both file formats, `.path.json` (the canonical Graph) and `.path.jsonl`
//! (a stream that seals to a single-path Graph), on every golden document.
//!
//! The RFC's promise: a `.path.jsonl` stream encodes exactly one inline Path
//! and seals to a single-path Graph at the file boundary, and a multi-path
//! Graph or a `$ref` entry has no JSONL form. Each golden's pinned
//! `document@<set>` is streamed to JSONL, sealed back, and compared with the
//! original in canonical form (keys sorted, no whitespace); `formats@<set>`
//! pins the stream's shape and the result. The committed `examples/*.path.jsonl`
//! are checked against their `.path.json` twins the same way, and the two
//! refusals are asserted.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use toolpath::v1::Graph;

const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/snapshots");
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");

/// Keys sorted at every level, then compact: the comparison form.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn canonical_bytes(g: &Graph) -> Vec<u8> {
    let v = serde_json::to_value(g).expect("graph serializes");
    serde_json::to_vec(&canonical(&v)).expect("value serializes")
}

/// Key paths whose values differ between `a` and `b` (array indices collapsed to `[]`).
fn differing(a: &Value, b: &Value, at: &str, out: &mut std::collections::BTreeSet<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let p = if at.is_empty() {
                    k.clone()
                } else {
                    format!("{at}.{k}")
                };
                match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => differing(l, r, &p, out),
                    _ => {
                        out.insert(p);
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (l, r) in x.iter().zip(y) {
                differing(l, r, &format!("{at}[]"), out);
            }
        }
        _ if a == b => {}
        _ => {
            out.insert(at.to_string());
        }
    }
}

/// The body of an insta snapshot: everything after the second `---` line.
fn snapshot_body(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("read snapshot");
    let mut parts = text.splitn(3, "---\n");
    parts.next();
    parts.next();
    parts.next().expect("snapshot has a body").to_string()
}

fn document_snapshots() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(SNAPSHOTS)
        .expect("read snapshots dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let set = name.strip_prefix("document@")?.strip_suffix(".snap")?;
            Some((set.to_string(), p))
        })
        .collect();
    out.sort();
    out
}

/// Stream `g` to JSONL, seal it back, compare in canonical form.
fn stream_and_seal(g: &Graph) -> Value {
    let jsonl = g
        .to_jsonl_string()
        .expect("a single-path graph has a JSONL form");
    let sealed = Graph::from_jsonl_str(&jsonl).expect("our own JSONL reads back");
    let mut tags: std::collections::BTreeMap<String, usize> = Default::default();
    for line in jsonl.lines() {
        let v: Value = serde_json::from_str(line).expect("each JSONL line is JSON");
        let tag = v
            .as_object()
            .and_then(|m| m.keys().next().cloned())
            .unwrap_or_default();
        *tags.entry(tag).or_default() += 1;
    }
    let (a, b) = (canonical_bytes(g), canonical_bytes(&sealed));
    let mut diff = std::collections::BTreeSet::new();
    if a != b {
        differing(
            &serde_json::to_value(g).unwrap(),
            &serde_json::to_value(&sealed).unwrap(),
            "",
            &mut diff,
        );
    }
    json!({
        "jsonl_lines": jsonl.lines().count(),
        "lines_by_tag": tags,
        "sealed_equals_json": a == b,
        "differing_key_paths": diff,
    })
}

#[test]
fn every_golden_document_streams_to_jsonl_and_seals_back_unchanged() {
    let docs = document_snapshots();
    assert_eq!(docs.len(), 13, "one document@<set> per golden set");
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.set_sort_maps(true);
    let mut unequal = Vec::new();
    settings.bind(|| {
        for (set, path) in &docs {
            let g = Graph::from_json(&snapshot_body(path))
                .unwrap_or_else(|e| panic!("document@{set} is a Graph: {e}"));
            let result = stream_and_seal(&g);
            if result["sealed_equals_json"] != json!(true) {
                unequal.push(format!("{set}: {}", result["differing_key_paths"]));
            }
            insta::assert_json_snapshot!(format!("formats@{set}"), result);
        }
    });
    assert!(
        unequal.is_empty(),
        "sealed .path.jsonl differs from .path.json:\n{}",
        unequal.join("\n")
    );
}

/// The committed `examples/*.path.jsonl` against their `.path.json` twins.
///
/// A stream carries no graph identity: sealing names the graph after its
/// path (`Graph::from_path`), while the committed twins name it
/// `graph-<path id>`. Everything else must match; the graph-id difference is
/// pinned in `formats@examples` rather than hidden.
#[test]
fn committed_jsonl_examples_seal_to_their_json_twins() {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(EXAMPLES)
        .expect("read examples")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".path.jsonl"))
        .collect();
    entries.sort();
    let mut rows = Vec::new();
    for jsonl in &entries {
        let twin = PathBuf::from(jsonl.to_string_lossy().replace(".path.jsonl", ".path.json"));
        let sealed = Graph::from_jsonl_str(&std::fs::read_to_string(jsonl).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", jsonl.display()));
        let json = Graph::from_json(&std::fs::read_to_string(&twin).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", twin.display()));
        let mut diff = std::collections::BTreeSet::new();
        differing(
            &serde_json::to_value(&json).unwrap(),
            &serde_json::to_value(&sealed).unwrap(),
            "",
            &mut diff,
        );
        let name = jsonl.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            diff.iter().all(|p| p == "graph.id"),
            "{name} differs from its .path.json twin beyond graph.id: {diff:?}"
        );
        rows.push(json!({
            "example": name,
            "differs_only_in": diff,
            "json_graph_id": json.graph.id,
            "sealed_graph_id": sealed.graph.id,
        }));
    }
    assert!(rows.len() >= 4, "the four path-0N examples have both forms");
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| insta::assert_json_snapshot!("formats@examples", rows));
}

#[test]
fn a_multi_path_graph_has_no_jsonl_form() {
    let release = std::fs::read_to_string(format!("{EXAMPLES}/graph-01-release.json")).unwrap();
    let g = Graph::from_json(&release).expect("graph-01-release parses");
    assert!(g.paths.len() > 1, "the release example holds several paths");
    let err = g
        .to_jsonl_string()
        .expect_err("multi-path graphs cannot stream");
    assert!(
        err.to_string().to_lowercase().contains("path"),
        "the refusal names the reason: {err}"
    );
}

#[test]
fn a_ref_only_graph_has_no_jsonl_form() {
    let g = Graph::from_json(
        r#"{"graph":{"id":"g-ref"},"paths":[{"$ref":"https://example.com/toolpath/path-44.path.json"}]}"#,
    )
    .expect("a $ref-only graph parses");
    g.to_jsonl_string()
        .expect_err("a $ref entry has no inline Path to stream");
}
