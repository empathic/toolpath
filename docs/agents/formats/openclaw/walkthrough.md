# A session, line by line

This is a representative OpenClaw session read top to bottom, with each line
cross-linked back to the reference docs. Because we have **no first-hand
on-disk sample** yet, the JSON below is **reconstructed from the upstream
types**, not a captured fixture — ids and timestamps are invented and it is
illustrative only. Real files are one JSON object per physical line; lines
are pretty-printed here for readability.

The scenario: a WhatsApp DM where the user asks the agent to fix a bug, the
model reads a file, and the context later compacts.

---

## Line 1 — session header

```json
{ "type": "session", "version": 3,
  "id": "0190ab00-aaaa-7bbb-8ccc-000000000001",
  "timestamp": "2026-06-30T12:00:00.000Z",
  "cwd": "/home/u/proj",
  "parentSession": null }
```

The first line is always the header
([jsonl-envelope.md](jsonl-envelope.md#the-header-line)). `version` must be
`3` or the reader rejects the file. `id` matches the filename stem; this is
a fresh session, so there's no `parentSession`. The session's channel/peer
is **not** here — it's in the routing key under which `sessions.json`
filed this file (`agent:main:whatsapp:direct:15555550123`, say); see
[channels-and-actors.md](channels-and-actors.md).

## Line 2 — the user's message

```json
{ "type": "message", "id": "0190ab10", "parentId": null,
  "timestamp": "2026-06-30T12:00:01.000Z",
  "message": { "role": "user",
    "content": [ { "type": "text", "text": "Fix the bug in x.ts" } ],
    "timestamp": 1751284801000 } }
```

A `message` entry ([entry-types.md §message](entry-types.md#message)) with
`role:"user"`. `parentId:null` makes it a root of the DAG. Note the **two
timestamps**: the entry's is an ISO string, the inner message's is epoch ms
([messages.md](messages.md#the-two-timestamp-encodings)). The human is the
WhatsApp peer from the key — there is no sender field on the message itself
([channels-and-actors.md](channels-and-actors.md#who-is-the-human)).

## Line 3 — model selection

```json
{ "type": "model_change", "id": "0190ab11", "parentId": "0190ab10",
  "timestamp": "2026-06-30T12:00:01.500Z",
  "provider": "anthropic", "modelId": "claude-..." }
```

A `model_change` marker ([entry-types.md §model_change](entry-types.md#model_change)).
It chains off the user message via `parentId`. The same model also appears
on each assistant message, so this is the explicit signal but not the only
record of it.

## Line 4 — the assistant turn

```json
{ "type": "message", "id": "0190ab12", "parentId": "0190ab11",
  "timestamp": "2026-06-30T12:00:05.000Z",
  "message": { "role": "assistant",
    "content": [
      { "type": "thinking", "thinking": "Let me read the file", "thinkingSignature": "opaque..." },
      { "type": "text", "text": "I'll read x.ts first." },
      { "type": "toolCall", "id": "call_1", "name": "read_file", "arguments": { "path": "src/x.ts" } } ],
    "api": "anthropic-messages", "provider": "anthropic", "model": "claude-...",
    "usage": { "input": 1200, "output": 340, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1540,
               "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
    "stopReason": "toolUse", "timestamp": 1751284805000 } }
```

One assistant message with **three content blocks in order**: a `thinking`
block, a `text` block, and a `toolCall`
([messages.md §content-blocks](messages.md#content-blocks)). The `usage`
here is this turn's spend, a per-step delta — sum these for a session total
([usage.md](usage.md#shape-a-persisted-per-message-usage-source-of-truth)).
The `thinking` text is present but its **token count is not** in `usage`
([usage.md](usage.md#reasoning-tokens-are-runtime-only)). `stopReason` is
`toolUse` because the turn ended on a tool call.

## Line 5 — the tool result

```json
{ "type": "message", "id": "0190ab13", "parentId": "0190ab12",
  "timestamp": "2026-06-30T12:00:05.500Z",
  "message": { "role": "toolResult", "toolCallId": "call_1", "toolName": "read_file",
    "content": [ { "type": "text", "text": "<file contents>" } ],
    "isError": false, "timestamp": 1751284805500 } }
```

The result is a **separate** `message` entry of role `toolResult`, linked to
the call by `toolCallId == "call_1"`
([tools.md](tools.md#call-and-result-are-separate-entries)). Had the read
failed, `isError` would be `true` with the error text in `content` — there's
no dedicated error field. Note this was a *read*; had it been an edit, there
would still be **no diff** on disk, only the tool arguments
([tools.md](tools.md#file-operations-tool-input-only-no-raw-diff)).

## Line 6 — the visible-leaf pointer

```json
{ "type": "leaf", "id": "0190ab14", "parentId": "0190ab13",
  "timestamp": "2026-06-30T12:00:05.600Z", "targetId": "0190ab13" }
```

A `leaf` control row ([entry-types.md §leaf](entry-types.md#leaf)) sets the
visible head to `0190ab13`. This is why you can't just take the last line as
the tip — the live branch is whatever the latest `leaf` points at, walked
back to root over `parentId`
([jsonl-envelope.md](jsonl-envelope.md#the-tree-and-the-visible-leaf)).

## Line 7 — a compaction boundary

```json
{ "type": "compaction", "id": "0190ab15", "parentId": "0190ab13",
  "timestamp": "2026-06-30T12:10:00.000Z",
  "summary": "## Goal\n...", "firstKeptEntryId": "0190ab12",
  "tokensBefore": 54000,
  "details": { "readFiles": [ "src/x.ts" ], "modifiedFiles": [] },
  "fromHook": false }
```

Later, context compacts ([entry-types.md §compaction](entry-types.md#compaction)).
Everything before `firstKeptEntryId` is replaced by `summary` at replay, and
assistant `usage` at/under this boundary is **zeroed on disk** — so a naive
sum across this point undercounts
([usage.md](usage.md#compaction-zeroes-stale-usage)). If this had instead
been turned into a *new* session via `sessions.compaction.branch`, you'd get
a fresh file whose root references this one
([lineage.md](lineage.md#3-compaction-as-a-new-branch)).

---

## Reconstructing the conversation

To turn this file into a linear transcript:

1. Read line 1 for session metadata; reject if `version != 3`.
2. Replay lines 2..N, tracking the visible leaf via `leaf` rows and skipping
   `appendMode:"side"` side-branch rows for the visible thread.
3. Walk the final `targetId` to the root over `parentId`; that ancestry is
   the live conversation. Off-ancestry entries are dead ends.
4. Apply any `compaction` boundary: drop pre-`firstKeptEntryId` history in
   favor of `summary`.
5. For the channel/peer actors, parse the routing key from `sessions.json`,
   not the transcript.
