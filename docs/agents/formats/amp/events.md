# The two wire shapes

Amp exposes a session in two forms. Neither is a file Amp maintains on disk.

| | `amp threads export <id>` | `amp -x … --stream-json` |
| --- | --- | --- |
| Shape | one pretty-printed JSON **document** | JSON **Lines**, one object per line |
| When | any time, any thread (server fetch) | only while the turn is running |
| Casing | camelCase | snake_case |
| Canonical for `toolpath-amp`? | **yes** | sidecar |

Everything below is `[observed, 0.0.1785170481-ga5b614]` unless tagged
otherwise. Fixtures: [`test-fixtures/amp/`](../../../../test-fixtures/amp/README.md).

---

## The export document

### Top-level envelope

```jsonc
{
  "v": 73,                                   // revision counter, NOT a schema version
  "id": "T-019fa4db-29cf-70c9-8d9b-81524df70e52",
  "title": "Filesystem tool exercise",       // server-generated
  "created": 1785177254351,                  // epoch ms
  "updatedAt": "2026-07-27T18:34:14.351Z",   // ISO 8601 — note: different type from `created`
  "agentMode": "medium",
  "creatorUserID": "user_01KYJ…",
  "pinned": false,
  "openExpiresAt": null,
  "env":   { /* see below */ },
  "meta":  { /* see below */ },
  "activatedSkills": [{ "name": "using-superpowers" }],
  "messages": [ /* the conversation */ ]
}
```

> **`v` is a revision counter, not a format version.** It was `20` on a
> 4-message thread, `41` on a 6-message thread, and `73` on a 24-message
> thread — it counts mutations applied to the thread. Do not branch parsing on
> it, and do not report it as a schema version.

### `env.initial` — the environment stamp

```jsonc
"env": { "initial": {
  "trees": [ { "uri": "file:///tmp/amp-elicit", "displayName": "amp-elicit" } ],
  "platform": {
    "os": "darwin", "osVersion": "25.5.0", "cpuArchitecture": "arm64",
    "client": "VS Code CLI Execute Mode",   // "VS Code CLI" for interactive TUI
    "clientType": "cli",
    "clientVersion": "0.0.1785170481-ga5b614",
    "webBrowser": false,
    "installationID": "…",                  // from ~/.local/share/amp/device-id.json
    "deviceFingerprint": "v1:fp_…"
  }
}}
```

- **`trees[0].uri`** is the session's working directory — the source for
  `ConversationView.base.working_dir`. It is a `file://` URI, not a bare path.
- **`clientVersion` pins the thread to the Amp build that created it.** The
  older thread on the capture machine carries `0.0.1785164324-gd1fcef` while
  the two new ones carry `ga5b614`. This is the per-thread version anchor —
  use it, not `amp --version`, when interpreting an old export.
- `client` distinguishes execute mode from the TUI.
- **No git information anywhere.** No branch, no commit, no remote. `[observed]`
  `SessionBase.vcs_*` therefore stays `None` for Amp.

### `meta`

```jsonc
"meta": {
  "executorType": "local-client",
  "lastKnownAgentState": { "state": "idle", "messageID": "M-…", "updatedAt": "…" },
  "deleted": false, "createdOnServer": false,
  "lastUserMessageAt": "2026-07-27T18:34:15.762Z",
  "workspaceID": null, "projectID": null,
  "visibility": "private", "sharedGroupIDs": [],
  "agentMode": "medium", "usesDtw": true, "usesThreadActors": true
}
```

`visibility` ∈ `private` | `unlisted` | `workspace` | `group` `[official]`
(from `amp --help`'s `--visibility`).

### Messages

Two roles only: `user` and `assistant`. **A `user` message is not necessarily
a human turn** — tool results come back as `user` messages carrying
`tool_result` blocks. In the feature-elicit capture, 12 `user` messages exist
but only **one** is a human prompt.

| Field | On | Notes |
| --- | --- | --- |
| `messageId` | both | **An integer index** (1, 2, 3 …), 1-based. |
| `protocolMessageID` | both | The `M-<base62>` string id. |
| `protocolMessageVersion` | both | Small int (0 or 1 observed). |
| `role` | both | `"user"` \| `"assistant"`. |
| `content` | both | Array of blocks (below). |
| `readAt` | both | `null` in all captures. |
| `meta` | some | user: `{sentAt: <epoch ms>}`; assistant: `{openAIResponsePhase: "final_answer"}`. |
| `userState` | user | `{timeZone, currentlyVisibleFiles: []}` — IDE context. |
| `state` | assistant | `{type: "complete", stopReason: "tool_use" \| "end_turn"}`. |
| `usage` | assistant | Per-message token usage — see below. |

> **Naming trap:** `messageId` in the *export* is an integer, while
> `messageId` in the *local thread log* is the `M-…` string. They are
> different fields with the same name. The export's string id is
> `protocolMessageID`.

### Content blocks

Four types observed. Counts are from the feature-elicit capture.

#### `text` (13)

```jsonc
{ "type": "text", "text": "I'll first load the required tool-use guidance…",
  "startTime": 1785177259924, "finalTime": 1785177259970, "blockState": "complete" }
```

`startTime`/`finalTime` are epoch ms and appear on assistant blocks only; a
user text block is just `{type, text}`.

#### `thinking` (12)

```jsonc
{ "type": "thinking", "provider": "openai",
  "thinking": "…natural-language reasoning summary…",   // often ""
  "signature": "gAAAAAB…",                              // opaque, ~1 kB
  "startTime": …, "finalTime": …, "blockState": "complete",
  "openAIReasoning": { "id": "rs_00f2…", "encryptedContent": "gAAAAAB…" } }
```

- **`thinking` is frequently empty.** In the feature-elicit capture only
  **5 of 12** carried text (355–451 chars); the other 7 were `""`. The real
  chain of thought is sealed in `openAIReasoning.encryptedContent`. Any
  fidelity claim about Amp thinking must be hedged accordingly.
- **`signature` and `openAIReasoning.encryptedContent` were byte-identical in
  all 12 blocks.** Treat them as one opaque value stored twice; preserve both
  verbatim for round-tripping and do not try to interpret either.
- `provider` was `"openai"` throughout, matching the `gpt-5.6-sol` model.
  Amp routes agent modes to different providers, so an Anthropic-backed mode
  would plausibly differ — `[unverified]`.

#### `tool_use` (11)

```jsonc
{ "type": "tool_use", "id": "TU-033wt8676K5StumvhRV8kd",
  "name": "shell_command", "input": { "command": "ls -la", "workdir": "/tmp/amp-elicit" },
  "complete": true, "blockState": "complete",
  "providerToolUseId": "call_kMXdmho5JPlQ3QL3GWSIkeRD" }
```

`id` is Amp's own `TU-<base62>`; `providerToolUseId` is the upstream model
provider's id (`call_…` for OpenAI). **Pair results by `id`.**

#### `tool_result` (11)

```jsonc
{ "type": "tool_result", "toolUseID": "TU-033wt8676K5StumvhRV8kd",
  "run": { "result": <polymorphic>, "status": "done", "progress": {} } }
```

- Always in the **following** `user` message.
- `status` was `"done"` for all 11 — **including the deliberately failing
  one**. It is a lifecycle state, not a success flag. See
  [file-fidelity.md](file-fidelity.md#how-errors-are-actually-signalled).
- `progress` appears on 3 of 11 and was `{}` every time.
- **`run.result` is polymorphic, keyed by which tool ran** — see next section.

### `run.result` shapes by tool

| Tool | `run.result` shape | n |
| --- | --- | --- |
| `shell_command` | `{ "output": "<stdout+stderr>", "exitCode": 0 }` | 6 |
| `apply_patch` | `{ "files": [{uri, diff, type, additions, deletions}], "summary": "…" }` | 3 |
| `skill` | `{ "content": [{ "type": "text", "text": "…" }] }` | 1 |
| `Task` | a **plain string** — the sub-agent's answer | 1 |

A reader must therefore branch on shape (or on the originating tool name), not
assume an object.

### Native tool vocabulary

`system/init` advertises **29 tools** at `agentMode: medium`:

```
apply_patch, clear_schedule, create_thread, download_thread_file, find_thread,
finder, get_current_user_identity, get_schedule, librarian, list_agent_modes,
list_runners, load_plugin, oracle, painter, public_artifact_url, read_thread,
read_web_page, set_schedule, shell_command, shell_command_status, skill, Task,
thread_file_url, thread_interact, update_schedule, upload_thread_file,
view_media, wait_for_threads, web_search
```

**There is no `read_file`, `edit_file`, `glob`, or `grep`.** File work goes
through `apply_patch` (writes) and `shell_command` (everything else) — a
Codex-shaped tool surface. Observed input shapes:

| Tool | `input` | `ToolCategory` |
| --- | --- | --- |
| `shell_command` | `{command, workdir}` | `Shell` |
| `apply_patch` | `{patchText}` — Codex-style `*** Begin Patch` envelope | `FileWrite` |
| `Task` | `{prompt, description}` | `Delegation` |
| `skill` | `{name}` | — (no category; a guidance loader) |
| `read_web_page` | `{url}` — the UI renderer reads exactly `input.url` `[reverse-eng, 0.0.1785228716-gedda19]` | `Network` |
| `web_search` | `{query}` (alt key `objective`) — the renderer titles "Web Search ⟨query ?? objective⟩" `[reverse-eng, 0.0.1785228716-gedda19]` | `Network` |
| `finder` | `{query}` — the renderer labels "Searching codebase" with `input.query` as detail `[reverse-eng, 0.0.1785228716-gedda19]` | `FileSearch` |
| `librarian` | `[unverified]` (search sub-agent, like `finder`) | `FileSearch` |
| `oracle` | `[unverified]` | `Delegation` |

Generic fallback: for tools without a dedicated renderer, the UI scans the
input for the first non-empty string among `path`, `filePattern`, `pattern`,
`query`, `url`, `objective`, `question`, `description`, `prompt` and shows it
as the row detail `[reverse-eng, 0.0.1785228716-gedda19]` — so even an
unmapped foreign tool renders usefully if it carries one of those keys.

The executor trace in the local log names the shell tool
**`async_shell_command`** while the wire name is `shell_command`
`[observed]` — don't be surprised by the mismatch.

---

## The `--stream-json` envelope

`amp --help` `[official]`: *"output in Claude Code-compatible stream JSON
format"*. Four line types; every line carries `session_id` = the `T-…` thread
id.

### `system` / `init` — one, first

```jsonc
{ "type": "system", "subtype": "init",
  "cwd": "/tmp/amp-elicit",
  "session_id": "T-019fa4db-…",
  "tools": [ /* 29 names */ ],
  "mcp_servers": [],
  "agent_mode": "medium" }
```

`agent_mode` is Amp's addition to the Claude Code shape.

### `user` / `assistant` — the conversation

```jsonc
{ "type": "user"|"assistant",
  "message": { "type": "message", "role": …, "content": [ … ],
               "stop_reason": "end_turn", "usage": { … } },
  "parent_tool_use_id": null,
  "session_id": "T-…" }
```

- `message.type`/`stop_reason`/`usage` are present on assistant lines only.
- Content blocks are **Anthropic-shaped**: `text`, `tool_use`
  (`{id, name, input}`), `tool_result` (`{tool_use_id, content, is_error}`).
- **`tool_result.content` is a string containing serialized JSON**, not an
  object — e.g. `"{\"output\":\"total 0\\n…\",\"exitCode\":0}"`. The export
  gives you the same data already parsed.
- **`is_error` exists here and nowhere else.** It was `false` throughout the
  capture, including for the failing `cat` (whose failure shows only as
  `exitCode: 1` inside the stringified payload).
- **`parent_tool_use_id` was `null` on all 26 lines**, including across the
  `Task` sub-agent dispatch. The Claude-Code nesting hook exists but Amp does
  not populate it — sub-agent turns are not streamed.
- Plain `--stream-json` emits **no `thinking` blocks**; `--stream-json-thinking`
  is required for those `[official]`.

### `result` / `success` — one, last

```jsonc
{ "type": "result", "subtype": "success",
  "duration_ms": 64413, "is_error": false, "num_turns": 12,
  "result": "<final assistant text>",
  "session_id": "T-…" }
```

`num_turns` counts assistant messages (12), not total messages (24).
Non-success `subtype` values are `[unverified]`.

### The two usage encodings

Same numbers, different names. Verified equal on **all 12** assistant messages
of the feature-elicit capture.

| Stream (snake) | Export (camel) |
| --- | --- |
| `input_tokens` | `inputTokens` |
| `output_tokens` | `outputTokens` |
| `cache_read_input_tokens` | `cacheReadInputTokens` |
| `cache_creation_input_tokens` | `cacheCreationInputTokens` |
| `max_tokens` | `maxInputTokens` |
| `service_tier` (`"standard"`) | — |
| — | `model`, `timestamp`, `totalInputTokens` |

### The full `usage` schema `[reverse-eng, ga5b614]`

The bundle's zod schema for `usage` has **nine** keys — one more than any
capture shows: `{model?, maxInputTokens, inputTokens, outputTokens,
cacheCreationInputTokens, cacheReadInputTokens, totalInputTokens,
thinkingBudget?, timestamp?}`. Three facts a capture alone cannot reveal:

- **`thinkingBudget` exists but appeared in none of the captured threads.**
  It is a *request budget*, not a consumption counter — never sum it, same
  rule as `maxInputTokens`. Any wire struct must tolerate it (an `Option`,
  or flattened extras) or the first thread that carries one breaks the
  value-identity round trip.
- **Both cache counters are `.nullable()`, not merely optional.** No null
  was observed, so fixtures can't catch this: decode null *and* absent to
  `None`, both distinct from a real `0`.
- **Amp's own UI session total is `cumulativeBilledTokens =
  Σ(totalInputTokens + outputTokens)` over assistant messages** — a
  client-side sum that counts cache read + creation as billed. Keep it
  *computable* from the stored counters (it reconciles field-for-field with
  the derived documents); never stamp it onto a turn.

Independently confirmed by the same sweep: **no reasoning/thinking token
counter exists anywhere in the bundle** (zero hits for
`reasoningTokens`/`thinkingTokens`), which is why the toolpath mapping emits
no `breakdowns`. The `totalInputTokens = inputTokens + cacheReadInputTokens
+ cacheCreationInputTokens` sum relation is `[observed]` — verified on all
17 usage objects across the three captured threads (the server computes it;
no client-side arithmetic exists for it).

---

## Mapping sketch to the toolpath IR

Target types are `toolpath_convo::{ConversationView, Turn, ToolInvocation,
DelegatedWork, TokenUsage}`. Source paths are into the **export** document.

| toolpath | Amp source | Notes |
| --- | --- | --- |
| `ConversationView.id` | `.id` | The `T-…` thread id. |
| `.started_at` | `.created` | Epoch **ms** → `DateTime<Utc>`. |
| `.last_activity` | `.updatedAt` | Already ISO 8601. |
| `.provider_id` | — | Literal `"amp"`. |
| `.producer` | `.env.initial.platform` | `{name: "amp", version: clientVersion}` — the **thread's** version, not the running binary's. |
| `.base.working_dir` | `.env.initial.trees[0].uri` | Strip the `file://` scheme. |
| `.base.vcs_*` | — | Always `None`; Amp records no git state. |
| `.total_usage` | Σ over messages | Sum of the four counters; safe because usage is per-message (see [RECON.md Q2](RECON.md#q2--tokens)). |
| `.files_changed` | `apply_patch` results | `run.result.files[].uri`, first-touch dedup. |
| `.events` | `.activatedSkills`, `.meta` | Preserve for round-tripping; not turns. |
| **`Turn`** | one per element of `.messages`, EXCEPT tool-result-only `user` messages | Those are transport plumbing: their results merge onto the originating invocation and no turn is emitted (the 24-message capture yields 13 turns). |
| `Turn.id` | `.protocolMessageID` | The `M-…` string — stable. **Not** the integer `messageId`. |
| `.parent_id` | previous message's id | Amp is linear; chain sequentially. |
| `.group_id` | — | **`None`.** Usage is already per-message; no grouping needed. |
| `.role` | `.role` | But a `user` message holding `tool_result` is plumbing, not a human turn. |
| `.timestamp` | `.meta.sentAt` / `.usage.timestamp` / block `startTime` | No single canonical field; prefer `usage.timestamp` on assistant turns. |
| `.text` | `content[].type == "text"` | Concatenate in order. |
| `.thinking` | `content[].type == "thinking"` → `.thinking` | Often `""` → map to `None`, not `Some("")`. |
| `.model` | `.usage.model` | |
| `.stop_reason` | `.state.stopReason` | |
| **`.token_usage`** | `.usage` | Four fields; drop `totalInputTokens` and `maxInputTokens`. |
| `.attributed_token_usage` | — | **`None`.** Amp reports per message, not per block. |
| `.token_usage.breakdowns` | — | **Omit.** Reasoning tokens are not itemized. |
| **`ToolInvocation`** | `content[].type == "tool_use"` | |
| `.id` | `.id` (`TU-…`) | |
| `.name` | `.name` | |
| `.input` | `.input` | |
| `.result` | the `tool_result` block with matching `toolUseID` | Lives in the **next** message; merge it onto the originating turn. |
| `.result.content` | `run.result` | Polymorphic — stringify non-string shapes. |
| `.result.is_error` | `run.result.exitCode != 0` | **Not** `run.status`. See [file-fidelity.md](file-fidelity.md#how-errors-are-actually-signalled). |
| `.category` | tool name | Table above. |
| **`DelegatedWork`** | `Task` tool calls | |
| `.agent_id` | the `TU-…` id | No separate sub-agent id exists. |
| `.prompt` | `input.prompt` | |
| `.result` | `run.result` (a string) | |
| `.turns` | — | **Always empty.** The sub-agent's turns are not in the parent thread and no sibling thread is created. |
| **`FileMutation`** | `apply_patch` result `files[]` | See [file-fidelity.md](file-fidelity.md). |

`Path.meta.kind` = `PATH_KIND_AGENT_CODING_SESSION`, as for every other
conversation provider.
