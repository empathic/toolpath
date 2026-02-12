# toolpath-git

Derive Toolpath provenance documents from git repository history.

## Overview

This crate converts git commit history into Toolpath documents using `git2` (libgit2 bindings). Each commit becomes a Step with the commit's diff as the `raw` change perspective, the author mapped to an actor string, and the commit message as intent.

- **Single branch** produces a `Path`
- **Multiple branches** produce a `Graph` of paths

## Usage

```rust
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
```

## API

| Function | Description |
|---|---|
| `derive(repo, branches, config)` | Main entry point. Single branch -> Path, multiple -> Graph |
| `derive_path(repo, spec, config)` | Derive a Path from a single branch |
| `derive_graph(repo, specs, config)` | Derive a Graph from multiple branches |
| `get_repo_uri(repo, remote)` | Get the repository URI from a remote |
| `normalize_git_url(url)` | Normalize a git URL (strip `.git`, convert SSH to HTTPS) |
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
