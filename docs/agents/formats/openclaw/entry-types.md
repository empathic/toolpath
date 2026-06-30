# Entry types

Every non-header line carries a `type` discriminant. There are **ten**
known entry types (`harness/types.ts:443-454`). A reader that handles only
`message` will silently drop model changes, compaction boundaries, branch
summaries, labels, and the leaf pointers that tell it which branch is even
live.

## Summary table

| `type` | Carries a message? | Purpose |
|---|---|---|
| `message` | yes (`message`) | A conversational turn: user / assistant / toolResult / bashExecution. The dominant type. |
| `model_change` | no | Records a model/provider switch mid-session. |
| `thinking_level_change` | no | OpenClaw-specific reasoning-budget toggle. |
| `compaction` | no | Context-compaction boundary: history before `firstKeptEntryId` is replaced by `summary`. |
| `branch_summary` | no | Summary of an abandoned branch when navigating away from it. |
| `custom` | no | Harness/app marker **not** replayed into model context. |
| `custom_message` | yes (`content`) | Harness/app content that **is** replayable into context. |
| `label` | no | Display label for a target entry (last write wins). |
| `session_info` | no | Session name/title (last write wins). |
| `leaf` | no | Visible-head pointer; see [jsonl-envelope.md](jsonl-envelope.md#the-tree-and-the-visible-leaf). |

All ten extend [`SessionTreeEntryBase`](jsonl-envelope.md#the-entry-base)
(`type` / `id` / `parentId` / `timestamp` / `appendMode?`). Fields below are
the *additional* ones each variant carries.

---

## `message`

`{ type:"message", message: AgentMessage }` (`harness/types.ts:367-370`).
The `message` object holds the role, content, and (for assistant turns) the
provider/usage metadata. Roles realistically seen on disk: `user`,
`assistant`, `toolResult`, `bashExecution`. Full treatment in
[messages.md](messages.md); tool-call/result correlation in
[tools.md](tools.md).

> The role union also names `custom` / `branchSummary` / `compactionSummary`,
> but those are **not** stored as `message` entries — they live as their own
> entry types (`custom_message`, `branch_summary`, `compaction`) and are
> reconstructed into role-bearing messages at read time
> (`session.ts:46-99`).

## `model_change`

`{ type:"model_change", provider, modelId }` (`harness/types.ts:379-382`).
A mid-session model/provider switch. The active model is also redundantly
recorded on every `assistant` message (`provider`/`model`/`api`), so a
reader can track the model without relying on these markers — but they're
the explicit signal.

## `thinking_level_change`

`{ type:"thinking_level_change", thinkingLevel }` (`harness/types.ts:373-376`).
OpenClaw's reasoning-budget toggle (analogous to a thinking-effort setting).
Informational for provenance.

## `compaction`

`{ type:"compaction", summary, firstKeptEntryId, tokensBefore, details?, fromHook? }`
(`harness/types.ts:386-393`).

| Field | Shape | Notes |
|---|---|---|
| `summary` | string | Structured markdown summary of the dropped history. |
| `firstKeptEntryId` | string | Entries strictly before this id are represented by `summary`; the tail is replayed. |
| `tokensBefore` | int | Estimated context tokens before compaction. |
| `details` | `{ readFiles: string[], modifiedFiles: string[] }` | Optional; the concrete shape `CompactionDetails`. |
| `fromHook` | bool | Optional; true if produced by an app hook rather than the built-in summarizer. |

At replay, history before `firstKeptEntryId` is dropped and replaced by a
synthetic `compactionSummary` message. See [lineage.md](lineage.md) and the
usage caveat in [usage.md](usage.md#compaction-zeroes-stale-usage).

## `branch_summary`

`{ type:"branch_summary", fromId, summary, details?, fromHook? }`
(`harness/types.ts:396-402`).

| Field | Shape | Notes |
|---|---|---|
| `fromId` | string | Entry id of the abandoned branch's source leaf (`"root"` when null). |
| `summary` | string | Summary of the abandoned branch. |
| `details` | `{ readFiles: string[], modifiedFiles: string[] }` | Optional; `BranchSummaryDetails`. |
| `fromHook` | bool | Optional. |

Appended by `Session.moveTo(...)` when navigating away from a branch
(`session.ts:268-289`). Reconstituted into context as a synthetic
`branchSummary` message wrapped in `<summary>…</summary>`.

## `custom`

`{ type:"custom", customType, data? }` (`harness/types.ts:405-409`). An
arbitrary harness/app marker that is **not** replayed into model context.
`customType` namespaces it; `data` is an opaque payload.

## `custom_message`

`{ type:"custom_message", customType, content, details?, display }`
(`harness/types.ts:412-418`). Like `custom`, but its `content` (string or
`(text|image)[]`) **is** replayable into context. `display` controls UI
visibility.

## `label`

`{ type:"label", targetId, label }` (`harness/types.ts:421-425`). A display
label for the entry `targetId`. **Last write wins** per target; an
`undefined`/empty `label` clears it.

## `session_info`

`{ type:"session_info", name? }` (`harness/types.ts:428-432`). The session
name/title. **Last write wins** (`session.ts getSessionName`). The
persisted discriminant `session_info` predates the "session name" wording,
so don't expect the field to be called `title`.

## `leaf`

`{ type:"leaf", targetId, appendParentId? }` (`harness/types.ts:435-440`).
Moves the visible-head pointer. Carries no message. Covered in detail in
[jsonl-envelope.md](jsonl-envelope.md#the-tree-and-the-visible-leaf) because
you cannot determine the live conversation without it.
