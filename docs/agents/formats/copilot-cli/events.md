# The `events.jsonl` event stream

`events.jsonl` is the per-session source of truth: an **append-only,
line-delimited JSON** log of everything that happened in the session. This is
the file a forward provider parses to reconstruct the conversation.

> **⚠️ This is the least-documented and most load-bearing part of the format.**
> GitHub has **not** published an `events.jsonl` schema — feature request
> [#3551](https://github.com/github/copilot-cli/issues/3551) literally asks them
> to "formalize `events.jsonl` as an official hook/integration API," which
> confirms it is currently an *undocumented internal format that can change
> between releases.* Everything below the envelope section is `[reverse-eng]`,
> observed at **v1.0.54**, and should be treated as a hypothesis until checked
> against a captured session.

## Line envelope

- **One JSON object per line.** Parsed with `JSON.parse()` per line, not as one
  array — confirmed by issue [#2012](https://github.com/github/copilot-cli/issues/2012),
  a bug where literal `U+2028`/`U+2029` characters embedded in a line break
  `JSON.parse()` during `/resume`. `[reverse-eng, High]`
- **A `type` discriminant** in a dotted namespace (`tool.execution_complete`)
  selects the event variant. `[reverse-eng, Medium]`
- The rest of the envelope — whether the payload is inline or nested under a
  `data` key, whether there is a top-level `timestamp` — is **`[inferred]` /
  `[unverified]`.** No source quoted a verbatim full line. The working
  hypothesis (model after Codex's `{timestamp, type, payload}` envelope):

  ```jsonc
  // [INFERRED — do not trust the key layout until verified against a sample]
  {"type": "tool.execution_complete", "timestamp": "…", "data": { /* per-type */ }}
  ```

  A robust parser should therefore: discriminate on `type`; tolerate the payload
  being either inline (sibling keys) or nested under `data`/`payload`; and keep
  an `Unknown { type, raw }` fallback for unrecognized types (forward-compat,
  exactly as `toolpath-codex` does with `RolloutItem::Unknown`).

## Event-type catalogue (v1.0.54)

`[reverse-eng, Medium]` — ~20 types, grouped by namespace. Sources: issue #3551
(RockNoggin's enumeration) + the jonmagic write-up. Field lists are only as
complete as those sources; absence below means "not reported," not "not present."

### `session.*` — lifecycle and session-level state

| Type | Reported fields | Notes |
|---|---|---|
| `session.start` | `version`, `cwd`, `model` | Session opener. The closest thing to Codex's `session_meta` — cwd + model live here. |
| `session.task_complete` | summary text | A task/turn finished; carries a summary. |
| `session.shutdown` | `modelMetrics` (model id), `usage.inputTokens` | Session close; carries per-session token/model metrics. **Note the camelCase payload keys** (`inputTokens`) — the one concrete hint at payload casing. |
| `session.model_change` | (model id) | User/agent switched models mid-session. |
| `session.mode_changed` | (mode) | Mode switch (e.g. plan vs. execute). |
| `session.plan_changed` | (plan) | The working plan was updated. |
| `session.compaction_start` | — | Context compaction began. |
| `session.compaction_complete` | token counts | Context was compacted; carries before/after token counts. |

### `user.*` and `assistant.*` — the conversation

| Type | Reported fields | Notes |
|---|---|---|
| `user.message` | (message text) | A user prompt. |
| `assistant.turn_start` | — | Opens an assistant turn. |
| `assistant.message` | (message text) | Assistant output. Multiple `assistant.message` events may fall between one `turn_start`/`turn_end` pair `[inferred]`. |
| `assistant.turn_end` | — | Closes an assistant turn — the natural turn boundary and token-accounting group. |

### `tool.*` — tool / command invocations

| Type | Reported fields | Notes |
|---|---|---|
| `tool.execution_start` | tool **name**, **args** | A tool/command call begins. `name` + `args` is what a provider maps to `ToolInvocation`. |
| `tool.execution_complete` | tool **name**, **args**, **success** | The call finished; `success` is the error flag. The *result content* (stdout, file content) is **not confirmed** to be inline — see [file-fidelity.md](file-fidelity.md). |

> **Correlation id is unverified.** The sources report only `name`/`args`/
> `success` on `tool.execution_*` — **no correlation id linking a `complete`
> back to its `start` was confirmed.** `toolpath-copilot` therefore uses an id
> to pair when one is present (any of `id`/`callId`/`call_id`/`toolCallId`), but
> falls back to positional pairing (the most-recent result-less invocation in
> the open turn, preferring the same tool name) when it's absent — so an
> id-less stream still collapses each `start`/`complete` to one invocation
> rather than double-counting. Confirm the real correlation mechanism against a
> sample (see [known-gaps](known-gaps-and-sourcing.md#verify-once-we-have-samples)).

### `subagent.*`, `skill.*`, `hook.*`, `abort`

| Type | Reported fields | Notes |
|---|---|---|
| `subagent.started` | — | A sub-agent was dispatched → maps to `Turn.delegations` / `DelegatedWork`. |
| `subagent.completed` | — | Sub-agent finished. Whether sub-agent turns are inline or in a separate file is **unverified**. |
| `skill.invoked` | (skill name) | A skill was activated. |
| `hook.start` / `hook.end` | — | A user hook ran → maps to `ConversationView.events`, not a turn. |
| `abort` | — | The session/turn was aborted. |

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
