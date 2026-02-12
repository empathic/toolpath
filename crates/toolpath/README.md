# toolpath

Core types, builders, and query operations for Toolpath provenance documents.

## Overview

This crate provides the type system for Toolpath -- a format for tracking artifact transformation provenance. It contains:

- **Types**: `Document`, `Graph`, `Path`, `Step`, `ArtifactChange`, and all supporting structures
- **Builders**: Convenient constructors and builder methods for constructing documents
- **Serde**: Full serialization/deserialization with `#[serde(untagged)]` document discrimination
- **Query**: Graph traversal and filtering operations on step DAGs

This is the gravity well of the workspace. All other crates depend on `toolpath`; it depends on nothing except `serde` and `serde_json`.

## Types

```
Document (enum: Graph | Path | Step)

Graph
  graph: GraphIdentity { id }
  paths: Vec<PathOrRef>         -- inline Path or $ref
  meta?: GraphMeta

Path
  path: PathIdentity { id, base?, head }
  steps: Vec<Step>
  meta?: PathMeta

Step
  step: StepIdentity { id, parents, actor, timestamp }
  change: HashMap<String, ArtifactChange>
  meta?: StepMeta
```

## Building documents

```rust
use toolpath::{Step, Path, Base, ArtifactChange};

// Build a step
let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
    .with_parent("step-000")
    .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-hello\n+world")
    .with_intent("Fix greeting")
    .with_vcs_source("git", "abc123def456");

// Build a path
let path = Path::new(
    "path-pr-42",
    Some(Base::vcs("github:org/repo", "abc123")),
    "step-001",
);

// Branch from another path's step
let base = Base::toolpath("path-main", "step-005");
```

## Query operations

The `query` module provides graph traversal and filtering over step slices:

```rust
use toolpath::query;

// Walk the parent chain from head
let ancestors = query::ancestors(&steps, "step-005");

// Find abandoned branches
let dead_ends = query::dead_ends(&steps, "step-005");

// Filter by actor type
let human_steps = query::filter_by_actor(&steps, "human:");
let agent_steps = query::filter_by_actor(&steps, "agent:");

// Filter by artifact
let main_rs = query::filter_by_artifact(&steps, "src/main.rs");

// Time range
let recent = query::filter_by_time_range(&steps, "2026-01-29T00:00:00Z", "2026-01-30T00:00:00Z");

// Summaries
let all_files = query::all_artifacts(&steps);
let all_actors = query::all_actors(&steps);
let index = query::step_index(&steps);  // id -> &Step
```

## Serialization

Documents roundtrip through JSON:

```rust
use toolpath::Document;

let doc = Document::from_json(json_str)?;
let json = doc.to_json_pretty()?;
```

The `Document` enum uses `#[serde(untagged)]` and discriminates by structure: it tries Graph (has `graph` + `paths`), then Path (has `path` + `steps`), then Step (has `step` + `change`).
