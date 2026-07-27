# toolpath-amp

Derive [Toolpath](https://toolpath.net) provenance documents from
**Amp** ([ampcode.com](https://ampcode.com)) agent threads — the CLI/IDE
coding agent distributed as `amp`.

> ⚠️ **Preview — schema reverse-engineered.** Amp's thread export format is
> undocumented. Everything this crate parses was verified against first-hand
> captures at Amp version `0.0.1785164324-gd1fcef` / `0.0.1785170481-ga5b614`
> (2026-07-27); each thread pins its own creating build in
> `env.initial.platform.clientVersion`. Amp versions are build-timestamped and
> churn fast — expect drift, and trust the per-thread version anchor over the
> running binary. See the format reference at
> [`docs/agents/formats/amp/`](../../docs/agents/formats/amp/README.md).

## What it reads

Amp threads are **server-authoritative** — no complete local record exists on
disk (the per-thread log under `~/.cache/amp/logs/threads/` is content-free
telemetry). The canonical artifact is the export document produced by:

```bash
amp threads export <thread-id>
```

This crate fetches that export by shelling out to the `amp` CLI (inheriting
its login), or reads a pre-exported JSON file. The export carries the full
conversation: text, thinking summaries, tool calls with parsed results,
per-message token usage, and file diffs from `apply_patch` results.

## Usage

```rust,no_run
use toolpath_amp::{AmpConvo, derive::{DeriveConfig, derive_path}};

let convo = AmpConvo::new();
let session = convo.read_session("T-019fa4db-29cf-70c9-8d9b-81524df70e52")?;
let path = derive_path(&session, &DeriveConfig::default());
# Ok::<(), toolpath_amp::ConvoError>(())
```

For tests (or offline use), inject a fetcher that reads exported files from a
directory instead of invoking `amp`:

```rust,no_run
use std::sync::Arc;
use toolpath_amp::{AmpConvo, io::DirFetcher};

let convo = AmpConvo::with_fetcher(Arc::new(DirFetcher::new("/path/to/exports")));
```

## Fidelity notes

- **Tokens**: Amp reports clean per-message usage on every assistant message
  (`inputTokens`, `outputTokens`, `cacheReadInputTokens`,
  `cacheCreationInputTokens`). Each turn carries its own `token_usage`; the
  session total is the field-wise sum. `totalInputTokens` (a derived sum) and
  `maxInputTokens` (the context-window capacity) are deliberately dropped —
  summing either would fabricate spend.
- **Thinking**: Amp's `thinking` blocks frequently carry an empty summary; the
  real chain of thought is sealed in an encrypted provider blob. Empty
  summaries map to `None`, not `Some("")`.
- **File changes**: `apply_patch` results embed real unified diffs — the
  derived `Path` gets a `raw` perspective on every mutated file.
- **Git**: Amp records no VCS state in a thread; `Path.base` never carries a
  commit or branch.
- **Sub-agents**: `Task` returns a bare string and no child thread is
  created; `DelegatedWork.turns` is always empty.
