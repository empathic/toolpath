# toolpath-cursor

Derive Toolpath provenance documents from [Cursor](https://cursor.com)
sessions.

Cursor stores every conversation across two places — a thin JSONL
transcript at `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`
(the agent's I/O log, lossy) and a rich SQLite bubble store at
`~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
(the source of truth). This crate reads the SQLite store (read-only,
with content-blob lookup) and maps it to Toolpath documents.

## Overview

- **Reading**: open `state.vscdb` via `rusqlite`, read
  `ItemTable.composer.composerHeaders` for the cross-workspace
  composer index, then per composer pull `cursorDiskKV.composerData:<id>`
  and the `cursorDiskKV.bubbleId:<comp>:<bubble>` rows. Content-
  addressed file blobs under `composer.content.<hash>` are looked up
  while reading so the IR-builder is pure.
- **Provider**: implements
  [`toolpath_convo::ConversationProvider`](https://docs.rs/toolpath-convo),
  pairing user/assistant bubbles into turns, lifting
  `toolFormerData` into `ToolInvocation`, decoding `params` /
  `result` JSON strings, and resolving `edit_file_v2` to real
  before/after file content via the content store.
- **Derivation**: produces `toolpath::v1::Path` documents through
  the shared `toolpath_convo::derive_path`. Edit-tool calls surface
  as sibling artifacts with the full unified diff as `raw`.

## Mapping

| Cursor source | Toolpath destination |
|---|---|
| `composerData.composerId` | `ConversationView.id`, `path.id = path-cursor-<first-8>` |
| `composerData.modelConfig.modelName` | Default `Turn.model` |
| `composerData.name` | `path.meta.title` |
| `composerHeader.workspaceIdentifier.uri.fsPath` | `path.base.uri` (as `file://…`) |
| `composerData.agentBackend` (`"cursor-agent"`) | `producer.name` (`"cursor"`) |
| Bubble `type: 1` (user) | Step with `actor: "human:user"` |
| Bubble `type: 2` (assistant) | Step with `actor: "agent:<model>"` |
| `bubble.allThinkingBlocks[*].text` | `Turn.thinking` |
| `bubble.toolFormerData` | one entry in `Turn.tool_uses[]` |
| `toolFormerData.tool == 38` (edit_file_v2) | `ToolCategory::FileWrite` + `FileMutation` with resolved before/after content and unified diff |
| `toolFormerData.tool == 15` (run_terminal_command_v2) | `ToolCategory::Shell` + `ToolResult { content: output, is_error: exitCode != 0 }` |
| `toolFormerData.tool == 40` (read_file_v2) | `ToolCategory::FileRead` |
| `toolFormerData.tool == 42` (glob_file_search) | `ToolCategory::FileSearch` |
| `bubble.tokenCount` | `Turn.token_usage` |
| `bubble.modelInfo.modelName` | `Turn.model` (overrides composer default) |
| Unknown `toolFormerData.tool` ids | preserved as `ToolInvocation { category: None }` — `name` + `input` still carry the call |

Provider-specific UI metadata (`checkpointId`, `requestId`,
`capabilityType`, `richText`, Lexical editor DOM, the
`anysphere.cursor-commits` checkpoint store, the AI-authorship
SQLite db) is intentionally *not* smuggled through the IR — those
fields don't belong in a cross-harness representation, and the
IR has no `Turn.extra` slot. They round-trip to nothing.

## Usage

```rust,no_run
use toolpath_cursor::{CursorConvo, derive::{DeriveConfig, derive_path}};

let manager = CursorConvo::new("/Users/alex");
let composer_id = "724686cd-875e-47da-a90b-dbc3e523efb8";
let session = manager.read_session(composer_id)?;
let path = derive_path(&session, &DeriveConfig::default());
# Ok::<(), toolpath_cursor::CursorError>(())
```

## What's *not* read

- `~/Library/Application Support/Cursor/User/globalStorage/anysphere.cursor-commits/`
  — per-request file snapshots. Their contents duplicate the
  `composer.content.<hash>` blobs already keyed in `cursorDiskKV`.
- `~/.cursor/ai-tracking/ai-code-tracking.db` — per-line AI-vs-human
  authorship stats. Derivable from the bubble store + git.
- `~/Library/Application Support/Cursor/User/workspaceStorage/<id>/state.vscdb`
  — per-workspace UI state. The cross-workspace `composer.composerHeaders`
  index in the global database covers session listing without it.
- `cursorAuth/*` keys — live OAuth tokens. Never read.
- `~/Library/Application Support/Cursor/logs/` — Chromium / renderer
  logs.

See
[`docs/agents/formats/cursor.md`](../../docs/agents/formats/cursor.md)
in the workspace for the full on-disk format reference.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath)
workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) — core provenance types
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) — provider-agnostic conversation abstraction
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) — Claude Code provider
- [`toolpath-codex`](https://crates.io/crates/toolpath-codex) — Codex CLI provider
- [`toolpath-gemini`](https://crates.io/crates/toolpath-gemini) — Gemini CLI provider
- [`toolpath-opencode`](https://crates.io/crates/toolpath-opencode) — opencode provider
- [`toolpath-pi`](https://crates.io/crates/toolpath-pi) — Pi provider
- [`path-cli`](https://crates.io/crates/path-cli) — unified CLI (`cargo install path-cli`)
