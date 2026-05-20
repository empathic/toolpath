# toolpath-dot

Generate Graphviz DOT visualizations from Toolpath documents.

Provenance data is only useful if you can see it. This crate renders the
step DAG as a Graphviz diagram so you can see at a glance who did what,
where the dead ends branched off, and how human/agent/tool contributions
interleave — color-coded by actor type.

## Overview

Renders a Toolpath `Graph` (the single root document type) as a Graphviz DOT string. Steps are colored by actor type, dead ends are highlighted, and the DAG structure is preserved visually. Single-path graphs use a path-focused layout; multi-path graphs use clustered subgraphs.

Depends only on `toolpath` -- no external rendering libraries. You'll need [Graphviz](https://graphviz.org/) installed to convert DOT output to images (`dot -Tpng`).

## Usage

```rust
use toolpath::v1::Graph;
use toolpath_dot::{render, RenderOptions};

let json_str = r#"{"graph":{"id":"g1"},"paths":[]}"#;
let graph = Graph::from_json(json_str).unwrap();
let dot = render(&graph, &RenderOptions::default());
assert!(dot.contains("digraph"));
```

Pipe through Graphviz to produce images:

```bash
path p import git --repo . --branch main --no-cache | path p render dot | dot -Tpng -o graph.png
```

## Render options

```rust
use toolpath_dot::RenderOptions;

let options = RenderOptions {
    show_files: true,           // list changed files in step labels
    show_timestamps: true,      // show timestamps in step labels
    highlight_dead_ends: true,  // dashed red border on dead-end steps (default)
};
```

## API

| Function | Description |
|---|---|
| `render(graph, options)` | Render a `Graph` (single-path graphs use the path layout) |
| `render_step(step, options)` | Render a single Step |
| `render_path(path, options)` | Render a Path with its step DAG |
| `render_graph(graph, options)` | Render a Graph with subgraph clusters per path |
| `actor_color(actor)` | Get the fill color for an actor type |
| `escape_dot(s)` | Escape a string for DOT labels |
| `escape_html(s)` | Escape a string for HTML-like labels |

## Visual conventions

| Actor type | Color |
|---|---|
| `human:*` | Blue (`#cce5ff`) |
| `agent:*` | Green (`#d4edda`) |
| `tool:*` | Yellow (`#fff3cd`) |
| `ci:*` | Purple (`#e2d5f1`) |
| Dead ends | Red dashed border (`#ffcccc`) |
| BASE node | Gray ellipse |

Color-coding makes multi-actor provenance scannable at a glance: you can immediately see where human work ends and agent work begins, and the red dashed borders draw your eye to abandoned approaches without cluttering the main path.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`path-cli`](https://crates.io/crates/path-cli) -- unified CLI (`cargo install path-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
