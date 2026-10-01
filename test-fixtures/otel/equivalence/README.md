# Re-encoded OpenRouter fixtures

Each file here is an OpenRouter Broadcast fixture from `../openrouter/`,
re-encoded as current GenAI semantic conventions. They are generated, never
edited by hand:

```bash
python3 scripts/otel-fixtures/reencode_openrouter.py          # write the files
python3 scripts/otel-fixtures/reencode_openrouter.py --check  # exit 1 when one is stale
```

The re-encoder uses only the standard library. The tests in
`crates/toolpath-otel/src/tests/equivalence_fixtures.rs` check these files
against the table below. `equivalence.rs` derives each file and its
OpenRouter source and requires Paths that agree on the cross-profile
comparison set (`src/tests/common/equivalence.rs`), and runs the retention
round trip over these files (see Retention in `docs/agents/formats/otel.md`).

| File | Source |
|---|---|
| `claude-code.ndjson` | `../openrouter/claude-code.ndjson` |
| `codex.ndjson` | `../openrouter/codex.ndjson` |
| `opencode.ndjson` | `../openrouter/opencode.ndjson` |
| `pi.ndjson` | `../openrouter/pi.ndjson` |
| `synthetic-fork.ndjson` | `../openrouter/synthetic-fork.ndjson` |
| `codex-error-span.ndjson` | `../openrouter/codex-error-span.json` (one delivery) |

`../openrouter/connection-test.json` is not re-encoded.

## Re-encoder table

Each output line is one source delivery. Only root spans (named
`LLM Generation`) are kept; the `generation` and `provider attempt N: …`
children are dropped, and a delivery with no root span writes no line. Each
kept span gets its own `resourceSpans` entry under the scope
`toolpath-reencode`.

Span fields:

| Source | Re-encoded |
|---|---|
| `traceId`, `spanId`, `parentSpanId`, `kind`, `startTimeUnixNano`, `endTimeUnixNano`, `status` | copied |
| `name` (`LLM Generation`) | `chat <gen_ai.request.model>` |
| attribute `trace.metadata.openrouter.api_key_name` | resource attribute `service.name` (no resource attributes when absent) |

Span attributes (every attribute not listed is dropped):

| Source | Re-encoded |
|---|---|
| (none) | `gen_ai.operation.name` = `"chat"` |
| `gen_ai.response.id`, `user.id`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.provider.name` | copied |
| `session.id` | `gen_ai.conversation.id` |
| `gen_ai.response.finish_reason` (string) | `gen_ai.response.finish_reasons` (array of that one string) |
| `gen_ai.usage.input_tokens` | `gen_ai.usage.input_tokens` |
| `gen_ai.usage.output_tokens` | `gen_ai.usage.output_tokens` |
| `gen_ai.usage.input_tokens.cached` | `gen_ai.usage.cache_read.input_tokens` |
| `gen_ai.usage.input_tokens.cache_write` | `gen_ai.usage.cache_write.input_tokens` |
| `gen_ai.usage.output_tokens.reasoning` | `gen_ai.usage.reasoning.output_tokens` |
| `gen_ai.prompt` (`{"messages": [...]}`) | `gen_ai.input.messages` (each message mapped as below) |
| `gen_ai.completion` | `gen_ai.output.messages` (one assistant message, mapped as below) |

Input messages (`gen_ai.prompt.messages[]` to `gen_ai.input.messages[]`):

| Source | Re-encoded |
|---|---|
| role `tool` | role `tool` with one `tool_call_response` part: `response` is the content's text (text parts joined by newlines, thinking parts skipped, other parts as `[<type>]`), `id` is `tool_call_id` when present |
| `reasoning_details[].text` | a `reasoning` part each (first) |
| string `content` | one `text` part |
| `content[]` text part | a `text` part |
| `content[]` part of any other type | the part verbatim |
| `tool_calls[]` | a `tool_call` part each: `name` and `arguments` from `function`, `id` when present (last) |

Output message (`gen_ai.completion` to `gen_ai.output.messages`):

| Source | Re-encoded |
|---|---|
| `reasoning` | a `reasoning` part (first) |
| `completion` | a `text` part |
| `toolCalls[]` | a `tool_call` part each, as for input messages (last) |
| `gen_ai.response.finish_reason` | the message's `finish_reason` |
