# The `message` object

`user` and `assistant` entries carry a `message` object containing the
actual conversation payload. Metadata entries (`system`, `attachment`,
`permission-mode`, etc.) do not.

## Shape

```jsonc
{
  "role":          "user" | "assistant" | "system",
  "content":       string | ContentPart[],
  "model":         "claude-opus-4-6",   // assistant only
  "id":            "msg_01P33i…",       // assistant only — Anthropic API message ID
  "type":          "message",           // assistant only — always the literal "message"
  "stop_reason":   "end_turn" | "tool_use" | "stop_sequence" | null,
  "stop_sequence": string | null,
  "usage":         Usage                // assistant only — see usage.md
}
```

Both `stop_reason` and `stop_sequence` can be `null`. `stop_reason` is
frequently `null` on disk even for turns that completed normally,
because the entry is persisted before the streaming API response
finalizes — see [known-issues.md](known-issues.md).

## The string-or-array `content` trap

`message.content` is **either a bare string or an array of content
parts**, depending on what the entry carries. A parser must handle both
shapes.

```jsonc
// Bare string — typical for simple user prompts
{"role": "user", "content": "what does this file do?"}

// Array of parts — assistant responses, tool results, slash commands, paste
{"role": "assistant", "content": [{"type": "text", "text": "Done."}]}
```

Empirically:
- **User entries** use both shapes. Direct prompts are strings; tool
  result carriers are arrays.
- **Assistant entries** use arrays virtually always. Even a
  plain-text response is wrapped as `[{"type": "text", "text": "…"}]`.
  The Claude Code *loader* relies on this — see
  [writing-compatible-jsonl.md](writing-compatible-jsonl.md).

## Content part types

Each part has a `type` discriminant.

### `text`

Plain text, the common case.

```json
{"type": "text", "text": "I'll help with that."}
```

### `thinking`

Extended-thinking output. Only emitted by models that support it
(`claude-opus-4-6` and later `-4-7`). Two fields:

```jsonc
{
  "type": "thinking",
  "thinking": "…reasoning text…",
  "signature": "EoYDClkIDBgCKkDVU…"   // base64; ~450 chars; cryptographic proof
}
```

The `signature` is an Anthropic-issued cryptographic proof of the
thinking content. Thinking blocks without a valid signature are
**rejected** as prior-turn context when the session is resumed — the
API won't replay them back to the model. A tool that rewrites or
truncates thinking content will break resume.

Empty-string `thinking` values with a valid `signature` are observed
and legal (they represent thinking content that was later redacted).

### `tool_use`

A tool call issued by the assistant.

```jsonc
{
  "type": "tool_use",
  "id": "toolu_01LJtcvehSnXfMztfk8b8ZLC",
  "name": "Grep",
  "input": {
    "pattern": "crab.?city",
    "-i": true,
    "output_mode": "files_with_matches"
  },
  "caller": {"type": "direct"}       // optional; origin of the call
}
```

- **`id`** — Anthropic tool-use ID with the `toolu_` prefix. Primary
  key linking this `tool_use` to its eventual `tool_result`.
- **`name`** — tool name. Built-in tools use PascalCase (`Read`,
  `Bash`, `Grep`). MCP-provided tools are namespaced:
  `mcp__<server>__<tool>`.
- **`input`** — tool-specific parameter object. See [tools.md](tools.md).
- **`caller`** — optional. `{type: "direct"}` for directly-issued
  calls. The full enumeration of `caller.type` values isn't well-known.

### `tool_result`

The result of a prior tool call, carried by the following **user**
entry.

```jsonc
{
  "type": "tool_result",
  "tool_use_id": "toolu_01LJtcvehSnXfMztfk8b8ZLC",
  "content": "Found 79 files\npackages/…",  // string OR array of text parts
  "is_error": false                          // default false
}
```

- **`tool_use_id`** — matches the `id` from the issuing `tool_use`.
- **`content`** — string, or an array of objects shaped
  `{text: "..."}`. An array with multiple parts should be joined with
  `\n` to recover the intended output.
- **`is_error`** — defaults to `false`. `true` indicates the tool
  raised or returned an error.

See [tools.md](tools.md) for the full tool-invocation lifecycle.

### Other content types (API-level, rarely seen on disk)

The Anthropic API defines several content-block types that Claude Code
can in principle persist but that don't commonly show up in typical
coding sessions:

- `image` — image blocks in messages
- `document` — attached documents
- `redacted_thinking` — thinking content redacted by Anthropic
- `server_tool_use` — server-executed tool calls (e.g. built-in web search)
- `web_search_tool_result` — results from server-side web search

A tolerant parser preserves unknown `type` values rather than failing.

## Role values

Lowercase: `"user"`, `"assistant"`, `"system"`. The `role` inside
`message` is distinct from the envelope `type` — they agree in every
sample we've inspected (`type: "user"` ↔ `role: "user"`,
`type: "assistant"` ↔ `role: "assistant"`), but they are separate
fields in the format. Key type discrimination off the envelope `type`;
`role` carries redundant information.

## Convenience: detecting "empty" user turns

A user entry that carries only `tool_result` parts (no text, no image,
no other user input) is a synthesized tool-result carrier, not a real
user turn. Detect it with:

```python
def is_tool_result_only(entry):
    msg = entry.get("message")
    if not msg or msg.get("role") != "user":
        return False
    content = msg.get("content")
    if isinstance(content, str):
        return False  # bare string is always a real user turn
    # content is an array
    for part in content:
        t = part.get("type")
        if t != "tool_result":
            return False
    return bool(content)
```

UIs rendering a transcript typically fold these into the preceding
assistant turn. See [tools.md](tools.md).
