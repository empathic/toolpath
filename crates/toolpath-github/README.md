# toolpath-github

Derive Toolpath provenance documents from GitHub pull requests.

A pull request captures more than code changes — reviews, inline comments,
CI checks, and discussion all contribute to the final artifact. This crate
maps the full PR lifecycle into a single Toolpath Path where every event
becomes a Step in the provenance DAG.

## Overview

Uses the GitHub REST API. Every PR event type becomes a Step with typed
actors and artifact changes. Commits carry code diffs; reviews and comments
are changes to `review://` artifacts; CI checks are changes to `ci://` artifacts.

## Usage

```rust,no_run
use toolpath_github::{derive_pull_request, resolve_token, DeriveConfig};

let token = resolve_token()?;
let config = DeriveConfig {
    token,
    include_ci: true,
    include_comments: true,
    ..Default::default()
};

let path = derive_pull_request("owner", "repo", 42, &config)?;
# Ok::<(), anyhow::Error>(())
```

## Artifact URI scheme

| Artifact type | URI pattern | Example |
|---|---|---|
| Source file | bare path (relative to base) | `src/main.rs` |
| Review comment thread | `review://{file}#L{line}` | `review://src/main.rs#L42` |
| PR conversation | `review://conversation` | `review://conversation` |
| Review decision | `review://decision` | `review://decision` |
| CI check result | `ci://checks/{name}` | `ci://checks/build` |

The `review://` and `ci://` schemes are platform-agnostic — they generalize
to GitLab MRs, Gerrit, Phabricator, etc.

## Mapping

| GitHub concept | Toolpath type | Details |
|---|---|---|
| Pull request | Path | id: `pr-{number}` |
| Commit | Step | actor: `human:{login}`, per-file raw diffs |
| Review comment | Step | artifact: `review://{path}#L{line}` |
| PR comment | Step | artifact: `review://conversation` |
| Review (approve/reject) | Step | artifact: `review://decision` |
| CI check run | Step | actor: `ci:{app}`, artifact: `ci://checks/{name}` |

## API

| Function | Description |
|---|---|
| `resolve_token()` | Resolve GitHub token from `GITHUB_TOKEN` or `gh auth token` |
| `derive_pull_request(owner, repo, pr, config)` | Derive a Path from a PR |
| `list_pull_requests(owner, repo, config)` | List PRs with summary info |
| `extract_issue_refs(body)` | Parse "Fixes #N" / "Closes #N" from text |

## CLI

```bash
# Derive a Toolpath document from a GitHub PR
path derive github --repo owner/repo --pr 42 --pretty

# Without CI checks or comments
path derive github --repo owner/repo --pr 42 --no-ci --no-comments

# List pull requests
path list github --repo owner/repo --json
```

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) -- core types and query API
- [`toolpath-git`](https://crates.io/crates/toolpath-git) -- derive from git repository history
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) -- derive from Claude conversations
- [`toolpath-dot`](https://crates.io/crates/toolpath-dot) -- Graphviz DOT rendering
- [`toolpath-cli`](https://crates.io/crates/toolpath-cli) -- unified CLI (`cargo install toolpath-cli`)
- [RFC](https://github.com/empathic/toolpath/blob/main/RFC.md) -- full format specification
