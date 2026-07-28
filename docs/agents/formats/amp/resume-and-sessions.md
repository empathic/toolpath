# Listing, resuming, and the writer surface

The command surface a `toolpath-amp` forward provider reads through, and the
surface a future `AmpProjector` / `path resume --harness amp` would have to
match.

`[official]` claims come from `amp --help` / `amp threads --help` at
`0.0.1785170481-ga5b614`.

## `amp threads` `[official]`

```
new            [n]        Create a new thread
continue       [c]        Continue an existing thread
list           [l, ls]    List all threads
usage                     Show usage information for a thread
visibility     [v]        Show or set default visibility for this repository
search         [find]     Search threads
label                     Add labels to a thread
share          [s]        Share a thread
  multiplayer             Open/close the thread for contributions
report                    Send a diagnostic report to Amp support
rename         [r]        Rename a thread
archive                   Archive a thread
delete                    Delete a thread
markdown       [md]       Render thread as markdown
export                    Export a thread as JSON
raw            [raw-thread]  Export raw actor thread data as JSON
```

Options: `--include-archived`, `--limit <n>`, `--offset <n>`.

Top-level siblings: `amp last` (continue the last thread), `amp top` (live
active threads), `amp usage` (account credit balance).

## Reading

### `amp threads list`

```
Title                     Last Updated  Visibility  Messages  Thread ID
Filesystem tool exercise  2m ago        Private     1         T-019fa4db-…
Reply with ok             4m ago        Private     2         T-019fa4d8-…
General assistance        1h ago        Private     2         T-019fa447-…
```

- **Server-backed** — run under a fresh isolated `HOME` it authenticated and
  then listed all three of the account's threads, none of which had any local
  representation in that home. It reads no thread data from disk.
- **Relative timestamps only** (`2m ago`). There is no `--format json`/`tsv`
  option; a machine-readable listing needs `export` per thread, or the
  `/api/threads/find` endpoint (`[reverse-eng]`, see
  [known-gaps-and-sourcing.md](known-gaps-and-sourcing.md#server-api-surface-reverse-eng)).
- `Messages` counts **human** messages, not protocol messages — see
  [session-state.md](session-state.md#message-counting-is-not-what-threads-list-shows).
- Archived threads are hidden without `--include-archived`, and `amp -x`
  archives new threads by default.

This is a real constraint on `p list amp`: producing the TSV columns PLAN.md
piece 02 specifies (`id · last_activity · line_count · cwd ·
first_user_message`) requires **one `export` call per thread**, because
`list` supplies none of `cwd`, `first_user_message`, or an absolute
timestamp.

### `amp threads export <id>` — the canonical read

Pretty-printed JSON of the whole thread. See [events.md](events.md). This is
what `toolpath-amp` should parse.

### `amp threads markdown <id>`

A human-readable render — YAML front-matter (`title`, `threadId`, `created`,
`agentMode`) then `## User` / `## Assistant` sections with fenced tool inputs
and results. Useful for eyeballing a capture; lossy (no usage, no ids, no
block states), so not a derivation source.

### `amp threads raw <id>`

Documented as "Export raw actor thread data as JSON". It **failed** on a
thread the account owns:

```
Error: Failed to export raw thread: Raw thread request failed (403):
{"error":"You do not have permission to access this resource"}
```

`[observed, 0.0.1785170481-ga5b614]` — presumably staff-only. It maps to the
thread actor's `GET /raw-thread` `[reverse-eng]`. Do not depend on it.

## Resuming

`amp threads continue <thread-id>` resumes a thread; it accepts the same
global options as a fresh run, including `-x`, `--stream-json`, and
`--stream-json-thinking`. Verified live: continuing the trivial thread with
`-x` appended two messages and streamed them, with cache-read tokens showing
the prior context was reused (`cache_read_input_tokens: 16393`).

So the resume invocation for `path resume --harness amp` is:

```
amp threads continue <thread-id>
```

`[observed]` for the `-x` form; the interactive-TUI form is `[inferred]` from
the same command shape.

**Resolved in piece 03:** a thread created with `amp threads new` and seeded
via `amp threads continue <id> -x` resumes normally and carries its context.
That — not document import — is how `path resume --harness amp` works. See
[writing-compatible.md](writing-compatible.md).

## The writer surface

Piece 03 must pick a route. Three candidates, in decreasing order of what the
evidence supports:

### 1. `--stream-json-input` — documented, in-process `[official]`

```
--stream-json-input   Read JSON Lines user messages from stdin.
                      Requires both --execute and --stream-json.
```

The accepted line shape, from the bundle's zod schema `[reverse-eng]` and the
CLI's own error hint `[official]`:

```json
{"type":"user","steer":false,"message":{"role":"user","content":[
  {"type":"text","text":"…"},
  {"type":"image","source":{"type":"base64","media_type":"image/png","data":"…"}}
]}}
```

Limits `[reverse-eng]`: ≤4 images per line, ≤1048576 bytes of stdin.
`steer` is an Amp-only extension with no Claude Code counterpart.

This feeds **user messages into a live turn** — it is not a thread-import
mechanism, so on its own it cannot reconstruct an assistant/tool transcript.

### 2. The thread actor's `POST /import` `[reverse-eng]`

The bundle contains an import call against the per-thread Rivet actor:

```js
.fetch("/import", { method: "POST", …, body: JSON.stringify({ thread: …}) })
```

with a 409 tolerated (already imported) and a sibling
`POST /api/thread-actors/<id>` whose failure message is
`"Failed to mark thread <id> as imported"`.

**Probed in piece 03 and rejected as a route.** A plain HTTPS
`POST /api/thread-actors` answers `201 Created` and creates no thread
(`amp threads export` immediately after: "does not exist"). The call above
is not REST — it is a *Rivet actor* fetch, addressed through the gateway
with a `wsToken` from a prior credentials exchange, so it is unreachable
without reimplementing that protocol. See
[writing-compatible.md](writing-compatible.md).
`[observed, 0.0.1785170481-ga5b614]`

### 3. Local fabrication — ruled out

There is no local thread store to write into (Q1). Copilot-style "write the
session file and let the CLI load it" is **not available**. Any projector must
go through the server.

## Consequences for `path resume`

- `argv_for(Harness::Amp)` = `["threads", "continue", "<id>"]`.
- The projector cannot mint a local id and hand it to `amp` — the id must be
  one the **server** knows. Either the import call returns it, or the flow is
  "create a thread, then import into it".
- The `--no-archive-after-execute` flag matters for any non-interactive
  verification script, or the thread vanishes from `list`.
- An isolated-home verification script **must** set `AMP_API_KEY`, or Amp
  opens a browser login that can silently succeed (see
  [RECON.md](RECON.md#️-the-gotcha-that-must-go-in-verify-amp-livesh)).
