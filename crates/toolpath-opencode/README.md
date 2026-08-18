# toolpath-opencode

Derive Toolpath provenance documents from
[opencode](https://opencode.ai) session databases.

opencode stores every conversation — sessions, messages, typed
parts, tool calls, filesystem snapshots — in a single SQLite
database at `~/.local/share/opencode/opencode.db`. This crate reads
that database directly (read-only) and maps it to Toolpath
documents, so every opencode-assisted change has a traceable origin
with real unified diffs reconstructed from the sibling snapshot
git repositories.

## Overview

- **Reading**: open `opencode.db` with `rusqlite`, enumerate
  projects and sessions, materialize typed `Message` / `Part` rows
  from the JSON-in-TEXT `data` columns.
- **Provider**: implements
  [`toolpath_convo::ConversationProvider`](https://docs.rs/toolpath-convo),
  pairing `tool` parts by `callID`, folding `reasoning` parts onto
  `Turn.thinking`, and capturing `step-start` / `step-finish`
  snapshot SHAs on `Turn.extra["opencode"]` for file-artifact
  reconstruction.
- **Derivation**: produces `toolpath::v1::Path` documents. When the
  matching snapshot git repo is still on disk, file changes per
  turn surface as sibling artifacts with a real `git diff` as the
  `raw` perspective — no diff reconstruction from tool output
  required.

## Mapping

| opencode source | Toolpath destination |
|---|---|
| `session.id` | `path.id = path-opencode-<first-8>` |
| `session.directory` + `project.worktree` | `path.base = { file://<worktree>, ref: <project-id> }` |
| User `message` | Step with `actor: "human:user"` |
| Assistant `message` | Step with `actor: "agent:<modelID>"` |
| `reasoning` part | `Turn.thinking` (plaintext — safe to render) |
| `text` part | appended to `Turn.text` |
| `tool` part (state: completed) | `Turn.tool_uses[]` with `input` + `result` |
| `tool` part (state: error) | same, `result.is_error = true` |
| `step-start` / `step-finish` snapshot SHA | sibling file artifacts on the turn — unified diff as `raw` |
| `compaction` / `retry` / `patch` / unknown parts | `ConversationEvent` |

## Usage

```rust,no_run
use toolpath_opencode::{OpencodeConvo, derive::{DeriveConfig, derive_path}};

let manager = OpencodeConvo::new("/Users/alex");
let session_id = "ses_24ee4deb6ffeWw7ZKWNVoOAgjD";
let convo = manager.read_session(session_id)?;
let path = derive_path(&convo, &DeriveConfig::default());
# Ok::<(), toolpath_opencode::ConvoError>(())
```

## CLI

```bash
path p list   opencode [--json]
path p import opencode --session <id> [--pretty]
```

## What's *not* read

- `auth.json` — live OAuth / API-key credentials. Deliberately
  never read to avoid leaking secrets into derived documents.
- `log/*.log` — process logs (same content already lands in the DB).
- `bin/`, `cache/`, `node_modules/` — transient caches.
- `event`, `event_sequence` tables — reserved for the sync service;
  observed empty in practice.

See
[`docs/agents/formats/opencode.md`](../../docs/agents/formats/opencode.md)
in the workspace for the full on-disk format reference.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) — core provenance types
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) — provider-agnostic conversation abstraction
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) — Claude Code provider
- [`toolpath-gemini`](https://crates.io/crates/toolpath-gemini) — Gemini CLI provider
- [`toolpath-pi`](https://crates.io/crates/toolpath-pi) — Pi (pi.dev) provider
- [`path-cli`](https://crates.io/crates/path-cli) — unified CLI (`cargo install path-cli`)
