//! Engine behind `path query`: load cached (and off-cache) documents, wrap
//! each step in its source context, and run a jaq (jq) filter over the whole
//! array.
//!
//! The model is one idea: load every scoped step into a single JSON array and
//! transform it with a jaq filter. Selection, projection, ranking, grouping,
//! and top-N are all expressed in the filter; this module only decides *which*
//! documents load and hands jaq the array.

mod filter;
mod plan;

use anyhow::{Context, Result};
use jaq_json::Val;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path as FsPath, PathBuf};

use toolpath::v1::{Graph, Path, PathOrRef, query};

use crate::kinds::{self, KindSelector};

/// What to load and how to scope it. Mirrors the `path query` scope flags.
pub struct Scope {
    /// `--source`: keep cache entries whose id starts with `<name>-`.
    pub source: Option<String>,
    /// `--id`: load only these cache ids (repeatable).
    pub ids: Vec<String>,
    /// `--input`: off-cache files to load (`-` for stdin, repeatable).
    pub inputs: Vec<String>,
    /// `--project`: keep only paths whose `base` resolves to this directory.
    pub project: Option<PathBuf>,
    /// `--project-under`: keep only paths whose `base` lives under this
    /// directory (subtree match).
    pub project_under: Option<PathBuf>,
    /// `--kind`: keep only paths whose `meta.kind` matches this selector.
    pub kind: Option<String>,
}

/// Run `filter` over the scoped steps and print the result.
///
/// `filter` is jaq source (`.` emits the array verbatim).
/// `compact` forces single-line JSON; otherwise output is pretty-printed.
/// `raw` prints string results without JSON quoting (like `jq -r`).
///
/// The filter is analyzed once into a [`plan::Plan`]; the executor then streams
/// documents one at a time. An element-wise `.[] | g` filter prints as it goes
/// and holds nothing; a recognized aggregation (`map`, top-N, `length`) holds
/// only its per-file partials — the filter's own output, not the input cache.
/// Anything the planner can't prove decomposable falls back to the whole-array
/// path, which is still lean — the step values are held once, not re-serialized.
pub fn run(scope: &Scope, code: &str, compact: bool, raw: bool) -> Result<()> {
    let plan = plan::analyze(code);
    // Opt-in observability: `TOOLPATH_QUERY_EXPLAIN=1` reveals the execution
    // strategy on stderr. Not a behavioral flag — purely diagnostic.
    let explain = std::env::var("TOOLPATH_QUERY_EXPLAIN");
    if matches!(explain.as_deref(), Ok(v) if !v.is_empty() && v != "0") {
        eprintln!("query plan: {}", plan.describe());
    }
    // Buffer stdout: the streaming path prints one value per output, and a
    // raw `StdoutLock` is line-buffered (a syscall per line).
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    filter::execute(&plan, code, compact, raw, &mut out, |emit| {
        stream_files(scope, emit)
    })?;
    out.flush().context("flush stdout")
}

/// Where a document came from, and the `cache_id` to stamp on its steps.
struct DocSource {
    cache_id: String,
    location: SourceLoc,
    /// The user named this document explicitly (`--input`/`--id`), so a read
    /// or parse failure is an error — not the skip-with-warning that's right
    /// for a corrupt file encountered during the whole-cache scan.
    explicit: bool,
}

enum SourceLoc {
    File(PathBuf),
    Stdin,
}

impl DocSource {
    fn label(&self) -> String {
        match &self.location {
            SourceLoc::File(p) => p.display().to_string(),
            SourceLoc::Stdin => "<stdin>".to_string(),
        }
    }
}

/// Stream each selected, scoped document to `emit` as a jaq array value,
/// one file at a time. Only one document's `Graph` and step values are alive
/// per iteration; the executor decides how to combine them.
fn stream_files(scope: &Scope, emit: &mut dyn FnMut(Val) -> Result<()>) -> Result<()> {
    let kind_sel = scope.kind.as_deref().map(kinds::parse_kind_selector);
    let project = scope.project.as_deref().map(canonicalize_or_self);
    let project_under = scope.project_under.as_deref().map(canonicalize_or_self);

    for src in select_files(scope)? {
        let graph = match read_source(&src) {
            Ok(g) => g,
            // An explicitly named document that won't read/parse is a hard
            // error (a typo'd `--input`, bad stdin). A corrupt file merely
            // encountered while scanning the cache is skipped with a warning.
            Err(e) if src.explicit => {
                return Err(e.context(format!("read {}", src.label())));
            }
            Err(e) => {
                eprintln!("warning: skipping {}: {e:#}", src.label());
                continue;
            }
        };
        let mut steps = Vec::new();
        wrap_graph(
            &src,
            &graph,
            kind_sel.as_ref(),
            project.as_deref(),
            project_under.as_deref(),
            &mut steps,
        );
        drop(graph);
        emit(filter::steps_to_val(steps)?)?;
    }
    Ok(())
}

/// Resolve the scope's file-selection flags to a deterministic list of
/// documents to load.
///
/// The cache is read when no file selector restricts to off-cache inputs:
/// that is, when `--source`/`--id` is present, or when no `--input` is given
/// at all (the default whole-cache scan). `--input` files are appended in the
/// order given.
fn select_files(scope: &Scope) -> Result<Vec<DocSource>> {
    let mut sources = Vec::new();

    let restrict = scope.source.is_some() || !scope.ids.is_empty();
    let load_cache = restrict || scope.inputs.is_empty();
    if load_cache {
        let id_set: Option<HashSet<&str>> = if scope.ids.is_empty() {
            None
        } else {
            Some(scope.ids.iter().map(String::as_str).collect())
        };
        let prefix = scope.source.as_ref().map(|s| format!("{s}-"));
        // `--id` names documents explicitly, so a corrupt one is an error, and
        // a requested id that doesn't exist must be reported, not silently
        // dropped. A `--source`/default scan is not explicit (skip-warn).
        let by_id = id_set.is_some();
        let mut seen_ids: HashSet<String> = HashSet::new();
        for entry in crate::cache::list_cached()? {
            if let Some(ids) = &id_set
                && !ids.contains(entry.id.as_str())
            {
                continue;
            }
            // The id exists in the cache — record that *before* the source
            // filter, so `--id X --source Y` where X isn't a Y-document yields
            // an empty intersection, not a false "no cached document" error.
            seen_ids.insert(entry.id.clone());
            if let Some(p) = &prefix
                && !entry.id.starts_with(p.as_str())
            {
                continue;
            }
            sources.push(DocSource {
                cache_id: entry.id,
                location: SourceLoc::File(entry.path),
                explicit: by_id,
            });
        }
        // Every requested `--id` must have matched a cached document.
        for id in &scope.ids {
            if !seen_ids.contains(id) {
                anyhow::bail!(
                    "no cached document with id `{id}`; run `path p cache ls` to see what's cached"
                );
            }
        }
    }

    for inp in &scope.inputs {
        if inp == "-" {
            sources.push(DocSource {
                cache_id: "stdin".to_string(),
                location: SourceLoc::Stdin,
                explicit: true,
            });
        } else {
            // The full path as given, so two inputs sharing a basename
            // (`proj1/doc.json`, `proj2/doc.json`) keep distinct cache_ids and
            // the identity triple (cache_id, path.id, step.id) stays unique.
            sources.push(DocSource {
                cache_id: inp.clone(),
                location: SourceLoc::File(PathBuf::from(inp)),
                explicit: true,
            });
        }
    }

    Ok(sources)
}

fn read_source(src: &DocSource) -> Result<Graph> {
    match &src.location {
        SourceLoc::File(p) => crate::io::read_document_auto(p),
        SourceLoc::Stdin => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read stdin")?;
            // No filename to route on, so accept either canonical JSON or the
            // JSONL (`.path.jsonl`) form — matching what `--input <file>` does.
            // On double failure report both parse errors; hiding the JSON one
            // would misdiagnose malformed JSON as a JSONL framing problem.
            match Graph::from_json(&s) {
                Ok(g) => Ok(g),
                Err(json_err) => Graph::from_jsonl_str(&s).map_err(|jsonl_err| {
                    anyhow::anyhow!(
                        "stdin is neither toolpath JSON ({json_err}) nor JSONL ({jsonl_err})"
                    )
                }),
            }
        }
    }
}

/// Walk a graph's inline paths, apply content scoping, and wrap surviving
/// steps into `out`.
fn wrap_graph(
    src: &DocSource,
    graph: &Graph,
    kind_sel: Option<&KindSelector>,
    project: Option<&FsPath>,
    project_under: Option<&FsPath>,
    out: &mut Vec<serde_json::Value>,
) {
    for entry in &graph.paths {
        let PathOrRef::Path(path) = entry else {
            continue;
        };

        if let Some(sel) = kind_sel {
            let kind = path.meta.as_ref().and_then(|m| m.kind.as_deref());
            if !kind.is_some_and(|k| sel.matches_uri(k)) {
                continue;
            }
        }
        if let Some(proj) = project
            && !path_matches_project(path, proj)
        {
            continue;
        }
        if let Some(dir) = project_under
            && !path_matches_project_under(path, dir)
        {
            continue;
        }

        wrap_path(src, path, out);
    }
}

/// Wrap every step of one path, computing the dead-end set once.
fn wrap_path(src: &DocSource, path: &Path, out: &mut Vec<serde_json::Value>) {
    let dead: HashSet<&str> = query::dead_ends(&path.steps, &path.path.head)
        .into_iter()
        .map(|s| s.step.id.as_str())
        .collect();

    let path_ctx = path_context(path);

    for step in &path.steps {
        // A Step serializes to `{"step": …, "change": …, "meta"?: …}`; we add
        // the three wrapper keys (`cache_id`, `path`, `dead_end`) alongside.
        let serde_json::Value::Object(mut obj) = serde_json::to_value(step).unwrap_or_default()
        else {
            continue;
        };
        obj.insert(
            "cache_id".to_string(),
            serde_json::Value::String(src.cache_id.clone()),
        );
        obj.insert("path".to_string(), path_ctx.clone());
        obj.insert(
            "dead_end".to_string(),
            serde_json::Value::Bool(dead.contains(step.step.id.as_str())),
        );
        out.push(serde_json::Value::Object(obj));
    }
}

/// The `path` context attached to every step: the parent path's `id`, `base`,
/// and `meta`.
fn path_context(path: &Path) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(
        "id".to_string(),
        serde_json::Value::String(path.path.id.clone()),
    );
    if let Some(base) = &path.path.base
        && let Ok(v) = serde_json::to_value(base)
    {
        m.insert("base".to_string(), v);
    }
    if let Some(meta) = &path.meta
        && let Ok(v) = serde_json::to_value(meta)
    {
        m.insert("meta".to_string(), v);
    }
    serde_json::Value::Object(m)
}

/// Whether a path's `base` resolves to `project` (a canonicalized directory).
/// Only `file://` bases can match; VCS bases (`github:…`) never do.
fn path_matches_project(path: &Path, project: &FsPath) -> bool {
    base_fs_path(path).is_some_and(|p| p == project)
}

fn path_matches_project_under(path: &Path, project_under: &FsPath) -> bool {
    base_fs_path(path).is_some_and(|p| p.starts_with(project_under))
}

fn base_fs_path(path: &Path) -> Option<PathBuf> {
    let base = path.path.base.as_ref()?;
    let fs = base.uri.strip_prefix("file://")?;
    Some(canonicalize_or_self(FsPath::new(fs)))
}

fn canonicalize_or_self(p: &FsPath) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath::v1::{Base, Graph, Path, PathIdentity, PathMeta, Step};

    fn doc_src(id: &str) -> DocSource {
        DocSource {
            cache_id: id.to_string(),
            location: SourceLoc::Stdin,
            explicit: false,
        }
    }

    /// A path with a fork: s1 → {s2 → s3 (head), s2a (dead end)}.
    fn forked_path() -> Path {
        let s1 = Step::new("s1", "human:alex", "2026-01-01T10:00:00Z")
            .with_raw_change("src/main.rs", "@@");
        let s2 = Step::new("s2", "agent:claude", "2026-01-01T11:00:00Z")
            .with_parent("s1")
            .with_raw_change("src/lib.rs", "@@");
        let s2a = Step::new("s2a", "agent:claude", "2026-01-01T11:30:00Z")
            .with_parent("s1")
            .with_raw_change("src/dead.rs", "@@");
        let s3 = Step::new("s3", "human:alex", "2026-01-01T12:00:00Z")
            .with_parent("s2")
            .with_raw_change("src/main.rs", "@@");
        Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("file:///work/repo", "abc")),
                head: "s3".into(),
                graph_ref: None,
            },
            steps: vec![s1, s2, s2a, s3],
            meta: Some(PathMeta {
                kind: Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION.to_string()),
                source: Some("claude".to_string()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn wraps_step_verbatim_with_context() {
        let path = forked_path();
        let mut out = Vec::new();
        wrap_path(&doc_src("claude-abc"), &path, &mut out);

        assert_eq!(out.len(), 4);
        let first = &out[0];
        // Wrapper keys present.
        assert_eq!(first["cache_id"], "claude-abc");
        assert_eq!(first["path"]["id"], "p1");
        assert_eq!(first["path"]["meta"]["source"], "claude");
        // Step body verbatim under `step`/`change`.
        assert_eq!(first["step"]["id"], "s1");
        assert_eq!(first["step"]["actor"], "human:alex");
        assert!(first["change"]["src/main.rs"]["raw"].is_string());
    }

    #[test]
    fn dead_end_flag_tracks_ancestry_of_head() {
        let path = forked_path();
        let mut out = Vec::new();
        wrap_path(&doc_src("g"), &path, &mut out);

        let dead: std::collections::HashMap<&str, bool> = out
            .iter()
            .map(|e| {
                (
                    e["step"]["id"].as_str().unwrap(),
                    e["dead_end"].as_bool().unwrap(),
                )
            })
            .collect();
        assert!(!dead["s1"]);
        assert!(!dead["s2"]);
        assert!(!dead["s3"]);
        assert!(dead["s2a"], "s2a is off the head's ancestry");
    }

    #[test]
    fn wrap_graph_filters_by_kind() {
        let graph = Graph::from_path(forked_path());
        let mut out = Vec::new();
        let sel = kinds::parse_kind_selector(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION);
        wrap_graph(&doc_src("g"), &graph, Some(&sel), None, None, &mut out);
        assert_eq!(out.len(), 4, "matching kind keeps all steps");

        out.clear();
        let miss = kinds::parse_kind_selector("agent-coding-session/v2");
        wrap_graph(&doc_src("g"), &graph, Some(&miss), None, None, &mut out);
        assert!(out.is_empty(), "non-matching kind drops the whole path");
    }

    #[test]
    fn project_matches_file_base_only() {
        let path = forked_path(); // base uri = file:///work/repo
        assert!(path_matches_project(
            &path,
            &canonicalize_or_self(FsPath::new("/work/repo"))
        ));
        assert!(!path_matches_project(
            &path,
            &canonicalize_or_self(FsPath::new("/other"))
        ));

        let mut vcs = forked_path();
        vcs.path.base = Some(Base::vcs("github:org/repo", "abc"));
        assert!(!path_matches_project(
            &vcs,
            &canonicalize_or_self(FsPath::new("/work/repo"))
        ));
    }

    // The cache-scanning branch of `select_files` (whole-cache, `--source`,
    // `--id`) reads the global config dir; it's covered end-to-end and
    // hermetically by `tests/query.rs` (per-process `$TOOLPATH_CONFIG_DIR`),
    // so we don't mutate process-global env in a unit test here.

    #[test]
    fn select_files_input_only_skips_cache() {
        let scope = Scope {
            source: None,
            ids: vec![],
            inputs: vec!["/tmp/some.json".to_string(), "-".to_string()],
            project: None,
            project_under: None,
            kind: None,
        };
        let files = select_files(&scope).unwrap();
        assert_eq!(files.len(), 2);
        // The full path as given: inputs sharing a basename stay distinct.
        assert_eq!(files[0].cache_id, "/tmp/some.json");
        assert!(matches!(files[1].location, SourceLoc::Stdin));
        assert_eq!(files[1].cache_id, "stdin");
    }
}
