# Toolpath

A tool-agnostic format for tracking artifact transformation provenance.

## What is this?

Toolpath records the complete history of how code (and other artifacts) evolved:

- **Who** made changes (humans, AI agents, formatters, linters, CI)
- **What** they changed (unified diffs + structural AST operations)
- **Why** they changed it (intent, linked issues, reasoning)
- **What else they tried** (dead ends preserved for reflection)
- **Verification** (cryptographic signatures, identity resolution)

## Three core objects

| Object    | What it represents                        | Example                |
|-----------|-------------------------------------------|------------------------|
| **Step**  | A single change to artifact(s)            | One commit, one edit   |
| **Path**  | A sequence of steps with a base context   | A PR, a coding session |
| **Graph** | A collection of related paths             | A release              |

Steps form a DAG via parent references. Dead ends are implicit: steps not in the ancestry of `path.head`.

```
              +-- step-3a -- step-4a  (dead end)
step-1 -- step-2 --+
              +-- step-3b -- step-4b -- step-5b  (head)
```

## Install

```bash
cargo install toolpath-cli
```

This installs a binary called `path`.

## Workspace

```
crates/
  toolpath/           Core types, builders, query API
  toolpath-git/       Derive from git repository history
  toolpath-claude/    Derive from Claude conversation logs
  toolpath-dot/       Graphviz DOT visualization
  toolpath-cli/       Unified CLI (binary: path)
```

See each crate's README for library-level documentation.

## Quick start

```bash
# Build everything
cargo build --workspace

# Derive a Toolpath document from this repo's git history
path derive git --repo . --branch main --pretty

# Visualize it
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png

# Derive from Claude conversation logs
path derive claude --project /path/to/project --pretty

# Query for dead ends (abandoned approaches)
path query dead-ends --input doc.json

# Filter steps by actor
path query filter --input doc.json --actor "agent:"

# Walk the ancestry of a step
path query ancestors --input doc.json --step-id step-003

# Merge multiple documents into a graph
path merge doc1.json doc2.json --title "Release v2" --pretty

# Validate a document
path validate --input examples/step-01-minimal.json
```

## CLI reference

```
path
  list
    git       [--repo PATH] [--remote NAME] [--json]
    claude    [--project PATH] [--json]
  derive
    git       --repo PATH --branch NAME[:START] [--base COMMIT] [--remote NAME] [--title TEXT]
    claude    --project PATH [--session ID] [--all]
  query
    ancestors --input FILE --step-id ID
    dead-ends --input FILE
    filter    --input FILE [--actor PREFIX] [--artifact PATH] [--after TIME] [--before TIME]
  render
    dot       [--input FILE] [--output FILE] [--show-files] [--show-timestamps]
  merge       FILE... [--title TEXT]
  track
    init      --file PATH --actor ACTOR [--title TEXT] [--base-uri URI] [--base-ref REF]
    step      --session FILE --seq N [--actor ACTOR] [--intent TEXT]
    visit     --session FILE --seq N
    note      --session FILE --intent TEXT
    export    --session FILE
    close     --session FILE
    list
  validate    --input FILE
  haiku
```

Global: `--pretty` for formatted JSON output.

## Using the libraries

### Core types

```rust
use toolpath::{Step, Path, Base, Document};

let step = Step::new("step-001", "human:alex", "2026-01-29T10:00:00Z")
    .with_parent("step-000")
    .with_raw_change("src/main.rs", "@@ -1,1 +1,1 @@\n-hello\n+world")
    .with_intent("Fix greeting");

let path = Path::new(
    "path-pr-42",
    Some(Base::vcs("github:org/repo", "abc123")),
    "step-001",
);
```

### Query operations

```rust
use toolpath::query;

let ancestors = query::ancestors(&path.steps, &path.path.head);
let dead_ends = query::dead_ends(&path.steps, &path.path.head);
let by_actor = query::filter_by_actor(&path.steps, "agent:");
let artifacts = query::all_artifacts(&path.steps);
```

### Git derivation

```rust
use toolpath_git::{derive, DeriveConfig};

let repo = git2::Repository::open(".")?;
let config = DeriveConfig { remote: "origin".into(), title: None, base: None };
let doc = derive(&repo, &["main".into()], &config)?;
```

### DOT rendering

```rust
use toolpath_dot::{render, RenderOptions};

let dot_string = render(&doc, &RenderOptions::default());
```

## Documentation

- [RFC.md](RFC.md) -- Full format specification
- [FAQ.md](FAQ.md) -- Design rationale, FAQ, and open questions
- [NOTES.md](NOTES.md) -- Working notes
- [schema/toolpath.schema.json](schema/toolpath.schema.json) -- JSON Schema
- [examples/](examples/) -- 11 example documents covering steps, paths, and graphs

## Requirements

Rust 1.85+ (edition 2024).

## License

Apache-2.0
