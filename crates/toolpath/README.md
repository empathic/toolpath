# toolpath

Core types, builders, and query operations for Toolpath provenance documents.

Toolpath records **who** changed **what**, **why**, what they tried that
didn't work, and how to verify all of it. Think "git blame, but for
everything that happens to code — including the stuff git doesn't see."

Three objects model this: a **Step** is one atomic change by one actor.
A **Path** is a DAG of steps (like a PR) — abandoned branches become
implicit dead ends. A **Graph** collects related paths (like a release).

## Overview

This crate provides the type system and query API for Toolpath. It contains:

- **Types**: `Document`, `Graph`, `Path`, `Step`, `ArtifactChange`, and all supporting structures
- **Builders**: Convenient constructors and builder methods for constructing documents
- **Serde**: Full serialization/deserialization with `#[serde(untagged)]` document discrimination
- **Query**: Graph traversal and filtering operations on step DAGs

This is the gravity well of the workspace. All other crates depend on `toolpath`; it depends on nothing except `serde` and `serde_json`.

## Types

```text
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
use toolpath::v1::{Step, Path, Base, ArtifactChange};

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
use toolpath::v1::{Step, query};

let s1 = Step::new("s1", "human:alex", "2026-01-29T10:00:00Z")
    .with_raw_change("src/main.rs", "@@");
let s2 = Step::new("s2", "agent:claude", "2026-01-29T10:01:00Z")
    .with_parent("s1")
    .with_raw_change("src/main.rs", "@@");
let steps = vec![s1, s2];

let ancestors = query::ancestors(&steps, "s2");
let dead_ends = query::dead_ends(&steps, "s2");
let human_steps = query::filter_by_actor(&steps, "human:");
let main_rs = query::filter_by_artifact(&steps, "src/main.rs");
let all_files = query::all_artifacts(&steps);
let all_actors = query::all_actors(&steps);
let index = query::step_index(&steps);
```

## Serialization

Documents roundtrip through JSON:

```rust
use toolpath::v1::Document;

let json_str = r#"{"Step":{"step":{"id":"s1","actor":"human:alex","timestamp":"2026-01-29T10:00:00Z"},"change":{}}}"#;
let doc = Document::from_json(json_str).unwrap();
let json = doc.to_json_pretty().unwrap();
assert!(json.contains("s1"));
```

The `Document` enum uses `#[serde(untagged)]` and discriminates by structure: it tries Graph (has `graph` + `paths`), then Path (has `path` + `steps`), then Step (has `step` + `change`).

## Part of Toolpath

This crate is the core of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`toolpath-cli`](https://crates.io/crates/toolpath-cli) -- unified CLI (`cargo install toolpath-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
- [FAQ](https://github.com/empathic/toolpath/blob/main/FAQ.md) -- design rationale
