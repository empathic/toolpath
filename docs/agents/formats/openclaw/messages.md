# Messages and content blocks

A `message` entry wraps an `AgentMessage`
([entry-types.md §message](entry-types.md#message)). This doc covers the
role model, the content-block variants, and the two timestamp encodings
that bite naive readers. Tool-call/result linkage is in
[tools.md](tools.md); token usage on assistant messages is in
[usage.md](usage.md).

The message types live in `packages/llm-core/src/types.ts`; the harness
extends them with extra roles in `packages/agent-core/src/types.ts`.

## Roles

`AgentMessage` is the LLM `Message` union (`user | assistant | toolResult`)
plus harness-only roles (`bashExecution`, and the read-time-only `custom` /
`branchSummary` / `compactionSummary`). On disk inside a `message` entry,
the realistic set is:

| `role` | Meaning |
|---|---|
| `user` | Human / channel input. `content` is a bare string **or** an array of `text`/`image` blocks. |
| `assistant` | Model output. `content` is always a block array; carries `api`/`provider`/`model`/`usage`/`stopReason`. |
| `toolResult` | Result of a prior `toolCall`. Carries `toolCallId`, `toolName`, `content`, `isError`. A **separate** entry from the call. See [tools.md](tools.md). |
| `bashExecution` | Harness shell-execution message (distinct shape). |

The `custom` / `branchSummary` / `compactionSummary` roles are
**reconstructed at read time** from `custom_message` / `branch_summary` /
`compaction` entries; you will not find them serialized as a `message`
entry's `role` (`session.ts:46-99`).

## Content blocks

Block discriminants are `text`, `thinking`, `image`, `toolCall`. There is
**no `tool_result` content block** — tool results are whole messages
(see [tools.md](tools.md)).

### `text` (`llm-core/src/types.ts:226-230`)

```json
{ "type": "text", "text": "I'll read x.ts first.", "textSignature": "…optional…" }
```

`textSignature` is optional; it may be a legacy id string or a
`TextSignatureV1` JSON blob `{ v:1, id, phase?: "commentary"|"final_answer" }`.

### `thinking` (`llm-core/src/types.ts:233-241`)

```json
{ "type": "thinking", "thinking": "Let me read the file", "thinkingSignature": "…opaque…", "redacted": false }
```

| Field | Shape | Notes |
|---|---|---|
| `thinking` | string | The reasoning text. |
| `thinkingSignature` | string (optional) | Opaque replay signature / reasoning-item id. |
| `redacted` | bool (optional) | Safety-redacted; the payload is kept in `thinkingSignature`. |

Note: this is reasoning **content**. The reasoning **token count** is a
different thing and is *not* in the persisted usage — see
[usage.md](usage.md#reasoning-tokens-are-runtime-only).

### `image` (`llm-core/src/types.ts:244-248`)

```json
{ "type": "image", "data": "<base64>", "mimeType": "image/png" }
```

### `toolCall` (`llm-core/src/types.ts:251-258`)

```json
{ "type": "toolCall", "id": "call_1", "name": "read_file",
  "arguments": { "path": "src/x.ts" },
  "thoughtSignature": "…", "executionMode": "sequential" }
```

| Field | Shape | Notes |
|---|---|---|
| `id` | string | Correlates with the later `toolResult.toolCallId`. |
| `name` | string | Tool name. |
| `arguments` | object | Free-form JSON args (the only place file paths etc. appear; [tools.md](tools.md)). |
| `thoughtSignature` | string (optional) | Google-specific opaque thought-context signature. |
| `executionMode` | `"sequential"`\|`"parallel"` (optional) | Scheduling hint. |

## Assistant message metadata

`AssistantMessage` (`llm-core/src/types.ts:287-304`) carries, beyond
`content`:

| Field | Shape | Notes |
|---|---|---|
| `api` | string | e.g. `"anthropic-messages"`, `"openai-responses"`. |
| `provider` | string | e.g. `"anthropic"`, `"openai"`. |
| `model` | string | Requested model. |
| `responseModel` | string (optional) | Concrete served model when it differs from requested (e.g. OpenRouter `auto`). |
| `responseId` | string (optional) | Provider response id. |
| `diagnostics` | array (optional) | `AssistantMessageDiagnostic[]`; serialized fields not confirmed against a sample. |
| `usage` | `Usage` | See [usage.md](usage.md). |
| `stopReason` | string | See below. |
| `errorMessage` / `errorCode` / `errorType` / `errorBody` | string (optional) | Error detail when the turn failed. |
| `timestamp` | int (epoch ms) | **Number, not ISO** — see below. |

### `stopReason`

Observed values: `stop`, `length`, `toolUse`, `error`, `aborted`. Treat
unknown values as forward-compatible (round-trip rather than reject).

## The two timestamp encodings

This is the single easiest thing to get wrong:

- The **entry-level** `timestamp` (header and every entry) is an **ISO-8601
  string** (`new Date().toISOString()`).
- The **inner message** `timestamp` (`UserMessage` / `AssistantMessage` /
  `ToolResultMessage`) is **epoch milliseconds (a number)** —
  `timestamp: number; // Unix timestamp in milliseconds`.

So a single `message` line has *both* a string entry-timestamp and a numeric
message-timestamp. Keep them in sync on round-trip. (The one exception,
`CompactionSummaryMessage.timestamp`, is typed `number | string` for
backward-compat, but that role isn't normally persisted as a `message`
entry.) This mirrors Pi's format exactly; see
[known-issues.md](known-issues.md).
