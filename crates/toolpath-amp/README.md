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
> **✅ Resume verified in amp `0.0.1785170481-ga5b614`** (and Claude-Code→amp
> at `0.0.1785228716-gedda19`): a projected session resumes in the real `amp`
> CLI and the model answers probing questions about the prior session
> correctly — with the fidelity ceiling documented below.

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

## What it writes (reverse path)

`AmpProjector` inverts `to_view` into an `amp threads export`-shaped document:
position-stable ids, preserved delegation ids, token re-expansion (the derived
`totalInputTokens` is regenerated; capacity is never invented), and foreign
tool names remapped into Amp's native vocabulary with arg shapes the Amp UI
renders (`apply_patch {patchText}` synthesis, `finder {query}`,
`web_search {query}`, …).

Because threads are server-authoritative and Amp accepts no document import
(the REST-looking route answers `201 Created` and creates nothing), resume
goes through the first-party CLI: `amp threads new` creates a fresh
server-side thread and `amp threads continue <id> -x` seeds it with a rendered
transcript. That is **context transfer, not a native-block import** — the
resumed model reasons about the prior work correctly (verified live), but
`amp threads export` on the resumed thread will not resemble the source. The
full-fidelity projected document is what `path p export amp --output` emits.
Contract, evidence, and verbatim probe results:
[`docs/agents/formats/amp/writing-compatible.md`](../../docs/agents/formats/amp/writing-compatible.md).
