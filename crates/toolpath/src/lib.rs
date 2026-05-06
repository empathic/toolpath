#![doc = include_str!("../README.md")]

mod jsonl;
mod query;
mod types;

/// The canonical JSON Schema for Toolpath documents, baked into the binary.
///
/// This is the same file as `schema/toolpath.schema.json` at the workspace
/// root (a symlink that resolves into this crate's `schema/` directory so
/// the schema lives inside the package boundary for `cargo publish`). The
/// schema describes the wire format produced by [`v1::Graph`] / [`v1::Path`]
/// / [`v1::Step`] serde and consumed by `path validate`.
pub const SCHEMA_JSON: &str = include_str!("../schema/toolpath.schema.json");

pub mod v1 {
    //! Versioned public API for Toolpath types and queries.
    //!
    //! Everything you need is re-exported from this module. Types are organized
    //! into four groups:
    //!
    //! # Documents
    //!
    //! The top-level types you construct, serialize, and deserialize:
    //!
    //! - [`Graph`] — a collection of paths; the single root type of every Toolpath document
    //! - [`Path`] — a sequence of steps (e.g. a PR)
    //! - [`Step`] — a single atomic change
    //!
    //! # Change representation
    //!
    //! How individual artifact changes are described:
    //!
    //! - [`ArtifactChange`] — wrapper holding one or both perspectives on a change
    //! - [`StructuralChange`] — language-aware AST-level operation
    //!
    //! # Identity and structure
    //!
    //! Types that wire the DAG together:
    //!
    //! - [`StepIdentity`] — step ID, parent links, actor, timestamp
    //! - [`PathIdentity`] — path ID, base context, head pointer
    //! - [`GraphIdentity`] — graph ID
    //! - [`Base`] — root context (repo URI + optional ref)
    //! - [`PathOrRef`] — inline path or external `$ref`
    //! - [`PathRef`] — external path reference URL
    //!
    //! # Metadata and provenance
    //!
    //! Optional annotations for richer context:
    //!
    //! - [`StepMeta`], [`PathMeta`], [`GraphMeta`] — metadata containers
    //! - [`ActorDefinition`] — full actor details (name, provider, keys)
    //! - [`Identity`] — external identity reference
    //! - [`Key`] — cryptographic key reference
    //! - [`Ref`] — link to external resource
    //! - [`Signature`] — cryptographic signature
    //! - [`VcsSource`] — VCS revision reference
    //!
    //! # Example — build a Path with two Steps
    //!
    //! ```
    //! use toolpath::v1::*;
    //!
    //! let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
    //!     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-old\n+new")
    //!     .with_intent("Initial fix");
    //!
    //! let s2 = Step::new("s2", "agent:claude-code", "2026-01-29T10:05:00Z")
    //!     .with_parent("s1")
    //!     .with_raw_change("src/main.rs", "@@ -1 +1 @@\n-new\n+better")
    //!     .with_intent("Refine fix");
    //!
    //! let path = Path {
    //!     path: PathIdentity {
    //!         id: "path-1".into(),
    //!         base: Some(Base::vcs("github:org/repo", "abc123")),
    //!         head: "s2".into(),
    //!         graph_ref: None,
    //!     },
    //!     steps: vec![s1, s2],
    //!     meta: None,
    //! };
    //!
    //! let json = serde_json::to_string_pretty(&path).unwrap();
    //! assert!(json.contains("path-1"));
    //! assert!(json.contains("s2"));
    //! ```

    /// DAG traversal and query functions for step collections.
    ///
    /// These functions operate on `&[Step]` slices, walking parent links to
    /// find ancestors, detect dead ends (abandoned branches), and filter steps
    /// by actor, artifact, or time range.
    ///
    /// # Example — find dead ends in a branching path
    ///
    /// ```
    /// use toolpath::v1::{Step, query};
    ///
    /// let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
    ///     .with_raw_change("f.rs", "@@");
    /// let s2 = Step::new("s2", "agent:claude", "2026-01-29T10:01:00Z")
    ///     .with_parent("s1")
    ///     .with_raw_change("f.rs", "@@");
    /// let abandoned = Step::new("s2a", "agent:claude", "2026-01-29T10:01:30Z")
    ///     .with_parent("s1")
    ///     .with_raw_change("f.rs", "@@");
    /// let s3 = Step::new("s3", "human:alex", "2026-01-29T10:02:00Z")
    ///     .with_parent("s2")
    ///     .with_raw_change("f.rs", "@@");
    ///
    /// let steps = vec![s1, s2, abandoned, s3];
    ///
    /// let dead = query::dead_ends(&steps, "s3");
    /// assert_eq!(dead.len(), 1);
    /// assert_eq!(dead[0].step.id, "s2a");
    ///
    /// let ancestors = query::ancestors(&steps, "s3");
    /// assert!(ancestors.contains("s1"));
    /// assert!(ancestors.contains("s3"));
    /// assert!(!ancestors.contains("s2a"));
    /// ```
    pub mod query {
        pub use crate::query::{
            all_actors, all_artifacts, ancestors, dead_ends, filter_by_actor, filter_by_artifact,
            filter_by_time_range, step_index,
        };
    }

    /// JSONL streaming format for single-path [`Graph`] documents.
    ///
    /// The on-wire shape is a sequence of externally-tagged objects
    /// `{"<Variant>": <body>}`, with one `JsonlLine` per logical event
    /// (path open, new step, etc.). Read a file with
    /// [`Graph::from_jsonl_reader`] / [`Graph::from_jsonl_str`], write with
    /// [`Graph::to_jsonl_writer`] / [`Graph::to_jsonl_string`]. The
    /// Path-level methods of the same name remain available for callers
    /// operating directly on an inner `Path`.
    pub mod jsonl {
        pub use crate::jsonl::{
            ActorDefBody, HeadBody, JsonlError, JsonlLine, PathCloseBody, PathMetaBody,
            PathMetaPatch, PathOpenBody, PathOpenMeta, SignatureBody, StepBody,
        };
    }

    pub use crate::types::{
        ActorDefinition, ArtifactChange, Base, Graph, GraphIdentity, GraphMeta, Identity, Key,
        Path, PathIdentity, PathMeta, PathOrRef, PathRef, Ref, Signature, Step, StepIdentity,
        StepMeta, StructuralChange, VcsSource,
    };
}
