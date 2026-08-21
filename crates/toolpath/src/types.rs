use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Graph — the root of every Toolpath document
// ============================================================================

/// A Toolpath document — a collection of related paths.
///
/// `Graph` is the single root type of the format. Every `.path.json` file
/// deserializes to a `Graph`; a "single PR" or "single conversation" is just
/// a Graph that happens to contain one path.
///
/// Each entry in `paths` is either an inline [`Path`] or a [`PathRef`]
/// pointing to an external document (via `$ref`).
///
/// # JSON shape
///
/// ```json
/// {
///   "graph": { "id": "release-v1.2" },
///   "paths": [
///     { "path": { "id": "pr-42", "head": "s3" }, "steps": [ … ] },
///     { "$ref": "https://example.com/path-pr-99.json" }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub graph: GraphIdentity,
    pub paths: Vec<PathOrRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<GraphMeta>,
}

/// Graph identity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphIdentity {
    pub id: String,
}

/// Graph metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actors: Option<HashMap<String, ActorDefinition>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Signature>,
    /// Additional properties (schema: `additionalProperties: true`)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Either an inline path or a reference to an external path
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathOrRef {
    Path(Box<Path>),
    Ref(PathRef),
}

/// Reference to an external path document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRef {
    #[serde(rename = "$ref")]
    pub ref_url: String,
}

// ============================================================================
// Path
// ============================================================================

/// An ordered sequence of steps forming a DAG — for example, a pull request.
///
/// `path.head` names the step ID at the tip of the active branch.  Steps
/// link to their parents via [`StepIdentity::parents`], forming a DAG.
/// Steps present in `steps` but **not** on the ancestry of `head` are
/// considered *dead ends* — abandoned approaches that are preserved for
/// provenance but did not contribute to the final result.
///
/// `path.base` optionally anchors the path to a repository and ref
/// (e.g. `"github:org/repo"` at commit `"abc123"`).
///
/// # JSON shape
///
/// ```json
/// {
///   "path": {
///     "id": "pr-42",
///     "base": { "uri": "github:org/repo", "ref": "main" },
///     "head": "step-004"
///   },
///   "steps": [
///     { "step": { "id": "step-001", "actor": "human:alex", … }, "change": { … } },
///     { "step": { "id": "step-002", "parents": ["step-001"], … }, "change": { … } }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Path {
    pub path: PathIdentity,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<PathMeta>,
}

/// Path identity and context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathIdentity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    pub head: String,
    /// Optional `$ref`-style URL naming the graph this path belongs to.
    /// Uses the same URL conventions as `Graph.paths[*].$ref`
    /// (e.g. `toolpath://archive/release-v2`, `https://...`, `file:///...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_ref: Option<String>,
}

/// Root context for a path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Base {
    /// Origin identifier: repo (e.g., "github:org/repo"), filesystem
    /// location ("file:///…"), or another toolpath step
    /// ("toolpath:path-id/step-id").
    pub uri: String,
    /// State identifier the origin uses to name a specific reproducible
    /// state — commit hash, revision number, tag, changeset ID, etc.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_str: Option<String>,
    /// Branch name the path was opened against, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// [`PathMeta::kind`] URI for a path derived from an AI coding conversation.
/// Spec at <https://toolpath.net/kinds/agent-coding-session/v1.1.0>.
///
/// v1.1.0 specifies message-level token accounting: steps derived from one
/// provider message share a `message_id`, and the message's `token_usage`
/// appears on exactly one of them (the group's last step in document
/// order), so summing usage over a path's steps yields session totals.
pub const PATH_KIND_AGENT_CODING_SESSION: &str =
    "https://toolpath.net/kinds/agent-coding-session/v1.1.0";

/// The previous version URI. Documents produced before the v1.1.0
/// accounting rule carry this kind; consumers summing their `token_usage`
/// per step must deduplicate repeated message-level usage themselves.
pub const PATH_KIND_AGENT_CODING_SESSION_V1_0_0: &str =
    "https://toolpath.net/kinds/agent-coding-session/v1.0.0";

/// Path metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actors: Option<HashMap<String, ActorDefinition>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Signature>,
    /// Additional properties (schema: `additionalProperties: true`)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ============================================================================
// Step
// ============================================================================

/// The atomic unit of provenance — one actor, one timestamp, one or more
/// artifact changes.
///
/// Actor strings follow the convention `type:name`, for example
/// `"human:alex"`, `"agent:claude-code"`, or `"tool:rustfmt"`.
///
/// Steps link to their parents via [`StepIdentity::parents`], forming a DAG.
/// A step with no parents is a root.
///
/// # Builder API
///
/// ```
/// use toolpath::v1::Step;
///
/// let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
///     .with_parent("step-000")
///     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
///     .with_intent("Fix greeting");
/// ```
///
/// # JSON shape
///
/// ```json
/// {
///   "step": {
///     "id": "step-001",
///     "parents": ["step-000"],
///     "actor": "human:alex",
///     "timestamp": "2026-01-29T10:00:00Z"
///   },
///   "change": {
///     "src/main.rs": { "raw": "@@ -1 +1 @@\n-old\n+new" }
///   },
///   "meta": { "intent": "Fix greeting" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub step: StepIdentity,
    pub change: HashMap<String, ArtifactChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<StepMeta>,
}

/// Step identity and lineage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepIdentity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    pub actor: String,
    pub timestamp: String,
}

/// A change to a single artifact, expressed from one or both perspectives.
///
/// - **`raw`** — a unified diff string (the classic patch format).
/// - **`structural`** — a language-aware, AST-level description of the change
///   (e.g. `"type": "add_function"` with structured metadata).
///
/// At least one perspective should be present. Both can coexist, giving
/// consumers a choice between human-readable diffs and machine-parseable
/// operations.
///
/// # JSON shape
///
/// ```json
/// {
///   "raw": "@@ -12,1 +12,1 @@\n-old_line\n+new_line",
///   "structural": { "type": "rename_function", "from": "foo", "to": "bar" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactChange {
    /// Unified Diff format change
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Language-aware structural operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural: Option<StructuralChange>,
}

/// Structural change representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralChange {
    #[serde(rename = "type")]
    pub change_type: String,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Step metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<VcsSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actors: Option<HashMap<String, ActorDefinition>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Signature>,
    /// Additional properties (schema: `additionalProperties: true`)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// VCS source reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcsSource {
    #[serde(rename = "type")]
    pub vcs_type: String,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    /// Additional properties (schema: `additionalProperties: true`)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ============================================================================
// Shared types
// ============================================================================

/// Reference to external resource
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ref {
    pub rel: String,
    pub href: String,
}

/// Full actor definition with identity and key information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActorDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<Identity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
}

/// External identity reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub system: String,
    pub id: String,
}

/// Cryptographic key reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Key {
    #[serde(rename = "type")]
    pub key_type: String,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// Cryptographic signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    pub signer: String,
    pub key: String,
    pub scope: String,
    pub sig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

// ============================================================================
// Convenience methods
// ============================================================================

impl Graph {
    /// Create a new graph with the given ID
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            graph: GraphIdentity { id: id.into() },
            paths: Vec::new(),
            meta: None,
        }
    }

    /// Create a single-path graph wrapping `path`. The graph's id mirrors the
    /// path's id so an unwrapped path-shaped derivation has a natural lift to
    /// the top-level format.
    pub fn from_path(path: Path) -> Self {
        Self {
            graph: GraphIdentity {
                id: path.path.id.clone(),
            },
            paths: vec![PathOrRef::Path(Box::new(path))],
            meta: None,
        }
    }

    /// Parse a Toolpath document from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize to pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// If this graph wraps exactly one inline path, return it.
    pub fn single_path(&self) -> Option<&Path> {
        if self.paths.len() != 1 {
            return None;
        }
        match &self.paths[0] {
            PathOrRef::Path(p) => Some(p),
            PathOrRef::Ref(_) => None,
        }
    }

    /// If this graph wraps exactly one inline path, take it by consuming the graph.
    pub fn into_single_path(self) -> Option<Path> {
        if self.paths.len() != 1 {
            return None;
        }
        match self.paths.into_iter().next().unwrap() {
            PathOrRef::Path(p) => Some(*p),
            PathOrRef::Ref(_) => None,
        }
    }
}

impl Path {
    /// Create a new path with the given ID, base, and head
    pub fn new(id: impl Into<String>, base: Option<Base>, head: impl Into<String>) -> Self {
        Self {
            path: PathIdentity {
                id: id.into(),
                base,
                head: head.into(),
                graph_ref: None,
            },
            steps: Vec::new(),
            meta: None,
        }
    }
}

impl Base {
    /// Create a VCS base reference
    pub fn vcs(uri: impl Into<String>, ref_str: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            ref_str: Some(ref_str.into()),
            branch: None,
        }
    }

    /// Create a toolpath base reference (branching from another path's step)
    pub fn toolpath(path_id: impl Into<String>, step_id: impl Into<String>) -> Self {
        Self {
            uri: format!("toolpath:{}/{}", path_id.into(), step_id.into()),
            ref_str: None,
            branch: None,
        }
    }

    /// Attach a VCS branch name to this base.
    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
}

impl Step {
    /// Create a new step
    pub fn new(
        id: impl Into<String>,
        actor: impl Into<String>,
        timestamp: impl Into<String>,
    ) -> Self {
        Self {
            step: StepIdentity {
                id: id.into(),
                parents: Vec::new(),
                actor: actor.into(),
                timestamp: timestamp.into(),
            },
            change: HashMap::new(),
            meta: None,
        }
    }

    /// Add a parent reference
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.step.parents.push(parent.into());
        self
    }

    /// Add a raw diff change for an artifact
    pub fn with_raw_change(mut self, artifact: impl Into<String>, raw: impl Into<String>) -> Self {
        self.change.insert(
            artifact.into(),
            ArtifactChange {
                raw: Some(raw.into()),
                structural: None,
            },
        );
        self
    }

    /// Set the intent
    pub fn with_intent(mut self, intent: impl Into<String>) -> Self {
        self.meta.get_or_insert_with(StepMeta::default).intent = Some(intent.into());
        self
    }

    /// Set the VCS source
    pub fn with_vcs_source(
        mut self,
        vcs_type: impl Into<String>,
        revision: impl Into<String>,
    ) -> Self {
        self.meta.get_or_insert_with(StepMeta::default).source = Some(VcsSource {
            vcs_type: vcs_type.into(),
            revision: revision.into(),
            change_id: None,
            extra: HashMap::new(),
        });
        self
    }
}

impl ArtifactChange {
    /// Create a raw diff change
    pub fn raw(diff: impl Into<String>) -> Self {
        Self {
            raw: Some(diff.into()),
            structural: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_builder() {
        let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-hello\n+world")
            .with_intent("Fix greeting");

        let json = serde_json::to_string_pretty(&step).unwrap();
        assert!(json.contains("step-001"));
        assert!(json.contains("human:alex"));
    }

    #[test]
    fn test_roundtrip() {
        let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
            .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-hello\n+world");

        let json = serde_json::to_string(&step).unwrap();
        let parsed: Step = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.step.id, "step-001");
        assert_eq!(parsed.step.actor, "human:alex");
    }

    #[test]
    fn test_base_constructors() {
        let vcs_base = Base::vcs("github:org/repo", "abc123");
        assert_eq!(vcs_base.uri, "github:org/repo");
        assert_eq!(vcs_base.ref_str, Some("abc123".to_string()));
        assert_eq!(vcs_base.branch, None);

        let toolpath_base = Base::toolpath("path-main", "step-005");
        assert_eq!(toolpath_base.uri, "toolpath:path-main/step-005");
        assert_eq!(toolpath_base.ref_str, None);
        assert_eq!(toolpath_base.branch, None);
    }

    #[test]
    fn test_base_branch_roundtrip() {
        let base = Base {
            uri: "github:org/repo".into(),
            ref_str: Some("abc123def456".into()),
            branch: Some("main".into()),
        };
        let json = serde_json::to_string(&base).unwrap();
        assert!(json.contains(r#""branch":"main""#));
        assert!(json.contains(r#""ref":"abc123def456""#));

        let parsed: Base = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.uri, "github:org/repo");
        assert_eq!(parsed.ref_str.as_deref(), Some("abc123def456"));
        assert_eq!(parsed.branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_base_branch_omitted_when_none() {
        let base = Base {
            uri: "github:org/repo".into(),
            ref_str: Some("abc123".into()),
            branch: None,
        };
        let json = serde_json::to_string(&base).unwrap();
        assert!(!json.contains("branch"));
    }

    #[test]
    fn test_base_with_branch_constructor() {
        let base = Base::vcs("github:org/repo", "abc123").with_branch("main");
        assert_eq!(base.branch.as_deref(), Some("main"));
        assert_eq!(base.ref_str.as_deref(), Some("abc123"));
    }

    // ── Graph serialization ────────────────────────────────────────────

    #[test]
    fn test_graph_roundtrip_empty() {
        let graph = Graph::new("g1");
        let json = graph.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        assert_eq!(parsed.graph.id, "g1");
        assert!(parsed.paths.is_empty());
    }

    #[test]
    fn test_graph_from_path_wraps_single_path() {
        let step =
            Step::new("s1", "human:alex", "2026-01-01T00:00:00Z").with_raw_change("f.rs", "@@");
        let path = Path {
            path: PathIdentity {
                id: "p1".into(),
                base: Some(Base::vcs("github:org/repo", "abc")),
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        };
        let graph = Graph::from_path(path);
        assert_eq!(graph.graph.id, "p1");
        let json = graph.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let inline = parsed.single_path().unwrap();
        assert_eq!(inline.path.id, "p1");
        assert_eq!(inline.steps.len(), 1);
    }

    #[test]
    fn test_graph_to_json_pretty() {
        let graph = Graph::new("g1");
        let json = graph.to_json_pretty().unwrap();
        assert!(json.contains('\n'));
        assert!(json.contains("\"g1\""));
    }

    #[test]
    fn test_graph_from_json_invalid() {
        let result = Graph::from_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_graph_single_path_none_for_multi() {
        let p1 = Path::new("p1", None, "s1");
        let p2 = Path::new("p2", None, "s2");
        let graph = Graph {
            graph: GraphIdentity { id: "g".into() },
            paths: vec![PathOrRef::Path(Box::new(p1)), PathOrRef::Path(Box::new(p2))],
            meta: None,
        };
        assert!(graph.single_path().is_none());
    }

    #[test]
    fn test_graph_single_path_none_for_ref() {
        let graph = Graph {
            graph: GraphIdentity { id: "g".into() },
            paths: vec![PathOrRef::Ref(PathRef {
                ref_url: "https://example.com/p.json".into(),
            })],
            meta: None,
        };
        assert!(graph.single_path().is_none());
    }

    // ── Graph::new ─────────────────────────────────────────────────────

    #[test]
    fn test_graph_new() {
        let g = Graph::new("my-graph");
        assert_eq!(g.graph.id, "my-graph");
        assert!(g.paths.is_empty());
        assert!(g.meta.is_none());
    }

    // ── Path::new ──────────────────────────────────────────────────────

    #[test]
    fn test_path_new() {
        let p = Path::new("p1", Some(Base::vcs("repo", "abc")), "head-step");
        assert_eq!(p.path.id, "p1");
        assert_eq!(p.path.head, "head-step");
        assert!(p.path.base.is_some());
        assert!(p.steps.is_empty());
        assert!(p.meta.is_none());
    }

    #[test]
    fn test_path_new_no_base() {
        let p = Path::new("p1", None, "s1");
        assert!(p.path.base.is_none());
    }

    // ── Step builder ───────────────────────────────────────────────────

    #[test]
    fn test_step_with_parent() {
        let step = Step::new("s2", "human:alex", "2026-01-01T00:00:00Z").with_parent("s1");
        assert_eq!(step.step.parents, vec!["s1".to_string()]);
    }

    #[test]
    fn test_step_with_multiple_parents() {
        let step = Step::new("s3", "human:alex", "2026-01-01T00:00:00Z")
            .with_parent("s1")
            .with_parent("s2");
        assert_eq!(step.step.parents.len(), 2);
    }

    #[test]
    fn test_step_with_intent() {
        let step = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z").with_intent("Fix bug");
        assert_eq!(step.meta.unwrap().intent.unwrap(), "Fix bug");
    }

    #[test]
    fn test_step_with_vcs_source() {
        let step =
            Step::new("s1", "human:alex", "2026-01-01T00:00:00Z").with_vcs_source("git", "abc123");
        let meta = step.meta.unwrap();
        let source = meta.source.unwrap();
        assert_eq!(source.vcs_type, "git");
        assert_eq!(source.revision, "abc123");
        assert!(source.change_id.is_none());
    }

    #[test]
    fn test_step_with_raw_change() {
        let step = Step::new("s1", "human:alex", "2026-01-01T00:00:00Z")
            .with_raw_change("f.rs", "@@ -1 +1 @@");
        assert!(step.change.contains_key("f.rs"));
        let change = &step.change["f.rs"];
        assert_eq!(change.raw.as_deref(), Some("@@ -1 +1 @@"));
        assert!(change.structural.is_none());
    }

    // ── ArtifactChange ─────────────────────────────────────────────────

    #[test]
    fn test_artifact_change_raw() {
        let change = ArtifactChange::raw("diff content");
        assert_eq!(change.raw.as_deref(), Some("diff content"));
        assert!(change.structural.is_none());
    }

    // ── PathOrRef serialization ────────────────────────────────────────

    #[test]
    fn test_path_or_ref_inline_path_roundtrip() {
        let path = Path::new("p1", None, "s1");
        let por = PathOrRef::Path(Box::new(path));
        let json = serde_json::to_string(&por).unwrap();
        assert!(json.contains("\"p1\""));
        let parsed: PathOrRef = serde_json::from_str(&json).unwrap();
        match parsed {
            PathOrRef::Path(p) => assert_eq!(p.path.id, "p1"),
            _ => panic!("Expected Path"),
        }
    }

    #[test]
    fn test_path_or_ref_ref_roundtrip() {
        let por = PathOrRef::Ref(PathRef {
            ref_url: "https://example.com/path.json".to_string(),
        });
        let json = serde_json::to_string(&por).unwrap();
        assert!(json.contains("$ref"));
        let parsed: PathOrRef = serde_json::from_str(&json).unwrap();
        match parsed {
            PathOrRef::Ref(r) => assert_eq!(r.ref_url, "https://example.com/path.json"),
            _ => panic!("Expected Ref"),
        }
    }

    // ── Metadata types serialization ───────────────────────────────────

    #[test]
    fn test_graph_meta_default_skips_empty() {
        let g = Graph {
            graph: GraphIdentity { id: "g1".into() },
            paths: vec![],
            meta: Some(GraphMeta::default()),
        };
        let json = serde_json::to_string(&g).unwrap();
        // Default meta should have all fields skipped — but meta itself stays
        assert!(!json.contains("\"title\""));
        assert!(!json.contains("\"refs\""));
    }

    #[test]
    fn test_step_meta_with_refs() {
        let step = Step {
            step: StepIdentity {
                id: "s1".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            change: HashMap::new(),
            meta: Some(StepMeta {
                refs: vec![super::Ref {
                    rel: "issue".into(),
                    href: "https://github.com/org/repo/issues/1".into(),
                }],
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"issue\""));
        assert!(json.contains("issues/1"));
    }

    #[test]
    fn test_path_meta_kind_serde() {
        let meta = PathMeta {
            kind: Some(PATH_KIND_AGENT_CODING_SESSION.to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains(&format!(r#""kind":"{PATH_KIND_AGENT_CODING_SESSION}""#)));
        let parsed: PathMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind.as_deref(), Some(PATH_KIND_AGENT_CODING_SESSION));
    }

    /// The one test that spells the current kind URI out literally: every
    /// other test goes through the constant, so a typo in the constant would
    /// otherwise self-validate.
    #[test]
    fn test_path_kind_constant_spells_current_uri() {
        assert_eq!(
            PATH_KIND_AGENT_CODING_SESSION,
            "https://toolpath.net/kinds/agent-coding-session/v1.1.0"
        );
    }

    #[test]
    fn test_path_meta_kind_omitted_when_none() {
        let json = serde_json::to_string(&PathMeta::default()).unwrap();
        assert!(!json.contains("kind"));
    }

    #[test]
    fn test_identity_serialization() {
        let id = super::Identity {
            system: "email".into(),
            id: "user@example.com".into(),
        };
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.contains("email"));
        assert!(json.contains("user@example.com"));
    }

    #[test]
    fn test_structural_change_serialization() {
        let mut extra = HashMap::new();
        extra.insert("from".to_string(), serde_json::json!("foo"));
        extra.insert("to".to_string(), serde_json::json!("bar"));
        let sc = StructuralChange {
            change_type: "rename_function".into(),
            extra,
        };
        let json = serde_json::to_string(&sc).unwrap();
        assert!(json.contains("rename_function"));
        assert!(json.contains("\"from\""));
        assert!(json.contains("\"bar\""));
    }
}
