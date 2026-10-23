# toolpath-copilot

Derive [Toolpath](https://toolpath.net) provenance documents from
**GitHub Copilot CLI** session logs — the standalone agentic CLI distributed
as the npm package [`@github/copilot`](https://www.npmjs.com/package/@github/copilot)
(command `copilot`).

> ⚠️ **Preview — schema partly verified.** Copilot CLI's `events.jsonl` format
> is undocumented. This crate was first built from docs + community
> reverse-engineering, then **verified against a first-hand session capture at
> `copilotVersion` 1.0.67**: the line envelope, `session.start` context,
> `tool.execution_*` (incl. `result.content`), `assistant.message`
> (`reasoningText`/`outputTokens`), and `system.message` are confirmed.
> Event types that didn't occur in that session (`subagent.*`, `skill.invoked`,
> `hook.*`, `abort`, `session.shutdown`, compaction) and the `checkpoints/`
> format remain unverified — the parser stays deliberately tolerant (payload
> inline or nested, multiple key spellings, unknown events preserved). See the
> format reference + verification checklist at
> [`docs/agents/formats/copilot-cli/`](../../docs/agents/formats/copilot-cli/README.md).

## What it reads

Sessions live under `~/.copilot/session-state/<session-id>/` (override the root
with `COPILOT_HOME`):

- `events.jsonl` — the append-only event stream this crate parses into a
  conversation.
- `workspace.yaml` — git context (root / repository / branch / revision), read
  into `Path.base` (tolerant key-scan parser; the schema is reverse-engineered).
- `checkpoints/` — file snapshots (not yet consumed; see
  [file-fidelity.md](../../docs/agents/formats/copilot-cli/file-fidelity.md)).

It also tolerates the legacy `history-session-state/` location.

## Usage

```rust,no_run
use toolpath_copilot::{CopilotConvo, derive};

let convo = CopilotConvo::new();

// List sessions (newest first).
for meta in convo.list_sessions()? {
    println!("{}  {}", meta.id, meta.first_user_message.unwrap_or_default());
}

// Read one and derive a Toolpath `Path`.
let session = convo.read_session("<session-id-or-prefix>")?;
let path = derive::derive_path(&session, &derive::DeriveConfig::default());
# Ok::<(), toolpath_copilot::ConvoError>(())
```

The forward pipeline is `EventReader::read_session_dir` → `provider::to_view`
(producing a `toolpath_convo::ConversationView`) → the shared
`toolpath_convo::derive_path`. The derived `Path` carries the
`agent-coding-session` kind, per-turn tool invocations classified into
toolpath's `ToolCategory` ontology, and — for file writes whose args carry full
content — a `raw` unified-diff perspective.

## Status & limitations

- **Both directions.** Forward (`path p import/list/show copilot`, `path share`)
  and reverse via `CopilotProjector` (`path p export copilot`, `path resume`).
  Resume writes `~/.copilot/session-state/<id>/` + a `session-store.db`
  `sessions` row (only ever a fresh id). ✅ **Verified against copilot CLI
  1.0.61 on 2026-10-15**: four projected sessions loaded in the real
  `copilot --resume` and the model answered from their history (an earlier
  run at 1.0.67 predates the fixes that made this hold).
  The loader's writer requirements (UUID ids, offset ISO timestamps, `turnId`,
  `messageId`, …) are documented in
  [writing-compatible.md](../../docs/agents/formats/copilot-cli/writing-compatible.md).
- **File fidelity is best-effort.** Copilot records file edits as tool-call args
  plus separate `checkpoints/`/`rewind-snapshots/` rather than inline diffs
  (unlike Codex). This crate synthesizes a `raw` diff when the tool args contain
  full file content; edit-shaped calls without the full file yield a structural
  change only. Snapshot-based diff reconstruction is deferred.
- **Token accounting** sums per-message `outputTokens` for the session output
  total. The session's input and cache totals exist only on
  `session.shutdown`; every counter no message reports is taken from there and
  put, verbatim, on the session's last assistant turn, so the turns (and the
  derived document's steps) sum to the session total. That turn's input/cache
  are the session's, not its own spend: Copilot records no per-message input.
  No input-token total is observed in an open (not shut down) session.

See the [known-gaps doc](../../docs/agents/formats/copilot-cli/known-gaps-and-sourcing.md)
for the full list and the checklist to run once a real session is captured.

## License

MIT
