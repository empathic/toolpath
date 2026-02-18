# Changelog

All notable changes to the Toolpath workspace are documented here.

## 0.1.4 — toolpath / 0.2.0 — toolpath-cli

### toolpath 0.1.4

- Added `extra: HashMap<String, serde_json::Value>` with `#[serde(flatten)]` to `PathMeta`, `StepMeta`, and `GraphMeta`, matching the schema's `additionalProperties: true` and enabling round-trip fidelity for extension fields

### toolpath-cli 0.2.0

- **Breaking:** `path track` session files are now valid `{"Path": {...}}` Toolpath documents at all times. Tracking bookkeeping (buffer cache, sequence mappings) lives in `meta.track` and is stripped on export/close. Any Toolpath tool can read a live session file — `path validate`, `path query dead-ends`, `path render dot` all work mid-session without export.

## 0.1.3 — toolpath / 0.1.2 — all other crates

### All crates

- Improved README documentation: added motivating "why" context, cross-links between crates
- READMEs render as docs.rs landing pages with compilable examples

## 0.1.2 — toolpath / 0.1.1 — all other crates

### toolpath 0.1.2

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page via `include_str!`
- All code examples in the README are compiled as doc tests

### toolpath-git 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `list_branches` to the API table in the README
- Added doc examples to `normalize_git_url` and `slugify_author`
- Module-level doc example for `derive`
- README now renders as the docs.rs landing page

### toolpath-claude 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- README now renders as the docs.rs landing page

### toolpath-dot 0.1.1

- Added `repository`, `keywords`, and `categories` to crate metadata
- Added module-level documentation with usage example
- Added field docs to `RenderOptions`
- README now renders as the docs.rs landing page

### toolpath-cli 0.1.1

- Renamed crate from `path` to `toolpath-cli` (binary still called `path`)
- Moved crate directory from `crates/path/` to `crates/toolpath-cli/`
- Added `repository`, `keywords`, and `categories` to crate metadata
- Added `haiku` subcommand
- Installable via `cargo install toolpath-cli`

## 0.1.1 — toolpath

- Initial metadata-only release (added `repository`, `keywords`, `categories`)

## 0.1.0 — all crates

- Initial public release
- Core types: `Document`, `Graph`, `Path`, `Step` with builder API
- Query operations: `ancestors`, `dead_ends`, `filter_by_actor`, `filter_by_artifact`, `filter_by_time_range`
- Git derivation via `git2` (single branch -> Path, multiple branches -> Graph)
- Claude conversation derivation with filesystem watching
- Graphviz DOT rendering with actor color-coding and dead-end highlighting
- CLI with `derive`, `query`, `render`, `validate`, `list`, `merge`, `track` commands
- JSON Schema (`schema/toolpath.schema.json`)
- 11 example documents
- Full format specification (RFC.md)
