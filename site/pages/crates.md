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

<svg class="topo topo-wide" viewBox="0 0 900 90" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
  <path d="M0,70 Q100,30 250,60 Q400,85 500,40 Q600,10 750,55 Q850,80 900,50" stroke="#b5652b" stroke-width="1" opacity="0.10" fill="none"/>
  <path d="M0,75 Q120,40 260,65 Q410,88 510,48 Q620,18 760,60 Q860,82 900,58" stroke="#8a8078" stroke-width="1" opacity="0.07" fill="none"/>
  <ellipse cx="500" cy="38" rx="60" ry="20" stroke="#b5652b" stroke-width="1" opacity="0.12" fill="none"/>
  <ellipse cx="502" cy="36" rx="35" ry="12" stroke="#b5652b" stroke-width="1" opacity="0.18" fill="none"/>
  <circle cx="503" cy="35" r="3" fill="#b5652b" opacity="0.20"/>
</svg>

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
