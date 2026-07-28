# Known gaps, sourcing, and the verification checklist

## Verification methodology

Everything in this folder came from one machine on 2026-07-27/28, at Amp
`0.0.1785170481-ga5b614` (tool-input-shape mining re-run at
`0.0.1785228716-gedda19` after a self-update), via four independent channels:

1. **Live captures.** Three private threads: a pre-existing install-session
   thread (6 messages), a trivial thread plus one continuation (4 messages),
   and a verbatim [feature-elicit](../../feature-elicit.md) run (24 messages,
   11 tool calls, 1 sub-agent). Total spend $0.41.
2. **The CLI's own output.** `amp --help`, `amp threads --help`,
   `amp threads list/export/markdown/usage`, `amp usage`. Tagged `[official]`
   where Amp states something about itself.
3. **Local artifact inspection.** `~/.local/share/amp`, `~/.cache/amp`,
   `~/.config/amp` — full `find` inventory before and after each capture, plus
   line-level analysis of the per-thread logs. `secrets.json` was never read.
4. **Bundle string mining.** `strings -n 6 ~/.amp/bin/amp` (~116k lines) over
   the Bun single-file executable's embedded minified JS, searched along five
   independent axes (token fields, env vars, stream envelope, local storage,
   server API) with an adversarial re-check pass. Tagged `[reverse-eng]`.

Claims were cross-checked between channels wherever possible. Two examples of
that paying off:

- The bundle shows `usage` attaching to exactly two zod schemas (assistant
  message, assistant delta) and the reducer **overwriting** rather than
  accumulating — independently confirming the per-message reading that the
  captures' non-monotonic `outputTokens` already implied.
- The bundle shows the snake_case stream shape is an *export mapping* built
  with `input_tokens: T.inputTokens || 0`, so a **zero in the stream can be a
  coerced absent value** while the export omits the field. A reason to treat
  the export as canonical.

### Environment safety notes for anyone repeating this

- `amp` **auto-enables execute mode when stdout is redirected** `[official]`.
  Under any output capture, a mistyped subcommand tries to start a billable
  thread. Always pass an explicit subcommand.
- An isolated `HOME`/`XDG_DATA_HOME` **without `AMP_API_KEY` opens a browser
  login flow that can complete unattended** and mint a real access token into
  the scratch directory. It happened twice here; both tokens were deleted.

## Resolved by these captures ✓

- **Reconstruction route** — `amp threads export <id>`, a server fetch. The
  local per-thread log is metadata + tool inputs only.
  [RECON.md Q1](RECON.md#q1--reconstruction).
- **Token pattern** — clean per-message; `token_usage` on the turn,
  no `group_id`, no `attributed_token_usage`, no `breakdowns`.
  [RECON.md Q2](RECON.md#q2--tokens).
- **`totalInputTokens` is derived**, not cumulative — verified on all 17 usage
  objects across three threads.
- **`maxInputTokens` is context-window capacity**, never a spend.
- **Isolation** — set `HOME` *and* all three XDG vars; each moves a
  different subset and `XDG_DATA_HOME` alone splits the data dir; no `AMP_*`
  data-dir override exists. [RECON.md Q3](RECON.md#q3--isolation).
- **Stream envelope** — four Claude-Code-compatible line types.
  [RECON.md Q4](RECON.md#q4--envelope).
- **Content model** — `text` / `thinking` / `tool_use` / `tool_result` blocks;
  tool results arrive in the following `user` message, paired by `toolUseID`.
- **`run.result` is polymorphic by tool** (4 shapes observed).
- **File fidelity is Codex-grade** — `apply_patch` results embed real unified
  diffs with `additions`/`deletions`. [file-fidelity.md](file-fidelity.md).
- **Errors are `exitCode`, not `status`** — `run.status` was `"done"` even for
  the deliberate failure, and the stream's `is_error` was `false`.
- **Sub-agent turns are unavailable** — `Task` returns a string; no child
  thread is created; `parent_tool_use_id` stays `null`.
- **No git state is recorded anywhere** in a thread.
- **`v` is a revision counter**, not a schema version.
- **`threads list`'s `Messages` column counts human messages**, not protocol
  messages.
- **Real fixture** — the feature-elicit capture lives at
  [`test-fixtures/amp/`](../../../../test-fixtures/amp/README.md) as
  `convo.json` (canonical export) + `stream.jsonl` (teed stream).

## Still open

1. **Whether a *document* can be imported into a thread.** Still open, but
   no longer gating: L2 is achieved without it via the first-party CLI
   two-step (see [writing-compatible.md](writing-compatible.md)). The REST
   route `POST /api/thread-actors` **was** exercised and does not work — it
   answers `201 Created` and creates no thread. The real `/import` is a
   Rivet actor fetch behind a wsToken handshake, still unexercised.
   `[observed + reverse-eng, 0.0.1785170481-ga5b614]`
2. **Non-`openai` providers.** Every capture ran `agentMode: medium` →
   `gpt-5.6-sol` → `provider: "openai"`. An Anthropic- or Google-backed mode
   would plausibly change the `thinking` block shape (`openAIReasoning` is
   clearly provider-specific) and possibly the usage fields. `[unverified]`
3. **When `thinking` is populated.** 5 of 12 blocks carried text and 7 were
   `""`, with no obvious pattern. Whether `--stream-json-thinking` ever yields
   more than the export does is untested on a thinking-rich thread.
4. **Error shapes for non-shell tools.** Only a failing `shell_command` was
   captured. `apply_patch` / `Task` / `skill` failure payloads are
   `[unverified]`.
5. **Delete in `apply_patch`.** `*** Add File:` and `*** Update File:`
   observed; a delete verb is assumed to exist. `[unverified]`
6. **Compaction.** Amp has `amp threads compact` in its orientation surface.
   No compaction event was observed, and whether it rewrites the thread
   document or appends a marker is unknown. Apply the "never stamp a
   cumulative counter" rule defensively when it appears.
7. **Non-success `result` subtypes** in the stream. Only
   `result/success` seen; error and max-turns variants `[unverified]`.
8. **The remaining 25 tools.** Only `shell_command`, `apply_patch`, `Task`,
   and `skill` were exercised live. Bundle mining at `gedda19` (piece 05)
   fixed the **input** shapes for `finder {query}`, `web_search {query}`,
   and `read_web_page {url}` (`[reverse-eng]`, see
   [events.md](events.md)); `librarian`, `oracle`, and the
   thread/schedule/orb families stay `[unverified]`, and **result** shapes
   for everything unexercised remain unknown.
8b. **A bundle code path emits `usage` on the stream's `result` line**,
   which no `--stream-json` capture produced. Unexplained; low priority
   (the export is canonical either way).
9. **`amp threads raw`** — 403 for a thread the account owns. Staff-gated?
10. **Multiplayer / shared / labelled threads** — `sharedGroupIDs`,
    `openExpiresAt`, `workspaceID`, `projectID` were all null/empty in every
    capture.

## Server API surface `[reverse-eng]`

Extracted from the bundle. Only `POST /api/thread-actors` was ever called
directly (piece 03; it returned `201 Created` and created no thread — see
[writing-compatible.md](writing-compatible.md)). The rest is unexercised.

- **Base URL** `https://ampcode.com/`, overridable by `AMP_URL` `[official]`.
  All HTTP goes through one request builder that attaches
  `Authorization: Bearer <apiKey>` and `Content-Type: application/json`.
- **HTTP paths**: `POST /api/internal?<method>` (the main RPC funnel),
  `POST /api/thread-actors`, `POST /api/thread-actors/<id>`,
  `POST /api/user-actor-credentials`, `GET /api/threads/find?q&limit&offset`,
  `POST /api/threads/<id>/diff-captures` (+ `/publish`),
  `POST /api/attachments`, `POST /api/telemetry`,
  `GET /auth/cli-login?authToken&callbackPort`, and git-over-HTTPS at
  `/git/@<namespace>/<repo>`.
- **Realtime** is **RivetKit actors**, not a bare websocket URL: a gateway at
  `<base>/actors` with `rvt-*` query parameters and `rivet_*` websocket
  subprotocol tokens carrying auth. Two actor kinds: `threadActor` (keyed by
  thread id) and `userActor` (keyed by user id). Overridable by
  `RIVET_PUBLIC_ENDPOINT`.
- **Thread-actor endpoints**: `WS /`, `POST /rpc`, **`POST /import`**,
  `GET /raw-thread`, `GET /inference-messages`, `GET /context-analysis`,
  `GET /skills`, `POST /inference`.
- **User-actor RPCs**: `getRecentThreads`, `registerRunner`,
  `runnerHeartbeat`, `unregisterRunner`, `setThreadPinned`;
  event `runnerIntentsUpdated`.
- **Transport** is JSON-RPC where the method *is* the message `type` (e.g.
  `client_append_user_msg`), request ids shaped
  `thread-client-<epoch>-<n>`.

The gateway embeds a hardcoded public client key; it is baked into a
publicly-distributed binary and is deliberately **not** reproduced here.

## Verify once we have more samples

- [ ] A thread from a **non-openai** agent mode (`-m high` / `-m ultra`) —
      does `thinking`/`provider`/`usage` change shape?
- [ ] A thread with **`amp threads compact`** applied.
- [ ] A **failing `apply_patch`** and a **failing `Task`**.
- [ ] An `apply_patch` that **deletes** a file, and one that touches
      **multiple files** in one call.
- [ ] A thread using `finder` / `librarian` / `oracle` / `web_search`, to fix
      their categories and result shapes.
- [ ] `--stream-json-thinking` on a thread that demonstrably reasons.
- [ ] A **shared / unlisted / workspace-visibility** thread, to see how
      `visibility` and `sharedGroupIDs` populate.
- [ ] An **abort** (Ctrl-C mid-turn) — the delta enum includes `aborted` and
      `error` states we never saw resolve into a message.
- [ ] A **non-success** `result` line from `--stream-json`.
- [ ] A **long** thread, to see whether `export` paginates or truncates.

## Sources

- **First-hand captures** (2026-07-27, `0.0.1785170481-ga5b614`) —
  `test-fixtures/amp/`, plus scratch captures not committed.
- **`amp --help` / `amp threads --help`** at the same version — the only
  first-party documentation consulted; treated as `[official]`.
- **The shipped Bun bundle** `~/.amp/bin/amp` — `[reverse-eng]`. Version
  specific by construction; re-mine after any `amp update`.
- **`roger-amp-plan/PLAN.md`** §Evidence base — the read-only recon from the
  planning session, at the older `gd1fcef` build. Superseded by this folder
  where the two disagree; its `AMP_*` env list and thread-model sketch held up.

No third-party write-ups or community reverse-engineering were used — none
were found for Amp's session format.
