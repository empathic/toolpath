# Writing a session Copilot CLI will resume

Empirically-discovered constraints a synthesized `events.jsonl` (+ its
`session-store.db` row) must satisfy for `copilot --resume <id>` to load it,
rather than erroring with `Session file is corrupted`.

> **Source: live `copilot --resume` runs at `copilotVersion` 1.0.67.** Unlike
> the rest of this folder (parsing = reading), these are *writer* constraints —
> Copilot's loader validates the file and rejects malformed envelopes with a
> specific message. **This list grows as new rejections are observed**; it is
> not known to be complete. Each row cites the verbatim error that revealed it.

## Loader requirements (observed)

| # | Requirement | Verbatim rejection | Status |
|---|---|---|---|
| 1 | Every event envelope **`id` must be a UUID string** (not `e1`, not a bare integer). | `invalid session event envelope: \`id\` must be a UUID string` | `[observed, 1.0.67]` |
| 2 | Every event **`timestamp` must be an ISO 8601 date-time with a timezone offset** (e.g. `2026-07-01T14:31:29.298Z` or `…+00:00`). Applies to **every** event, `session.start` included. | `invalid session event envelope: \`timestamp\` must be an ISO 8601 date-time string with a timezone offset` | `[observed, 1.0.67]` |
| 3 | `parentId` (when present) is an event id, so the same UUID rule applies. | — (not independently observed; applied defensively) | `[inferred]` |
| 4 | The session must have a row in `session-store.db`'s `sessions` table, or the resume picker / id lookup won't find it. | — (from the DB's role as the resume index) | `[inferred]` — see [session-store-db.md](session-store-db.md) |

## How `toolpath-copilot`'s projector satisfies these

`CopilotProjector` (in `toolpath-copilot/src/project.rs`), invoked by
`path resume` / `path p export copilot` via `project_copilot`:

- **UUID envelope ids (req 1, 3):** each event gets a syntactically-valid,
  per-session-unique, v4-shaped UUID (`00000000-0000-4000-8000-<counter>`);
  `parentId` chains off the previous event's id. Deterministic — Copilot
  validates the *shape*, not randomness.
- **Offset-bearing timestamps (req 2):** every event (including
  `session.start`) is stamped with a valid RFC 3339 timestamp. The projector
  picks a base (the first offset-bearing turn timestamp, else the view's
  `started_at`) and normalizes each turn's timestamp against it, so no event
  ever lacks a timezone offset.
- **`sessions` row (req 4):** `project_copilot` writes an `INSERT OR REPLACE`
  into `session-store.db` (fresh session UUID only — never mutates existing
  sessions), plus `session-state/<id>/{events.jsonl,workspace.yaml}`.

## ⚠️ Still being verified

`copilot --resume` acceptance is validated **only against live runs the user
relays** — this environment can't run the authenticated CLI. The two envelope
rules above are confirmed; the loader may enforce more (required fields like
`messageId`/`turnId`, `sessionId` matching the directory, checkpoints, a
`schema_version` row, …). When a new rejection surfaces, add a row above with
its verbatim message and teach the projector to satisfy it. Track open items in
[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md).
