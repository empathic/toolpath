---
layout: base.njk
title: Toolpath
nav: home
---

<div class="hero">
  <h1>Toolpath</h1>
  <p class="tagline">
    Know your tools. A tool-agnostic format for tracking artifact transformation provenance.
    Git blame, but for everything that happens to code &mdash; including the stuff git doesn't see.
  </p>
  <div class="hero-install">
    <span class="prompt">$ </span>cargo install toolpath-cli
  </div>
</div>
<div class="divider"></div>

## The problem

When Claude writes code, `rustfmt` reformats it, and a human refines it, git blame attributes everything to the human's commit. The actual provenance is lost. Dead ends disappear. Tool contributions collapse into whoever typed `git commit`.

Toolpath records **who** changed **what**, **why**, what they tried that didn't work, and how to verify all of it.

## Three core objects

<div class="objects">
  <div class="object-card">
    <h3>Step</h3>
    <p>A single change to artifact(s) by one actor. One commit, one edit, one format pass.</p>
  </div>
  <div class="object-card">
    <h3>Path</h3>
    <p>A DAG of steps with a base context. A PR, a coding session, a branch.</p>
  </div>
  <div class="object-card">
    <h3>Graph</h3>
    <p>A collection of related paths. A release, a sprint, a project.</p>
  </div>
</div>

Steps form a DAG via parent references. Dead ends are implicit: steps not in the ancestry of `path.head`.

<div class="dag-ascii">
              +-- step-3a -- step-4a  <span class="muted">(dead end)</span>
step-1 -- step-2 --+
              +-- step-3b -- step-4b -- step-5b  <span class="muted">(head)</span>
</div>

## What Toolpath adds

| What                   | Git                         | Toolpath                                         |
| ---------------------- | --------------------------- | ------------------------------------------------ |
| Who made the change    | Single author per commit    | Typed actors: `human:`, `agent:`, `tool:`, `ci:` |
| Why they changed it    | Unstructured commit message | `meta.intent` + linked refs                      |
| Abandoned approaches   | Lost when branch is deleted | Dead ends preserved in the DAG                   |
| Multi-actor provenance | Collapsed into one commit   | Each actor gets their own step                   |
| Verification           | GPG on whole commit         | Scoped signatures: author, reviewer, CI          |
| Granularity            | Commit-level                | Sub-commit: multiple steps between commits       |

## Minimal example

A valid Toolpath document can be tiny:

```json
{
  "Step": {
    "step": {
      "id": "step-001",
      "actor": "human:alex",
      "timestamp": "2026-01-29T10:00:00Z"
    },
    "change": {
      "src/main.rs": {
        "raw": "@@ -12,1 +12,1 @@\n-    println!(\"Hello world\");\n+    println!(\"Hello, world!\");"
      }
    }
  }
}
```

No parents (it's the first step). No meta. One file, one perspective. Still valid.

## Quick start

```bash
# Install
cargo install toolpath-cli

# Derive provenance from this repo's git history
path derive git --repo . --branch main --pretty

# Visualize it
path derive git --repo . --branch main | path render dot | dot -Tpng -o graph.png

# Derive from Claude conversation logs
path derive claude --project /path/to/project --pretty

# Query for dead ends
path query dead-ends --input doc.json

# Filter by actor
path query filter --input doc.json --actor "agent:"
```

## Workspace

Toolpath is a Rust workspace of focused crates:

| Crate                                                | What it does                               |
| ---------------------------------------------------- | ------------------------------------------ |
| [`toolpath`](https://docs.rs/toolpath)               | Core types, builders, query API            |
| [`toolpath-git`](https://docs.rs/toolpath-git)       | Derive from git history                    |
| [`toolpath-claude`](https://docs.rs/toolpath-claude) | Derive from Claude conversations           |
| [`toolpath-dot`](https://docs.rs/toolpath-dot)       | Graphviz DOT visualization                 |
| [`toolpath-cli`](https://docs.rs/toolpath-cli)       | Unified CLI (`cargo install toolpath-cli`) |

See [Crates](/crates/) for details, or [docs.rs](https://docs.rs/toolpath) for API reference.
