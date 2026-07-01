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
| 3 | Every event must **carry a `parentId` key** — a UUID string, or explicit `null` for the root (`session.start`). Omitting it is rejected; it can't just be absent. | `invalid session event envelope: \`parentId\` must be a UUID string or null` | `[observed, 1.0.67]` |
| 4 | The session must have a row in `session-store.db`'s `sessions` table, or the resume picker / id lookup won't find it. | — (from the DB's role as the resume index) | `[inferred]` — see [session-store-db.md](session-store-db.md) |
| 5 | `session.start`'s `data` must include **`startTime`** (offset-bearing ISO 8601). The loader checks required top-level fields one at a time; the projector emits the full observed 1.0.67 set (`sessionId`, `version`, `producer`, `copilotVersion`, `startTime`, `contextTier`, `context`, `alreadyInUse`, `remoteSteerable`) to avoid repeat rejections. | `missing field \`startTime\`` | `[observed, 1.0.67]` |
| 6 | Turn-scoped events (`assistant.turn_start`/`.message`/`.turn_end`, `tool.execution_start`/`_complete`) must carry a **`turnId`** — the string index of the assistant turn (`"0"`, `"1"`, …). Not present on `session.*` or `user.message`. | `missing field \`turnId\`` | `[observed, 1.0.67]` |
| 7 | `assistant.message` and `assistant.turn_end` must carry a **`messageId`** (UUID string); `turn_end` references the message it closes. | `missing field \`messageId\`` | `[observed, 1.0.67]` |
| 8 | `tool.execution_start`/`_complete` (and the `assistant.message.toolRequests` mirror) need a **non-empty `toolCallId`** — an *empty string* is rejected as missing. The projector synthesizes one when the IR tool id is empty, stable across the request/start/complete for a call. | `missing field \`toolCallId\`` | `[observed, 1.0.67]` |

## How `toolpath-copilot`'s projector satisfies these

`CopilotProjector` (in `toolpath-copilot/src/project.rs`), invoked by
`path resume` / `path p export copilot` via `project_copilot`:

- **UUID envelope ids (req 1, 3):** each event gets a syntactically-valid,
  per-session-unique, v4-shaped UUID (`00000000-0000-4000-8000-<counter>`);
  `parentId` is **always emitted** — the previous event's UUID, or `null` on
  the root `session.start`. Deterministic — Copilot validates the *shape*, not
  randomness.
- **Offset-bearing timestamps (req 2):** every event (including
  `session.start`) is stamped with a valid RFC 3339 timestamp. The projector
  picks a base (the first offset-bearing turn timestamp, else the view's
  `started_at`) and normalizes each turn's timestamp against it, so no event
  ever lacks a timezone offset.
- **`session.start` shape (req 5):** `session_start_data` emits the full
  observed 1.0.67 top-level field set (incl. `startTime`, stamped with the same
  base timestamp as the envelope) plus a `context` block with cwd/git.
- **`turnId` (req 6):** `push_assistant` stamps a per-assistant-turn index
  (`"0"`, `"1"`, …) on every event it emits — turn start/message/end and each
  tool execution. `user.message`/`session.*` don't get one.
- **`messageId` (req 7):** each assistant turn gets a stable UUID (distinct
  namespace from event ids) on its `assistant.message` and matching
  `assistant.turn_end`.
- **`sessions` row (req 4):** `project_copilot` writes an `INSERT OR REPLACE`
  into `session-store.db` (fresh session UUID only — never mutates existing
  sessions), plus `session-state/<id>/{events.jsonl,workspace.yaml}`.

## ✅ Verified (copilotVersion 1.0.67)

A session projected by `CopilotProjector` — the real `~/.copilot` session
`53a1f220…` imported to a Path and projected back — **loads and resumes cleanly
in `copilot --resume`.** The seven requirements above were discovered and
cleared one at a time (the loader validates one required field per line, so the
failure marched line 1 → line 7 as each was fixed); after the last, Copilot
resumed the full conversation context (↑22k tokens) and responded normally.

The loop was driven locally with an isolated `COPILOT_HOME` (a copy of
`~/.copilot/config.json` for auth), so it never touched real sessions. To
reproduce:

```sh
H=$(mktemp -d); cp ~/.copilot/config.json "$H/"
sqlite3 "$H/session-store.db" 'CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, repository TEXT, host_type TEXT, branch TEXT, summary TEXT, created_at TEXT, updated_at TEXT);'
# project some Path doc into $H (path resume with a no-op copilot on PATH), then:
COPILOT_HOME="$H" copilot --resume <projected-id> -p "reply ok"
```

**Caveat:** verified for the tested session shape (shell + file tools, single
project). A session exercising features that shape didn't (sub-agents, MCP
tools, compaction) could surface a further required field — re-run the loop and
add a row here if one appears. Open items:
[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md).
