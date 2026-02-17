---
layout: base.njk
title: Crates
nav: crates
permalink: /crates/
---

# Crates

Toolpath is a Rust workspace of focused, composable crates. The core crate has zero dependencies beyond serde; satellite crates add derivation sources and rendering.

```
toolpath-cli (binary: path)
 +-- toolpath           (core types)
 +-- toolpath-git     -> toolpath
 +-- toolpath-claude  -> toolpath
 +-- toolpath-dot     -> toolpath
```

No cross-dependencies between satellite crates. `toolpath` is the sole shared foundation.

{% for crate in crates %}

<div class="crate-card">
  <h3><code>{{ crate.name }}</code></h3>
  <div class="version">v{{ crate.version }}</div>
  <p>{{ crate.description }}</p>
  <p class="role">{{ crate.role }}</p>
  <div class="crate-links">
    <a href="{{ crate.docs }}">docs.rs</a>
    <a href="{{ crate.crate }}">crates.io</a>
  </div>
</div>
{% endfor %}

## Using the libraries

### Core types

```rust
use toolpath::v1::{Step, Path, Base, Document};

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
use toolpath::v1::query;

let ancestors = query::ancestors(&path.steps, &path.path.head);
let dead_ends = query::dead_ends(&path.steps, &path.path.head);
let by_actor = query::filter_by_actor(&path.steps, "agent:");
let artifacts = query::all_artifacts(&path.steps);
```

### Git derivation

```rust
use toolpath_git::{derive, DeriveConfig};

let repo = git2::Repository::open(".")?;
let config = DeriveConfig {
    remote: "origin".into(),
    title: None,
    base: None,
};
let doc = derive(&repo, &["main".into()], &config)?;
```

### DOT rendering

```rust
use toolpath::v1::Document;
use toolpath_dot::{render, RenderOptions};

let dot_string = render(&doc, &RenderOptions::default());
// Pipe through `dot -Tpng` for an image
```

### Visual conventions (toolpath-dot)

| Actor type | Color                                                                           |
| ---------- | ------------------------------------------------------------------------------- |
| `human:*`  | <span class="actor-human" style="padding: 0.15em 0.5em;">Copper (light)</span>  |
| `agent:*`  | <span class="actor-agent" style="padding: 0.15em 0.5em;">Copper (medium)</span> |
| `tool:*`   | <span class="actor-tool" style="padding: 0.15em 0.5em;">Pencil gray</span>      |
| `ci:*`     | <span class="actor-ci" style="padding: 0.15em 0.5em;">Pencil gray dashed</span> |
| Dead ends  | <span class="actor-dead" style="padding: 0.15em 0.5em;">Red dashed</span>       |
