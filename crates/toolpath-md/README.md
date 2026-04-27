# toolpath-md

Render Toolpath documents as Markdown for LLM consumption.

Provenance data is only useful if you can feed it to the systems that need it.
This crate renders the step DAG as readable Markdown — a narrative an LLM can
reason about. Dead ends are called out explicitly ("here's what was tried and
abandoned"), giving models anti-examples alongside the successful path.

## Overview

Renders any Toolpath `Document` (Step, Path, or Graph) as a Markdown string.
Steps are topologically sorted, dead ends are marked and summarized, and the
output includes enough anchoring information (step IDs, artifact paths, actor
strings) for an LLM to reference back into the original document.

Depends only on `toolpath` — no template engines, no external dependencies.

## Usage

```rust
use toolpath::v1::Document;
use toolpath_md::{render, RenderOptions};

let json_str = r#"{"Step":{"step":{"id":"s1","actor":"human:alex","timestamp":"T"},"change":{}}}"#;
let doc = Document::from_json(json_str).unwrap();
let md = render(&doc, &RenderOptions::default());
assert!(md.contains("# s1"));
```

Pipe into an LLM for contextual assistance:

```bash
path derive git --repo . --branch main | path render md | pbcopy
# Paste into Claude/ChatGPT: "here's what I've tried, help me with the next step"
```

## Render options

```rust
use toolpath_md::{RenderOptions, Detail};

let options = RenderOptions {
    detail: Detail::Summary,  // or Detail::Full for inline diffs
    front_matter: false,      // emit YAML front matter with metadata
};
```

### Detail levels

- **`Summary`** (default) — file paths with diffstat (`+3 -1`). Compact, fits in tight context windows.
- **`Full`** — inline diffs as fenced code blocks. Use when the LLM needs to reason about specific line changes.

### Front matter

When `front_matter: true`, the output starts with YAML front matter containing
machine-readable metadata (document type, step count, actor list, artifact list,
dead end count). Useful for LLM workflows that parse structured preambles.

## API

| Function | Description |
|---|---|
| `render(doc, options)` | Render any `Document` variant |
| `render_step(step, options)` | Render a single Step |
| `render_path(path, options)` | Render a Path with its step DAG |
| `render_graph(graph, options)` | Render a Graph with all paths |

## Output structure

### Step

```markdown
# step-001
**Actor:** `human:alex`
**Timestamp:** 2026-01-29T10:00:00Z
> Fix greeting
- `src/main.rs` (+1 -1)
```

### Path

```markdown
# Add email validation
**Base:** `github:org/repo` @ `main`
**Head:** `step-004`
**Steps:** 5 | **Artifacts:** 2 | **Dead ends:** 1

## Timeline
### step-001 — human:alex
### step-002a — agent:claude ❌ dead end
### step-002 — agent:claude
...

## Dead Ends
- **step-002a** — Regex approach (abandoned) | Parent: `step-001`
```

### Graph

Renders a summary table of all paths, then each path as a subsection.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) — core types and query API
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) — Graphviz DOT visualization
- [`toolpath-git`](https://crates.io/crates/toolpath-git) — derive from git history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) — derive from Claude conversations
- [`path-cli`](https://crates.io/crates/path-cli) — unified CLI (`cargo install path-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) — full format specification
