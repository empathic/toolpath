# The JSONL envelope

An OpenClaw session is one **append-only JSONL file**: the first non-blank
line is a session header, and every subsequent non-blank line is one
**session-tree entry**. Crucially, it is **not a flat message log** — the
entries form a tree (a DAG) via `parentId`, and the "current" conversation
is the path from a leaf back to the root. Branching, the visible-leaf
pointer, model changes, and compaction are all just additional entry types
appended to the same file.

The format self-identifies as **version 3**.

> **Two code layers, one file.** The transcript is read/written by the
> agent-core harness storage (`packages/agent-core/src/harness/session/jsonl-storage.ts`,
> types in `harness/types.ts`) and mirrored by the gateway session manager
> (`src/agents/sessions/session-manager.ts`, writer
> `src/config/sessions/transcript-jsonl.ts`). They use slightly different
> type names (`SessionTreeEntry`/`MessageEntry` vs
> `SessionEntry`/`SessionMessageEntry`) for what serializes to the same
> JSON. This doc uses the agent-core names. See
> [known-issues.md](known-issues.md#two-code-layers-for-one-format).

## The header line

Always the first line. From `jsonl-storage.ts:22-29`:

```json
{
  "type": "session",
  "version": 3,
  "id": "0190ab00-aaaa-7bbb-8ccc-000000000001",
  "timestamp": "2026-06-30T12:00:00.000Z",
  "cwd": "/home/u/proj",
  "parentSession": "/home/u/.openclaw/agents/main/sessions/<parent>.jsonl"
}
```

| Field | Shape | Req? | Notes |
|---|---|---|---|
| `type` | `"session"` | required | Header discriminant. |
| `version` | int | required | Format version. **Reader hard-rejects `version !== 3`** (`jsonl-storage.ts:80-82`). |
| `id` | string (UUID) | required | Session id. Matches the transcript filename stem. |
| `timestamp` | ISO-8601 string | required | Session creation time (`new Date().toISOString()`). |
| `cwd` | string | required | Working directory the session ran in. |
| `parentSession` | string (path) | optional | Path to a parent session **file** when this session was forked. Cross-file lineage; see [lineage.md](lineage.md). |

The header maps to `JsonlSessionMetadata { id, createdAt, cwd, path,
parentSessionPath? }`; `loadJsonlSessionMetadata` reads only this first line
(`jsonl-storage.ts:150-173`). Notably **absent** from the header: model,
title, and channel — those are carried elsewhere (see
[entry-types.md](entry-types.md) and
[channels-and-actors.md](channels-and-actors.md)).

## The entry base

Every non-header line shares `SessionTreeEntryBase`
(`harness/types.ts:353-364`):

```ts
interface SessionTreeEntryBase {
  type: string;              // discriminator (see entry-types.md)
  id: string;                // entry id, unique within this file
  parentId: string | null;   // parent entry id; null for a root entry
  timestamp: string;         // ISO-8601 string (NB: differs from inner message.timestamp)
  appendMode?: "side";       // this row advances the raw cursor, not the visible leaf
}
```

| Field | Shape | Req? | Notes |
|---|---|---|---|
| `type` | string | required | Entry discriminant. Ten known values; see [entry-types.md](entry-types.md). Unknown future values are tolerated. |
| `id` | string | required | **8-char prefix of a UUIDv7** (`uuidv7().slice(0,8)`, `storage-base.ts:35-43`), with collision retry. Time-sortable, **file-scoped only** — do not assume global uniqueness. |
| `parentId` | string \| null | required | Parent entry's `id`; `null` marks a root. The DAG backbone. |
| `timestamp` | ISO-8601 string | required | Entry time. Validated with `Date.parse`, but writers emit ISO-8601. **This is a string**; the inner `message.timestamp` is epoch-ms — see [messages.md](messages.md). |
| `appendMode` | `"side"` | optional | Marks a side-branch row that moves the raw append cursor without selecting a model-visible branch (`storage-base.ts:46-50`). |

## The tree and the visible leaf

`parentId` forms a tree/DAG. The active conversation is reconstructed by
walking from the current leaf to the root (`getPathToRoot`,
`storage-base.ts`). Anything not on the leaf's ancestry is a **dead end** —
which lines up exactly with toolpath's implicit dead-end model (steps not on
`path.head`'s ancestry).

The **visible leaf is a separate pointer**, not simply "the last line."
It is maintained by `leaf` control rows (`LeafEntry`,
`harness/types.ts:435-440`):

```json
{ "type": "leaf", "id": "0190ab14", "parentId": "0190ab13",
  "timestamp": "2026-06-30T12:00:05.600Z", "targetId": "0190ab13" }
```

| Field | Shape | Notes |
|---|---|---|
| `targetId` | string \| null | The entry the branch currently points at (the visible head). |
| `appendParentId` | string \| null (optional) | Overrides the raw parent for the next append when it differs from the visible leaf. |

So to find the live conversation you (1) replay the entries, tracking the
leaf via `leaf` rows and `appendMode:"side"`, then (2) walk `targetId →
root` over `parentId`. The helpers `leafIdUpdateAfterEntry` /
`appendParentIdAfterEntry` (`storage-base.ts:46-78`) encode the exact rules.

## Robustness notes

- **Blank lines are tolerated.** The reader splits on `\n` and filters empty
  lines (`jsonl-storage.ts:184`); do not assume one record per physical line
  with no gaps.
- **The inner message body is not validated on read.** `parseEntryLine`
  validates only the envelope (`type`, `id`, `parentId`, `timestamp`, and
  the `leaf`-specific fields), then casts the rest through
  (`jsonl-storage.ts:108-147`). The `message`/content-block shapes in
  [messages.md](messages.md) are the **producer** contract — trusted on
  read, not runtime-checked. There is no Zod/TypeBox over persisted lines.
- **Unknown entry `type`s are skipped gracefully**, not fatal
  (`storage-base.ts:46-69`) — OpenClaw expects to add entry types over time.
