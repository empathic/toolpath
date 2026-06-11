# toolpath-codex

Derive Toolpath provenance documents from Codex CLI session logs.

When Codex CLI writes your code, the rollout file — the reasoning,
tool calls, shell output, and file patches — is the provenance. This
crate reads those rollout files directly from
`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` and maps them to Toolpath
documents so every Codex-assisted change has a traceable origin.

## Overview

- **Reading**: parse Codex rollout JSONL into strongly-typed
  `RolloutLine` records.
- **Provider**: implements
  [`toolpath_convo::ConversationProvider`](https://docs.rs/toolpath-convo),
  pairing function-call and tool-call outputs by `call_id` and folding
  `exec_command_end` / `patch_apply_end` events into `Turn` state.
- **Derivation**: produces `toolpath::v1::Path` documents. File
  changes from `patch_apply_end` surface as sibling artifacts with
  the real unified diff as the `raw` perspective — no fidelity loss.

## Mapping

| Codex source | Toolpath destination |
|---|---|
| `session_meta.id` | `path.id = path-codex-<first-8>` |
| `session_meta.cwd` + `git.commit_hash` | `path.base = { file://<cwd>, ref: <commit> }` |
| `response_item.message` (user) | Step with `actor: "human:user"` |
| `response_item.message` (assistant) | Step with `actor: "agent:<model>"` |
| `response_item.message` (developer) | Step with `actor: "system:codex"` |
| `response_item.reasoning.encrypted_content` | `Turn.thinking` (opaque string) |
| `response_item.function_call` + `function_call_output` paired by `call_id` | `Turn.tool_uses[].{input,result}` |
| `response_item.custom_tool_call` (e.g. `apply_patch`) | same, with raw `input` string preserved |
| `event_msg.patch_apply_end.changes[file]` | Sibling `ArtifactChange` on that step with `raw = unified_diff` |
| Other `event_msg` types | `ConversationEvent` on the `ConversationView` |

## Usage

```rust,no_run
use toolpath_codex::{CodexConvo, derive::{DeriveConfig, derive_path}};

let manager = CodexConvo::new();
let session_id = "019dabc6-8fef-7681-a054-b5bb75fcb97d";
let convo = manager.read_session(session_id)?;
let path = derive_path(&convo, &DeriveConfig::default()).expect("derive");
# Ok::<(), toolpath_codex::ConvoError>(())
```

## CLI

```bash
path p list   codex [--json]
path p import codex --session <uuid|filename-stem> [--pretty]
```

## What's *not* read

- `~/.codex/state_5.sqlite` — a cheap index over the rollout files.
  Could be used for fast listing on very large histories; not needed
  for v1.
- `~/.codex/logs_1.sqlite` and `~/.codex/log/codex-tui.log` —
  application logs, not conversation content.
- `~/.codex/history.jsonl` — redundant user-prompt cache.
- `~/.codex/memories`, `skills`, `shell_snapshots` — not conversation.

See [`docs/agents/formats/codex.md`](../../docs/agents/formats/codex.md)
in the workspace for the full on-disk format reference.

## Part of Toolpath

This crate is part of the [Toolpath](https://github.com/empathic/toolpath) workspace. See also:

- [`toolpath`](https://crates.io/crates/toolpath) — core provenance types
- [`toolpath-convo`](https://crates.io/crates/toolpath-convo) — provider-agnostic conversation abstraction
- [`toolpath-claude`](https://crates.io/crates/toolpath-claude) — Claude Code provider
- [`toolpath-gemini`](https://crates.io/crates/toolpath-gemini) — Gemini CLI provider
- [`toolpath-pi`](https://crates.io/crates/toolpath-pi) — Pi (pi.dev) provider
- [`path-cli`](https://crates.io/crates/path-cli) — unified CLI (`cargo install path-cli`)
