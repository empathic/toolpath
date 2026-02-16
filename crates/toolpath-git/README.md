# toolpath-git

Derive Toolpath provenance documents from git repository history.

Git knows *what* changed but loses *why* — commit messages are unstructured,
multi-actor provenance collapses into a single author, and abandoned
approaches disappear entirely. This crate bridges that gap by converting
git history into structured Toolpath documents where every commit becomes
a step with typed actors, intent, and full diff provenance.

## Overview

Uses `git2` (libgit2 bindings). Each commit becomes a Step with the commit's diff as the `raw` change perspective, the author mapped to an actor string, and the commit message as intent.

- **Single branch** produces a `Path`
- **Multiple branches** produce a `Graph` of paths

## Usage

```rust,no_run
use toolpath_git::{derive, DeriveConfig};

let repo = git2::Repository::open(".")?;
let config = DeriveConfig {
    remote: "origin".into(),
    title: None,
    base: None,
};

// Single branch -> Path document
let doc = derive(&repo, &["main".into()], &config)?;

// Multiple branches -> Graph document
let doc = derive(&repo, &["main".into(), "feature".into()], &config)?;

// Branch with start point
let doc = derive(&repo, &["main:HEAD~10".into()], &config)?;
# Ok::<(), anyhow::Error>(())
```

## API

| Function | Description |
|---|---|
| `derive(repo, branches, config)` | Main entry point. Single branch -> Path, multiple -> Graph |
| `derive_path(repo, spec, config)` | Derive a Path from a single branch |
| `derive_graph(repo, specs, config)` | Derive a Graph from multiple branches |
| `list_branches(repo)` | List local branches with summary metadata |
| `get_repo_uri(repo, remote)` | Get the repository URI from a remote |
| `normalize_git_url(url)` | Normalize a git URL (strip `.git`, convert SSH to short form) |
| `slugify_author(name, email)` | Create a URL-safe slug from author info |

## Branch specifications

Branches are specified as `"name"` or `"name:start"`:

- `"main"` -- all commits on main (with auto-detection of where to start)
- `"main:HEAD~10"` -- last 10 commits on main
- `"feature:abc123"` -- feature branch from commit abc123

The `DeriveConfig.base` field overrides per-branch starts globally.

## Mapping

| Git concept | Toolpath concept |
|---|---|
| Commit | Step |
| Branch | Path |
| Commit hash | `step.id` (first 8 chars) |
| Author | `step.actor` as `human:slug` |
| Commit message | `meta.intent` |
| Diff | `change[file].raw` (unified diff) |
| Commit hash | `meta.source.revision` |
| Remote URL | `path.base.uri` |

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`toolpath-cli`](https://crates.io/crates/toolpath-cli) -- unified CLI (`cargo install toolpath-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
