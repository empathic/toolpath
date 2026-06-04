//! JSONL streaming format for single-path Toolpath documents.
//!
//! A `.path.jsonl` file is a line-oriented sequence of self-describing JSON
//! objects that seals to a single-path [`Graph`]. On the wire, each line is
//! an externally tagged JSON object: `{"<Variant>": <body>}`. Writers append
//! one line per logical event (path open, new step, new signature, etc.);
//! readers accumulate these into the inner [`Path`], then wrap it as a
//! single-path [`Graph`] at the file boundary.
//!
//! See `docs/RFC-jsonl.md` for the full specification. Round-trip guarantee:
//! reading a JSONL file and writing it back produces a JSON document
//! equivalent to what [`Graph::to_json`] would produce for the same logical
//! graph — signatures computed over canonical JSON remain valid across
//! conversions.
//!
//! # Reading
//!
//! ```
//! use toolpath::v1::Graph;
//!
//! let jsonl = concat!(
//!     r#"{"PathOpen":{"version":"1","id":"pr-42","base":{"uri":"github:org/repo","ref":"abc"}}}"#, "\n",
//!     r#"{"Step":{"step":{"id":"s1","actor":"human:alex","timestamp":"2026-01-01T00:00:00Z"},"change":{"f.rs":{"raw":"@@"}}}}"#, "\n",
//!     r#"{"Head":{"step_id":"s1"}}"#, "\n",
//!     r#"{"PathClose":{}}"#, "\n",
//! );
//! let graph = Graph::from_jsonl_str(jsonl).unwrap();
//! let path = graph.single_path().expect("single-path graph");
//! assert_eq!(path.path.id, "pr-42");
//! assert_eq!(path.path.head, "s1");
//! assert_eq!(path.steps.len(), 1);
//! ```
//!
//! # Writing
//!
//! ```
//! use toolpath::v1::{Base, Graph, Path, PathIdentity, Step};
//!
//! let step = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z")
//!     .with_raw_change("f.rs", "@@ -0,0 +1 @@\n+hi");
//! let path = Path {
//!     path: PathIdentity {
//!         id: "p1".into(),
//!         base: Some(Base::vcs("github:org/repo", "abc")),
//!         head: "s1".into(),
//!         graph_ref: None,
//!     },
//!     steps: vec![step],
//!     meta: None,
//! };
//! let graph = Graph::from_path(path);
//! let jsonl = graph.to_jsonl_string().unwrap();
//! let first_line = jsonl.lines().next().unwrap();
//! assert!(first_line.starts_with(r#"{"PathOpen":"#));
//! ```

use crate::types::{
    ActorDefinition, Base, Graph, Path, PathIdentity, PathMeta, PathOrRef, Ref, Signature, Step,
    StepMeta,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, Write};

// ============================================================================
// Line kind types
// ============================================================================

/// One line of a `.path.jsonl` file. Externally tagged to match the RFC
/// wire format: `{"<Variant>": <body>}` per line, compact JSON,
/// LF-terminated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JsonlLine {
    PathOpen(PathOpenBody),
    Step(StepBody),
    ActorDef(ActorDefBody),
    Signature(SignatureBody),
    PathMeta(PathMetaBody),
    Head(HeadBody),
    PathClose(PathCloseBody),
}

/// Body of a `PathOpen` line — the first line of every file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathOpenBody {
    pub version: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<PathOpenMeta>,
}

/// `PathMeta` fields allowed inside `PathOpen.meta`. Excludes `actors` and
/// `signatures` (those have dedicated line kinds per the RFC).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathOpenMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Body of a `Step` line — the existing `Step` JSON shape verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepBody(pub Step);

/// Body of an `ActorDef` line. Inserts or overwrites an entry in
/// `path.meta.actors`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorDefBody {
    pub actor: String,
    pub definition: ActorDefinition,
}

/// Body of a `Signature` line. `target` is either `"path"` or
/// `"step:<step-id>"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureBody {
    pub target: String,
    pub signature: Signature,
}

/// Body of a `PathMeta` line. Fields in `patch` field-wise overwrite
/// `path.meta`. Arrays (via `Option<Vec<_>>`) replace; absent fields are
/// unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathMetaBody {
    pub patch: PathMetaPatch,
}

/// Patch body for `PathMeta` lines. Scalar fields overwrite when `Some`;
/// `refs` replaces when `Some` (empty `Vec` means "replace with empty").
/// `actors` and `signatures` are not representable — use dedicated
/// [`ActorDefBody`] / [`SignatureBody`] lines.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathMetaPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<Ref>>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Body of a `Head` line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadBody {
    pub step_id: String,
}

/// Body of a `PathClose` line (optional terminator; currently empty).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathCloseBody {}

// ============================================================================
// Errors
// ============================================================================

/// Errors produced while reading or writing a `.path.jsonl` stream.
#[derive(Debug)]
pub enum JsonlError {
    /// Underlying I/O failure.
    Io(std::io::Error),
    /// The stream was empty; a `PathOpen` line is required.
    Empty,
    /// The first line was not a `PathOpen` line.
    FirstLineNotPathOpen { line_num: usize },
    /// A second `PathOpen` appeared mid-stream.
    DuplicatePathOpen { line_num: usize },
    /// A line was not parseable JSON.
    MalformedJson {
        line_num: usize,
        source: serde_json::Error,
    },
    /// A line was valid JSON but not a JSON object.
    NotAnObject { line_num: usize },
    /// A line was an object with zero keys or more than one key.
    NotSingleKey { line_num: usize },
    /// A line had a known tag but the body failed to deserialize.
    BadBody {
        line_num: usize,
        tag: String,
        source: serde_json::Error,
    },
    /// A `Signature` line targeted a step that had not appeared yet.
    OrphanStepSignature { line_num: usize, step_id: String },
    /// EOF reached with no `Head` line and head cannot be unambiguously
    /// inferred.
    AmbiguousHead { candidates: Vec<String> },
    /// EOF reached with no steps and no `Head`.
    NoSteps,
    /// A line appeared after `PathClose`.
    AfterClose { line_num: usize },
    /// JSONL only encodes single-path graphs; the source graph held zero or
    /// more than one inline path.
    NotSinglePathGraph { path_count: usize },
}

impl fmt::Display for JsonlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonlError::Io(e) => write!(f, "I/O error: {e}"),
            JsonlError::Empty => write!(f, "empty JSONL stream (expected a PathOpen line)"),
            JsonlError::FirstLineNotPathOpen { line_num } => {
                write!(f, "line {line_num}: first line must be a PathOpen")
            }
            JsonlError::DuplicatePathOpen { line_num } => {
                write!(f, "line {line_num}: duplicate PathOpen mid-stream")
            }
            JsonlError::MalformedJson { line_num, source } => {
                write!(f, "line {line_num}: malformed JSON: {source}")
            }
            JsonlError::NotAnObject { line_num } => {
                write!(f, "line {line_num}: expected a JSON object")
            }
            JsonlError::NotSingleKey { line_num } => write!(
                f,
                "line {line_num}: expected a single-key object {{\"<Variant>\": ...}}"
            ),
            JsonlError::BadBody {
                line_num,
                tag,
                source,
            } => write!(f, "line {line_num}: invalid body for {tag}: {source}"),
            JsonlError::OrphanStepSignature { line_num, step_id } => write!(
                f,
                "line {line_num}: Signature targets step {step_id:?} which has not appeared"
            ),
            JsonlError::AmbiguousHead { candidates } => write!(
                f,
                "no Head line and head cannot be inferred (candidates: {candidates:?})"
            ),
            JsonlError::NoSteps => write!(f, "no Head line and no steps in file"),
            JsonlError::AfterClose { line_num } => {
                write!(f, "line {line_num}: unexpected line after PathClose")
            }
            JsonlError::NotSinglePathGraph { path_count } => write!(
                f,
                "JSONL only encodes single-path graphs (got {path_count} paths)"
            ),
        }
    }
}

impl std::error::Error for JsonlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JsonlError::Io(e) => Some(e),
            JsonlError::MalformedJson { source, .. } | JsonlError::BadBody { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

impl From<std::io::Error> for JsonlError {
    fn from(e: std::io::Error) -> Self {
        JsonlError::Io(e)
    }
}

// ============================================================================
// Reader
// ============================================================================

/// Internal per-line parse result. Each value is consumed immediately by the
/// reader's match block — never stored — so the size difference between
/// variants is irrelevant here.
#[allow(clippy::large_enum_variant)]
enum ParsedLine {
    Known(JsonlLine),
    Unknown { tag: String },
}

fn parse_line(line: &str, line_num: usize) -> Result<ParsedLine, JsonlError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|source| JsonlError::MalformedJson { line_num, source })?;
    let obj = value
        .as_object()
        .ok_or(JsonlError::NotAnObject { line_num })?;
    if obj.len() != 1 {
        return Err(JsonlError::NotSingleKey { line_num });
    }
    let tag = obj.keys().next().cloned().unwrap();

    match tag.as_str() {
        "PathOpen" | "Step" | "ActorDef" | "Signature" | "PathMeta" | "Head" | "PathClose" => {
            let line_obj = serde_json::from_value::<JsonlLine>(value).map_err(|source| {
                JsonlError::BadBody {
                    line_num,
                    tag: tag.clone(),
                    source,
                }
            })?;
            Ok(ParsedLine::Known(line_obj))
        }
        _ => Ok(ParsedLine::Unknown { tag }),
    }
}

impl Path {
    /// Read a JSONL `Path` document from any buffered reader.
    ///
    /// Each line is parsed as a [`JsonlLine`]; the reader accumulates them
    /// per the RFC algorithm and returns the sealed `Path` at EOF.
    ///
    /// Unknown line variants are skipped with a warning to stderr. Malformed
    /// JSON, a first-line that is not `PathOpen`, a duplicate `PathOpen`, a
    /// `Signature` targeting an unknown step, or an EOF with an ambiguous
    /// head are all fatal.
    pub fn from_jsonl_reader<R: BufRead>(reader: R) -> Result<Self, JsonlError> {
        let mut lines_iter = reader.lines().enumerate();

        // Line 1 MUST be PathOpen.
        let (path_id, base, graph_ref, mut path_meta) = loop {
            match lines_iter.next() {
                None => return Err(JsonlError::Empty),
                Some((idx, io_res)) => {
                    let line = io_res?;
                    if line.is_empty() {
                        // Tolerate an initial empty line (e.g. blank first
                        // line from a file with a BOM-stripped start or a
                        // concatenated stream) to stay forgiving. The RFC
                        // does not require first-line-PathOpen to be
                        // byte-offset 0, only the first non-empty line.
                        continue;
                    }
                    let line_num = idx + 1;
                    match parse_line(&line, line_num)? {
                        ParsedLine::Known(JsonlLine::PathOpen(po)) => {
                            let mut meta = PathMeta::default();
                            if let Some(m) = po.meta {
                                meta.title = m.title;
                                meta.kind = m.kind;
                                meta.source = m.source;
                                meta.intent = m.intent;
                                meta.refs = m.refs;
                                meta.extra = m.extra;
                            }
                            break (po.id, po.base, po.graph_ref, meta);
                        }
                        ParsedLine::Known(_) | ParsedLine::Unknown { .. } => {
                            return Err(JsonlError::FirstLineNotPathOpen { line_num });
                        }
                    }
                }
            }
        };

        let mut steps: Vec<Step> = Vec::new();
        let mut step_idx: HashMap<String, usize> = HashMap::new();
        let mut head: Option<String> = None;
        let mut closed = false;

        for (idx, io_res) in lines_iter {
            let line = io_res?;
            if line.is_empty() {
                continue;
            }
            let line_num = idx + 1;
            if closed {
                return Err(JsonlError::AfterClose { line_num });
            }

            match parse_line(&line, line_num)? {
                ParsedLine::Known(JsonlLine::PathOpen(_)) => {
                    return Err(JsonlError::DuplicatePathOpen { line_num });
                }
                ParsedLine::Known(JsonlLine::Step(StepBody(step))) => {
                    let id = step.step.id.clone();
                    step_idx.insert(id, steps.len());
                    steps.push(step);
                }
                ParsedLine::Known(JsonlLine::ActorDef(body)) => {
                    let actors = path_meta.actors.get_or_insert_with(HashMap::new);
                    actors.insert(body.actor, body.definition);
                }
                ParsedLine::Known(JsonlLine::Signature(body)) => {
                    apply_signature(&mut path_meta, &mut steps, &step_idx, body, line_num)?;
                }
                ParsedLine::Known(JsonlLine::PathMeta(body)) => {
                    apply_meta_patch(&mut path_meta, body.patch);
                }
                ParsedLine::Known(JsonlLine::Head(body)) => {
                    head = Some(body.step_id);
                }
                ParsedLine::Known(JsonlLine::PathClose(_)) => {
                    closed = true;
                }
                ParsedLine::Unknown { tag } => {
                    eprintln!(
                        "toolpath::jsonl: line {line_num}: unknown variant {tag:?}, skipping"
                    );
                }
            }
        }

        let head = resolve_head(head, &steps)?;
        let meta = if path_meta_is_empty(&path_meta) {
            None
        } else {
            Some(path_meta)
        };
        Ok(Path {
            path: PathIdentity {
                id: path_id,
                base,
                head,
                graph_ref,
            },
            steps,
            meta,
        })
    }

    /// Read a JSONL `Path` document from a string.
    pub fn from_jsonl_str(s: &str) -> Result<Self, JsonlError> {
        Self::from_jsonl_reader(std::io::Cursor::new(s))
    }
}

impl Graph {
    /// Read a JSONL toolpath document from any buffered reader. The stream is
    /// parsed as a single inline [`Path`] and wrapped in a single-path
    /// [`Graph`].
    pub fn from_jsonl_reader<R: BufRead>(reader: R) -> Result<Self, JsonlError> {
        let path = Path::from_jsonl_reader(reader)?;
        Ok(Graph::from_path(path))
    }

    /// Read a JSONL toolpath document from a string.
    pub fn from_jsonl_str(s: &str) -> Result<Self, JsonlError> {
        Self::from_jsonl_reader(std::io::Cursor::new(s))
    }

    /// Write the graph as JSONL. Errors with [`JsonlError::NotSinglePathGraph`]
    /// if the graph does not hold exactly one inline path — JSONL has no
    /// representation for multi-path graphs or `$ref` entries.
    pub fn to_jsonl_writer<W: Write>(&self, w: &mut W) -> Result<(), JsonlError> {
        match self.paths.as_slice() {
            [PathOrRef::Path(p)] => p.to_jsonl_writer(w),
            other => Err(JsonlError::NotSinglePathGraph {
                path_count: other.len(),
            }),
        }
    }

    /// Write the graph as JSONL into a string.
    pub fn to_jsonl_string(&self) -> Result<String, JsonlError> {
        let mut buf: Vec<u8> = Vec::new();
        self.to_jsonl_writer(&mut buf)?;
        Ok(String::from_utf8(buf).expect("jsonl writer emits utf-8"))
    }
}

fn apply_signature(
    path_meta: &mut PathMeta,
    steps: &mut [Step],
    step_idx: &HashMap<String, usize>,
    body: SignatureBody,
    line_num: usize,
) -> Result<(), JsonlError> {
    if body.target == "path" {
        path_meta.signatures.push(body.signature);
        return Ok(());
    }
    if let Some(step_id) = body.target.strip_prefix("step:") {
        let idx =
            step_idx
                .get(step_id)
                .copied()
                .ok_or_else(|| JsonlError::OrphanStepSignature {
                    line_num,
                    step_id: step_id.to_string(),
                })?;
        let step = &mut steps[idx];
        let meta = step.meta.get_or_insert_with(StepMeta::default);
        meta.signatures.push(body.signature);
        return Ok(());
    }
    // Unknown target form — treat as orphan for strict parsing.
    Err(JsonlError::OrphanStepSignature {
        line_num,
        step_id: body.target,
    })
}

fn apply_meta_patch(path_meta: &mut PathMeta, patch: PathMetaPatch) {
    if let Some(v) = patch.title {
        path_meta.title = Some(v);
    }
    if let Some(v) = patch.kind {
        path_meta.kind = Some(v);
    }
    if let Some(v) = patch.source {
        path_meta.source = Some(v);
    }
    if let Some(v) = patch.intent {
        path_meta.intent = Some(v);
    }
    if let Some(v) = patch.refs {
        path_meta.refs = v;
    }
    for (k, v) in patch.extra {
        path_meta.extra.insert(k, v);
    }
}

fn resolve_head(explicit: Option<String>, steps: &[Step]) -> Result<String, JsonlError> {
    if let Some(h) = explicit {
        return Ok(h);
    }
    if steps.is_empty() {
        return Err(JsonlError::NoSteps);
    }
    let mut referenced: HashSet<&str> = HashSet::new();
    for s in steps {
        for p in &s.step.parents {
            referenced.insert(p.as_str());
        }
    }
    let candidates: Vec<String> = steps
        .iter()
        .map(|s| s.step.id.as_str())
        .filter(|id| !referenced.contains(*id))
        .map(str::to_string)
        .collect();
    if candidates.len() == 1 {
        Ok(candidates.into_iter().next().unwrap())
    } else {
        Err(JsonlError::AmbiguousHead { candidates })
    }
}

fn path_meta_is_empty(m: &PathMeta) -> bool {
    m.title.is_none()
        && m.kind.is_none()
        && m.source.is_none()
        && m.intent.is_none()
        && m.refs.is_empty()
        && m.actors.as_ref().is_none_or(|a| a.is_empty())
        && m.signatures.is_empty()
        && m.extra.is_empty()
}

// ============================================================================
// Writer
// ============================================================================

impl Path {
    /// Write the normalized JSONL form of this path to any writer.
    ///
    /// Emission order matches the RFC: `PathOpen`, then `ActorDef` entries
    /// sorted by actor key for determinism, then each `Step` followed by its
    /// step-level `Signature` lines in original order, then path-level
    /// `Signature` lines in original order, then `Head`, then `PathClose`.
    /// Each line is compact JSON followed by a single `\n`.
    pub fn to_jsonl_writer<W: Write>(&self, w: &mut W) -> Result<(), JsonlError> {
        // PathOpen
        let open_meta = self.meta.as_ref().and_then(path_meta_for_open);
        let open = JsonlLine::PathOpen(PathOpenBody {
            version: "1".to_string(),
            id: self.path.id.clone(),
            base: self.path.base.clone(),
            graph_ref: self.path.graph_ref.clone(),
            meta: open_meta,
        });
        write_line(w, &open)?;

        // ActorDef lines, sorted by actor key for determinism
        if let Some(actors) = self.meta.as_ref().and_then(|m| m.actors.as_ref()) {
            let sorted: BTreeMap<&String, &ActorDefinition> = actors.iter().collect();
            for (actor, def) in sorted {
                let line = JsonlLine::ActorDef(ActorDefBody {
                    actor: actor.clone(),
                    definition: def.clone(),
                });
                write_line(w, &line)?;
            }
        }

        // Steps + per-step signatures
        for step in &self.steps {
            let mut trimmed = step.clone();
            let step_sigs: Vec<Signature> = trimmed
                .meta
                .as_mut()
                .map(|m| std::mem::take(&mut m.signatures))
                .unwrap_or_default();
            if let Some(m) = trimmed.meta.as_ref()
                && step_meta_is_empty(m)
            {
                trimmed.meta = None;
            }
            let line = JsonlLine::Step(StepBody(trimmed));
            write_line(w, &line)?;
            for sig in step_sigs {
                let sig_line = JsonlLine::Signature(SignatureBody {
                    target: format!("step:{}", step.step.id),
                    signature: sig,
                });
                write_line(w, &sig_line)?;
            }
        }

        // Path-level signatures
        if let Some(meta) = &self.meta {
            for sig in &meta.signatures {
                let line = JsonlLine::Signature(SignatureBody {
                    target: "path".to_string(),
                    signature: sig.clone(),
                });
                write_line(w, &line)?;
            }
        }

        // Head
        write_line(
            w,
            &JsonlLine::Head(HeadBody {
                step_id: self.path.head.clone(),
            }),
        )?;
        // PathClose
        write_line(w, &JsonlLine::PathClose(PathCloseBody {}))?;
        Ok(())
    }

    /// Write the normalized JSONL form of this path to a string.
    pub fn to_jsonl_string(&self) -> Result<String, JsonlError> {
        let mut buf: Vec<u8> = Vec::new();
        self.to_jsonl_writer(&mut buf)?;
        Ok(String::from_utf8(buf).expect("jsonl writer emits utf-8"))
    }
}

fn write_line<W: Write>(w: &mut W, line: &JsonlLine) -> Result<(), JsonlError> {
    let s = serde_json::to_string(line).map_err(|e| JsonlError::BadBody {
        line_num: 0,
        tag: "<writer>".into(),
        source: e,
    })?;
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    Ok(())
}

fn step_meta_is_empty(m: &StepMeta) -> bool {
    m.intent.is_none()
        && m.source.is_none()
        && m.refs.is_empty()
        && m.actors.as_ref().is_none_or(|a| a.is_empty())
        && m.signatures.is_empty()
        && m.extra.is_empty()
}

/// Project the parts of `PathMeta` that may be carried in `PathOpen.meta`.
/// Returns `None` if the projection would be empty.
fn path_meta_for_open(m: &PathMeta) -> Option<PathOpenMeta> {
    let open = PathOpenMeta {
        title: m.title.clone(),
        kind: m.kind.clone(),
        source: m.source.clone(),
        intent: m.intent.clone(),
        refs: m.refs.clone(),
        extra: m.extra.clone(),
    };
    if open.title.is_none()
        && open.kind.is_none()
        && open.source.is_none()
        && open.intent.is_none()
        && open.refs.is_empty()
        && open.extra.is_empty()
    {
        None
    } else {
        Some(open)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArtifactChange, Ref};
    use serde_json::json;
    use std::collections::HashMap;

    fn make_step(id: &str, parent: Option<&str>) -> Step {
        let mut s = Step::new(id, "human:alex", "2026-01-01T00:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-a\n+b");
        if let Some(p) = parent {
            s = s.with_parent(p);
        }
        s
    }

    fn canonical_json(path: &Path) -> serde_json::Value {
        serde_json::to_value(path).unwrap()
    }

    // ── Line kind serde ────────────────────────────────────────────────────

    #[test]
    fn path_open_serde_wire_shape() {
        let wire = r#"{"PathOpen":{"version":"1","id":"pr-42","base":{"uri":"github:org/repo","ref":"abc"},"meta":{"title":"T"}}}"#;
        let line: JsonlLine = serde_json::from_str(wire).unwrap();
        let back = serde_json::to_string(&line).unwrap();
        // Field ordering may differ; compare JSON values.
        let a: serde_json::Value = serde_json::from_str(wire).unwrap();
        let b: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn step_body_is_transparent() {
        let step = make_step("s1", None);
        let body = StepBody(step.clone());
        let wire = serde_json::to_value(&body).unwrap();
        let direct = serde_json::to_value(&step).unwrap();
        assert_eq!(wire, direct);
    }

    #[test]
    fn head_body_serde() {
        let wire = r#"{"Head":{"step_id":"s5"}}"#;
        let line: JsonlLine = serde_json::from_str(wire).unwrap();
        assert!(matches!(line, JsonlLine::Head(HeadBody { ref step_id }) if step_id == "s5"));
    }

    #[test]
    fn path_close_empty_object() {
        let wire = r#"{"PathClose":{}}"#;
        let line: JsonlLine = serde_json::from_str(wire).unwrap();
        assert!(matches!(line, JsonlLine::PathClose(_)));
    }

    // ── Reader: error paths ────────────────────────────────────────────────

    #[test]
    fn reader_empty_stream_fatal() {
        let err = Path::from_jsonl_str("").unwrap_err();
        assert!(matches!(err, JsonlError::Empty));
    }

    #[test]
    fn reader_first_line_not_path_open_fatal() {
        // First line has a valid body but wrong variant — FirstLineNotPathOpen.
        let err = Path::from_jsonl_str(r#"{"Head":{"step_id":"s1"}}"#).unwrap_err();
        assert!(matches!(err, JsonlError::FirstLineNotPathOpen { .. }));
    }

    #[test]
    fn reader_first_line_malformed_step_body_is_bad_body() {
        // `{"Step":{}}` has a known tag but a body that can't deserialize,
        // so the error is `BadBody`, not `FirstLineNotPathOpen`.
        let err = Path::from_jsonl_str(r#"{"Step":{}}"#).unwrap_err();
        assert!(matches!(err, JsonlError::BadBody { .. }));
    }

    #[test]
    fn reader_malformed_json_fatal() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            "not json at all\n",
        );
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::MalformedJson { line_num: 2, .. }));
    }

    #[test]
    fn reader_duplicate_path_open_fatal() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"PathOpen":{"version":"1","id":"p2"}}"#,
            "\n",
        );
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::DuplicatePathOpen { line_num: 3 }));
    }

    #[test]
    fn reader_orphan_step_signature_fatal() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Signature":{"target":"step:nope","signature":{"signer":"s","key":"k","scope":"author","sig":"x"}}}"#,
            "\n",
        );
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::OrphanStepSignature { .. }));
    }

    #[test]
    fn reader_ambiguous_head_fatal() {
        // Two steps, neither is a parent of the other → two candidates.
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s2","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
        );
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::AmbiguousHead { .. }));
    }

    #[test]
    fn reader_no_steps_fatal() {
        let input = concat!(r#"{"PathOpen":{"version":"1","id":"p"}}"#, "\n");
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::NoSteps));
    }

    #[test]
    fn reader_after_close_fatal() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
            r#"{"PathClose":{}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s2","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
        );
        let err = Path::from_jsonl_str(input).unwrap_err();
        assert!(matches!(err, JsonlError::AfterClose { line_num: 5 }));
    }

    #[test]
    fn reader_unknown_variant_skipped() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"FutureKind":{"whatever":true}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).expect("unknown variant must be skipped, not fatal");
        assert_eq!(path.steps.len(), 1);
    }

    // ── Reader: happy paths ────────────────────────────────────────────────

    #[test]
    fn reader_linear_path_with_inferred_head() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s2","parents":["s1"],"actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).unwrap();
        assert_eq!(path.path.head, "s2");
    }

    #[test]
    fn reader_actor_def_last_write_wins() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"ActorDef":{"actor":"human:alex","definition":{"name":"Alex v1"}}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"human:alex","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"ActorDef":{"actor":"human:alex","definition":{"name":"Alex v2"}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).unwrap();
        let actors = path.meta.unwrap().actors.unwrap();
        assert_eq!(actors["human:alex"].name.as_deref(), Some("Alex v2"));
    }

    #[test]
    fn reader_path_meta_patch_refs_replace() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p","meta":{"refs":[{"rel":"fixes","href":"issue:1"}]}}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"PathMeta":{"patch":{"refs":[{"rel":"tracks","href":"issue:2"}]}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).unwrap();
        let refs = path.meta.unwrap().refs;
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].rel, "tracks");
    }

    #[test]
    fn reader_path_meta_extra_merges() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p","meta":{"custom_a":1}}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"PathMeta":{"patch":{"custom_b":2}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).unwrap();
        let extra = path.meta.unwrap().extra;
        assert_eq!(extra.get("custom_a"), Some(&json!(1)));
        assert_eq!(extra.get("custom_b"), Some(&json!(2)));
    }

    #[test]
    fn reader_step_signature_attached() {
        let input = concat!(
            r#"{"PathOpen":{"version":"1","id":"p"}}"#,
            "\n",
            r#"{"Step":{"step":{"id":"s1","actor":"a","timestamp":"t"},"change":{}}}"#,
            "\n",
            r#"{"Signature":{"target":"step:s1","signature":{"signer":"human:alex","key":"gpg:A","scope":"author","sig":"X"}}}"#,
            "\n",
            r#"{"Head":{"step_id":"s1"}}"#,
            "\n",
        );
        let path = Path::from_jsonl_str(input).unwrap();
        let sigs = &path.steps[0].meta.as_ref().unwrap().signatures;
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].scope, "author");
    }

    // ── Writer ─────────────────────────────────────────────────────────────

    #[test]
    fn writer_emits_path_open_first() {
        let path = Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None)],
            meta: None,
        };
        let jsonl = path.to_jsonl_string().unwrap();
        let first = jsonl.lines().next().unwrap();
        assert!(first.starts_with(r#"{"PathOpen":"#));
    }

    #[test]
    fn writer_sorts_actor_defs() {
        let mut actors = HashMap::new();
        actors.insert(
            "human:zoe".to_string(),
            ActorDefinition {
                name: Some("Zoe".into()),
                ..Default::default()
            },
        );
        actors.insert(
            "human:alex".to_string(),
            ActorDefinition {
                name: Some("Alex".into()),
                ..Default::default()
            },
        );
        let meta = PathMeta {
            actors: Some(actors),
            ..Default::default()
        };
        let path = Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None)],
            meta: Some(meta),
        };
        let jsonl = path.to_jsonl_string().unwrap();
        let actor_lines: Vec<&str> = jsonl
            .lines()
            .filter(|l| l.starts_with(r#"{"ActorDef":"#))
            .collect();
        assert_eq!(actor_lines.len(), 2);
        let alex_idx = actor_lines.iter().position(|l| l.contains("alex")).unwrap();
        let zoe_idx = actor_lines.iter().position(|l| l.contains("zoe")).unwrap();
        assert!(alex_idx < zoe_idx, "alex should come before zoe");
    }

    #[test]
    fn writer_strips_step_signatures_from_step_body() {
        let mut step = make_step("s1", None);
        step.meta = Some(StepMeta {
            signatures: vec![Signature {
                signer: "human:alex".into(),
                key: "gpg:A".into(),
                scope: "author".into(),
                sig: "X".into(),
                timestamp: None,
            }],
            ..Default::default()
        });
        let path = Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        };
        let jsonl = path.to_jsonl_string().unwrap();
        // The Step line must not contain any signature payload, only the
        // following Signature line should carry it.
        let step_line = jsonl
            .lines()
            .find(|l| l.starts_with(r#"{"Step":"#))
            .unwrap();
        assert!(
            !step_line.contains("\"signatures\""),
            "step body should not carry its own signatures: {step_line}"
        );
        let sig_line = jsonl
            .lines()
            .find(|l| l.starts_with(r#"{"Signature":"#))
            .unwrap();
        assert!(sig_line.contains(r#""target":"step:s1""#));
    }

    // ── Round-trip ─────────────────────────────────────────────────────────

    fn linear_path() -> Path {
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s2".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None), make_step("s2", Some("s1"))],
            meta: None,
        }
    }

    fn signed_path_with_actors() -> Path {
        let mut actors = HashMap::new();
        actors.insert(
            "human:alex".to_string(),
            ActorDefinition {
                name: Some("Alex Kesling".into()),
                ..Default::default()
            },
        );
        let sig = Signature {
            signer: "human:alex".into(),
            key: "gpg:A".into(),
            scope: "author".into(),
            sig: "SIG".into(),
            timestamp: None,
        };
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: Some(Base::vcs("github:org/repo", "abc123")),
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None)],
            meta: Some(PathMeta {
                title: Some("Test".into()),
                actors: Some(actors),
                signatures: vec![sig],
                ..Default::default()
            }),
        }
    }

    fn path_with_step_signature() -> Path {
        let mut step = make_step("s1", None);
        step.meta = Some(StepMeta {
            intent: Some("fix bug".into()),
            signatures: vec![Signature {
                signer: "human:alex".into(),
                key: "gpg:A".into(),
                scope: "author".into(),
                sig: "SIG".into(),
                timestamp: None,
            }],
            ..Default::default()
        });
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        }
    }

    fn path_with_graph_ref() -> Path {
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: Some("toolpath://archive/release-v2".into()),
            },
            steps: vec![make_step("s1", None)],
            meta: None,
        }
    }

    fn path_with_dead_end() -> Path {
        Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s2".into(),
                graph_ref: None,
            },
            steps: vec![
                make_step("s1", None),
                make_step("s2", Some("s1")),
                make_step("s_dead", Some("s1")),
            ],
            meta: None,
        }
    }

    #[test]
    fn roundtrip_linear_path() {
        let p = linear_path();
        let jsonl = p.to_jsonl_string().unwrap();
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_signed_with_actors() {
        let p = signed_path_with_actors();
        let jsonl = p.to_jsonl_string().unwrap();
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_step_signature() {
        let p = path_with_step_signature();
        let jsonl = p.to_jsonl_string().unwrap();
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_graph_ref() {
        let p = path_with_graph_ref();
        let jsonl = p.to_jsonl_string().unwrap();
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_dead_end_uses_explicit_head() {
        let p = path_with_dead_end();
        let jsonl = p.to_jsonl_string().unwrap();
        // Writer always emits Head so reader doesn't have to disambiguate.
        assert!(jsonl.contains(r#"{"Head":{"step_id":"s2"}}"#));
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_refs_in_path_meta() {
        let p = Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None)],
            meta: Some(PathMeta {
                refs: vec![Ref {
                    rel: "fixes".into(),
                    href: "issue://1".into(),
                }],
                ..Default::default()
            }),
        };
        let jsonl = p.to_jsonl_string().unwrap();
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn roundtrip_kind_in_path_meta() {
        let p = Path {
            path: PathIdentity {
                id: "p".into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![make_step("s1", None)],
            meta: Some(PathMeta {
                kind: Some(crate::v1::PATH_KIND_AGENT_CODING_SESSION.to_string()),
                ..Default::default()
            }),
        };
        let jsonl = p.to_jsonl_string().unwrap();
        assert!(
            jsonl.contains(r#""kind":"https://toolpath.net/kinds/agent-coding-session/v1.0.0""#)
        );
        let back = Path::from_jsonl_str(&jsonl).unwrap();
        assert_eq!(canonical_json(&p), canonical_json(&back));
    }

    #[test]
    fn path_meta_line_can_set_kind() {
        let patch = PathMetaPatch {
            kind: Some("https://toolpath.net/kinds/agent-coding-session/v1.0.0".into()),
            ..Default::default()
        };
        let mut meta = PathMeta::default();
        apply_meta_patch(&mut meta, patch);
        assert_eq!(
            meta.kind.as_deref(),
            Some("https://toolpath.net/kinds/agent-coding-session/v1.0.0")
        );
    }

    #[test]
    fn change_artifact_roundtrip_preserved() {
        // Sanity check that we don't mangle ArtifactChange fields through
        // the transparent StepBody.
        let art = ArtifactChange {
            raw: Some("@@".into()),
            structural: None,
        };
        let v = serde_json::to_value(&art).unwrap();
        assert_eq!(v["raw"], "@@");
    }
}
