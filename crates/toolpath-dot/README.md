# toolpath-dot

Generate Graphviz DOT visualizations from Toolpath documents.

## Overview

This crate renders any Toolpath `Document` (Step, Path, or Graph) as a Graphviz DOT string. Steps are colored by actor type, dead ends are highlighted, and the DAG structure is preserved visually.

Depends only on `toolpath` -- no external rendering libraries.

## Usage

```rust
use toolpath::Document;
use toolpath_dot::{render, RenderOptions};

let doc = Document::from_json(json_str)?;
let dot = render(&doc, &RenderOptions::default());
println!("{}", dot);
```

Pipe through Graphviz to produce images:

```bash
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png
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
| `render(doc, options)` | Render any `Document` variant |
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
