# CLAUDE.md

## What is this project?

Toolpath is a format for artifact transformation provenance. It records who changed what, why, what they tried that didn't work, and how to verify all of it. Think "git blame, but for everything that happens to code, including the stuff git doesn't see."

Three core objects: **Step** (a single change), **Path** (a sequence of steps, e.g. a PR), **Graph** (a collection of paths, e.g. a release). Steps form a DAG via parent references. Dead ends are implicit -- steps not on the ancestry of `path.head`.

## Repository layout

```
Cargo.toml                      # workspace root (edition 2024, resolver 2)
crates/
  toolpath/                     # core types, builders, serde, query API
  toolpath-convo/               # provider-agnostic conversation types and traits
  toolpath-git/                 # derive from git repos (git2)
  toolpath-claude/              # derive from Claude conversation logs
  toolpath-dot/                 # Graphviz DOT rendering
  toolpath-cli/                 # unified CLI (binary: path)
schema/toolpath.schema.json     # JSON Schema for the format
examples/*.json                 # 11 example documents (step, path, graph)
RFC.md                          # full format specification
FAQ.md                          # design rationale, FAQ, and open questions
```

## Dependency graph

```
toolpath-cli (binary: path)
 ├── toolpath           (core types)
 ├── toolpath-convo     (conversation abstraction)
 ├── toolpath-git     → toolpath
 ├── toolpath-claude  → toolpath, toolpath-convo
 └── toolpath-dot     → toolpath
```

No cross-dependencies between satellite crates except `toolpath-claude → toolpath-convo`.

## Build and test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Requires Rust 1.85+ (edition 2024). Currently on rustc 1.93.0.

## CLI usage

The binary is called `path` (package: `toolpath-cli`):

```bash
cargo run -p toolpath-cli -- derive git --repo . --branch main --pretty
cargo run -p toolpath-cli -- derive claude --project /path/to/project
cargo run -p toolpath-cli -- render dot --input doc.json
cargo run -p toolpath-cli -- query dead-ends --input doc.json
cargo run -p toolpath-cli -- query ancestors --input doc.json --step-id step-003
cargo run -p toolpath-cli -- query filter --input doc.json --actor "agent:"
cargo run -p toolpath-cli -- merge doc1.json doc2.json --title "Combined"
cargo run -p toolpath-cli -- list git --repo .
cargo run -p toolpath-cli -- track init --file src/main.rs --actor "human:alex"
cargo run -p toolpath-cli -- validate --input doc.json
```

## Key conventions

- Actor strings follow the pattern `type:name` (e.g. `human:alex`, `agent:claude-code`, `tool:rustfmt`)
- Artifact keys in `change` are URLs; bare paths are relative to `path.base`
- Change perspectives: `raw` (unified diff) and `structural` (AST-level operations)
- The `meta` object is always optional; minimal documents need only `step` + `change`
- IDs must be unique within their containing scope (steps within a path, paths within a graph)

## Testing

Tests live alongside the code (`#[cfg(test)] mod tests`). No integration test directory yet. Key test areas:

- `toolpath`: serde roundtrip, builder methods, query functions (12 tests)
- `toolpath-claude`: path resolution, conversation reading, query, watcher, derive (22 unit + 4 doc tests)

Validate example documents: `for f in examples/*.json; do cargo run -p toolpath-cli -- validate --input "$f"; done`

## Feature flags

- `toolpath-claude` has a `watcher` feature (default: on) gating `notify`/`tokio` dependencies for filesystem watching

## Things to know

- The `Document` enum is externally tagged -- JSON documents are wrapped in `{"Step": ...}`, `{"Path": ...}`, or `{"Graph": ...}`
- `PathOrRef::Path` is `Box<Path>` to avoid a large enum variant size difference
- The git derivation (`toolpath-git`) uses `git2` (libgit2 bindings), not shelling out to git
- Claude conversation data lives in `~/.claude/projects/` as JSONL files; `toolpath-claude` reads these directly
