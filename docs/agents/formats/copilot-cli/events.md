# The `events.jsonl` event stream

`events.jsonl` is the per-session source of truth: an **append-only,
line-delimited JSON** log of everything that happened in the session. This is
the file a forward provider parses to reconstruct the conversation.

> **⚠️ Undocumented internal format.** GitHub has **not** published an
> `events.jsonl` schema — feature request
> [#3551](https://github.com/github/copilot-cli/issues/3551) asks them to
> "formalize `events.jsonl` as an official hook/integration API," so it can
> change between releases. **The envelope and the core event types below are
> now `[observed]` against a first-hand capture at `copilotVersion` 1.0.67**;
> items still flagged `[reverse-eng]`/`[unverified]` were not exercised by that
> session (no sub-agent / skill / hook / abort / shutdown occurred in it).

## Line envelope `[observed, 1.0.67]`

Every line is one JSON object of the form:

```jsonc
{"type": "tool.execution_complete",
 "id": "d4f4054e-…",           // per-event UUID
 "parentId": "e34fb2e9-…",     // parent event UUID — events form a tree
 "timestamp": "2026-07-01T14:…Z",
 "data": { /* per-type payload */ }}
```

- **One JSON object per line**, parsed with `JSON.parse()` per line (not one
  array) — corroborated by issue [#2012](https://github.com/github/copilot-cli/issues/2012)
  (raw `U+2028`/`U+2029` breaking `/resume`).
- **`type`** is a dotted-namespace discriminant; **`data`** holds the payload
  (confirmed — payload is *not* inline). **`id`**/**`parentId`** form an event
  tree (a `parentId` chain, not just sequential). `toolpath-copilot` preserves
  `id`/`parentId` but derives turns sequentially (the tree is not yet used).
- The reader still tolerates payload-inline / `payload`-keyed shapes and keeps
  an `Unknown { type, raw }` fallback for unrecognized types — belt-and-suspenders
  in case the envelope shifts in another version.

## Event-type catalogue

Grouped by namespace. Rows tagged `[observed, 1.0.67]` were seen in a first-hand
capture; `[reverse-eng]` rows come from issue #3551 + the jonmagic write-up and
did not occur in that session. Field paths are relative to `data`.

### `session.*` — lifecycle and session-level state

| Type | `data` fields | Notes |
|---|---|---|
| `session.start` | `sessionId`, `version` (int schema ver), `producer` (`"copilot-agent"`), `copilotVersion`, `startTime`, **`context`** `{cwd, gitRoot, repository, hostType, repositoryHost, branch, headCommit, baseCommit}` | `[observed]` Session opener. cwd + git live under `context`, **not** top-level; the CLI version is `copilotVersion` (top-level `version` is an int). No `model` here. |
| `session.model_change` | `newModel` (e.g. `"auto"`) | `[observed]` Model switched (also emitted once right after start). |
| `session.task_complete` | `summary` | `[observed]` A task finished. |
| `session.shutdown` | `modelMetrics` (model id), `usage.inputTokens` | `[reverse-eng]` Session close. **Did not occur** in the captured session (it had an `inuse.<pid>.lock`); token totals there came per-message instead — see below. |
| `session.mode_changed` / `session.plan_changed` / `session.compaction_start` / `session.compaction_complete` | (mode / plan / token counts) | `[reverse-eng]` Not seen in the sample. |

### `system.*`, `user.*`, `assistant.*` — the conversation

| Type | `data` fields | Notes |
|---|---|---|
| `system.message` | `role: "system"`, `content` | `[observed]` The system prompt (large — ~56 KB). Recorded as a `ConversationEvent`, not a turn. |
| `user.message` | `content`, `transformedContent`, `interactionId`, `attachments`, `parentAgentTaskId` | `[observed]` `content` is the raw prompt; `transformedContent` adds datetime/system-reminder wrapping. |
| `assistant.turn_start` / `assistant.turn_end` | — | `[observed]` Turn boundary. |
| `assistant.message` | `content`, `model`, **`reasoningText`** (thinking), `reasoningOpaque`, **`toolRequests`** `[{toolCallId, name, arguments, intentionSummary}]`, **`outputTokens`**, `messageId`, `turnId`, `requestId` | `[observed]` One turn can have several. `reasoningText` → `Turn.thinking`; `outputTokens` summed for the session total. `toolRequests` mirror the following `tool.execution_start` (we take the tool from the execution events to avoid double-counting). |

### `tool.*` — tool / command invocations `[observed]`

| Type | `data` fields | Notes |
|---|---|---|
| `tool.execution_start` | **`toolCallId`**, **`toolName`**, **`arguments`**, `model`, `turnId`, `shellToolInfo` | Opens a call. `toolName`/`arguments` → `ToolInvocation`. |
| `tool.execution_complete` | **`toolCallId`**, **`success`**, **`result`** `{content, detailedContent}`, `model`, `turnId`, `toolTelemetry` | The result text is under **`result.content`** (an object — earlier versions of this doc wrongly guessed a top-level string). `success` is the error flag. |

**Correlation** `[observed]`: `tool.execution_complete` links to its start via
**`toolCallId`** (same value on both, and on the `assistant.message`'s
`toolRequests`). `toolpath-copilot` pairs on it, and additionally falls back to
positional pairing (most-recent result-less invocation in the open turn) if a
future version ever omits the id — so it never double-counts.

### `subagent.*`, `skill.*`, `hook.*`, `abort` `[reverse-eng]`

None of these occurred in the captured session, so their `data` shapes remain
unverified. Handling: `subagent.started`/`completed` → `Turn.delegations`
(`DelegatedWork`, paired by `id`); `skill.invoked` / `hook.*` / `abort` →
`ConversationView.events`.

| Type | Notes |
|---|---|
| `subagent.started` / `subagent.completed` | Sub-agent dispatch/finish. Whether sub-agent turns are inline or in a separate stream is **unverified**. |
| `skill.invoked` | A skill was activated. |
| `hook.start` / `hook.end` | A user hook ran. |
| `abort` | The session/turn was aborted. |

### A conflicting source

A DeepWiki page rendered some names differently (`message`, `call_tool`,
`subagentStart`, "Rewind"). These look like DeepWiki's own paraphrase rather
than literal `type` strings; the dotted-namespace names above are corroborated
by two independent sources, so **prefer them.** `[reverse-eng, Low on DeepWiki]`

## Mapping sketch to the toolpath IR

How these events would build a `ConversationView` (a forward-provider design
note, all `[inferred]` pending a sample):

| Copilot event | IR target |
|---|---|
| `session.start` | `ConversationView.base` (cwd), `producer`, first turn `model` |
| `user.message` | `Turn { role: User }` |
| `assistant.turn_start`/`message`/`turn_end` | `Turn { role: Assistant }`, `group_id` = the turn span |
| `tool.execution_start` | open a `ToolInvocation { name, input: args }` |
| `tool.execution_complete` | back-fill `ToolInvocation.result { is_error: !success }` |
| `subagent.started`/`completed` | `Turn.delegations` (`DelegatedWork`) |
| `skill.invoked` | a `Delegation`-category `ToolInvocation`, or a `ConversationEvent` |
| `hook.*`, `abort` | `ConversationView.events` |
| `session.shutdown`, `session.compaction_complete` | `TokenUsage` (see token-accounting caveats in [known-gaps](known-gaps-and-sourcing.md)) |

The single biggest unknown for this mapping is **where tool result content and
file edits live** — covered next in [file-fidelity.md](file-fidelity.md).
