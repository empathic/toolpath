# The `events.jsonl` event stream

`events.jsonl` is the per-session source of truth: an **append-only,
line-delimited JSON** log of everything that happened in the session. This is
the file a forward provider parses to reconstruct the conversation.

> **⚠️ Undocumented internal format.** GitHub has **not** published an
> `events.jsonl` schema — feature request
> [#3551](https://github.com/github/copilot-cli/issues/3551) asks them to
> "formalize `events.jsonl` as an official hook/integration API," so it can
> change between releases. **The envelope and almost all event types below are
> now `[observed]` against first-hand captures at `copilotVersion` 1.0.67–1.0.68**
> (incl. a feature-elicit run with a real sub-agent and `session.shutdown`);
> only `skill.*` / `hook.*` / `abort` / mode-plan-compaction remain
> `[reverse-eng]`.

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
| `session.shutdown` | `shutdownType`, **`tokenDetails`** `{input,cache_read,cache_write,output: {tokenCount}}`, `modelMetrics` (**map keyed by model name** → `{requests: {count, cost}, usage}`), `totalPremiumRequests`, `totalApiDurationMs`, `sessionStartTime` (epoch ms), `eventsFileSizeBytes`, `codeChanges {linesAdded, linesRemoved, filesModified[]}` | `[observed, 1.0.68]` Session close. `tokenDetails.output.tokenCount` equals Σ per-message `outputTokens` (verified) — totals, not additive. The old reverse-eng shape (`usage.inputTokens`, `modelMetrics.model`) was wrong. |
| `session.mode_changed` / `session.plan_changed` / `session.compaction_start` / `session.compaction_complete` | (mode / plan / token counts) | `[reverse-eng]` Not seen in the sample. |

### `system.*`, `user.*`, `assistant.*` — the conversation

| Type | `data` fields | Notes |
|---|---|---|
| `system.message` | `role: "system"`, `content` | `[observed]` The system prompt (large — ~56 KB). Recorded as a `ConversationEvent`, not a turn. |
| `user.message` | `content`, `transformedContent`, `interactionId`, `attachments`, `parentAgentTaskId` | `[observed]` `content` is the raw prompt; `transformedContent` adds datetime/system-reminder wrapping. |
| `assistant.turn_start` / `assistant.turn_end` | — | `[observed]` Turn boundary. |
| `assistant.message` | `content`, `model`, **`reasoningText`** (thinking), `reasoningOpaque`, **`toolRequests`** `[{toolCallId, name, arguments, intentionSummary}]`, **`outputTokens`**, `messageId`, `turnId`, `requestId` | `[observed]` One turn can have several. `reasoningText` → `Turn.thinking`; `outputTokens` summed for the session total. `toolRequests` mirror the following `tool.execution_start` (we take the tool from the execution events to avoid double-counting) — but note: **the resumed-timeline UI builds its tool rows from this mirror, not from the execution events**, so a writer must keep the mirror's `name`/`arguments` in Copilot's native vocabulary (see [file-fidelity.md](file-fidelity.md)). |

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

### Native tool vocabulary `[observed, 1.0.67–1.0.68]`

The built-in tool names and argument shapes seen in real sessions (what a
writer must remap foreign tool calls into — the timeline UI keys row rendering
off these):

| Tool | `arguments` | Result notes |
|---|---|---|
| `bash` | `{command, description?}` | stdout in `result.content`/`detailedContent`. |
| `view` | `{path, view_range?: [start, end]}` | file/dir listing text. Row title: `<path> (lines a-b)`. |
| `edit` | `{path, old_str, new_str}` | `result.content` = `File <path> updated with changes.`; `result.detailedContent` = git-style unified diff (see [file-fidelity.md](file-fidelity.md)). |
| `create` | `{path, file_text}` | `result.content` = `Created file <path> with N characters`; `detailedContent` = create diff. |
| `glob` / `grep` | `{pattern, path?}` | (grep shape `[reverse-eng]` — matches the row-title renderer). |

The str_replace_editor family also accepts `str_replace`/`insert` command
variants `[bundle]`, but only `edit`/`create`/`view` were observed in sessions.
Sessions also carry pass-through names from MCP/custom tools (`task`,
`ToolSearch`, `Skill`, …) which render generically.

### `subagent.*` `[observed, 1.0.68]`, `skill.*` / `hook.*` / `abort` `[reverse-eng]`

| Type | `data` fields | Notes |
|---|---|---|
| `subagent.started` | **`toolCallId`**, `agentName`, `agentDisplayName`, `agentDescription`, `model` | `[observed]` A **thin marker**: the sub-agent is dispatched via a **`task` tool call** (args `{name, agent_type, description, prompt, mode}`; result on its `tool.execution_complete`) sharing the same `toolCallId`. The marker carries the *agent-type* metadata only — no `id`/`prompt`/`result` of its own; the sub-agent's turns are **not** in the parent stream. Forward mapping: `Turn.delegations` with `agent_id = toolCallId` (pairs with the tool call). |
| `subagent.completed` | `toolCallId`, `agentName`, `agentDisplayName`, `model` | `[observed]` Closing marker (no result payload — see the `task` tool's complete). |
| `skill.invoked` | (skill name) | `[reverse-eng]` Not yet observed. |
| `hook.start` / `hook.end` | — | `[reverse-eng]` Not yet observed. → `ConversationView.events`. |
| `abort` | — | `[reverse-eng]` Not yet observed. |

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
