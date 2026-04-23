# Tool calls

Tool calls are the most complex part of the format. A single tool
invocation produces **two JSONL entries** plus optional top-level
summary data, and the pairing has to be reconstructed by the reader.

## The two-entry lifecycle

A tool call is always a pair:

1. **Assistant entry** with a `tool_use` content part in
   `message.content`. This is the call.
2. **User entry** whose `message.content` is an array of `tool_result`
   parts. This is the response.

Example (abbreviated):

```jsonc
// 1) Assistant issues the call
{
  "type": "assistant",
  "uuid": "3995c068-...",
  "message": {
    "role": "assistant",
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_01LJtcvehSnXfMztfk8b8ZLC",
        "name": "Grep",
        "input": {"pattern": "TODO"}
      }
    ],
    "stop_reason": "tool_use"
  }
}

// 2) User entry (synthesized, not human-typed) carries the result
{
  "type": "user",
  "uuid": "1e5e8efb-...",
  "parentUuid": "3995c068-...",
  "sourceToolAssistantUUID": "3995c068-...",
  "message": {
    "role": "user",
    "content": [
      {
        "type": "tool_result",
        "tool_use_id": "toolu_01LJtcvehSnXfMztfk8b8ZLC",
        "content": "src/main.rs\nsrc/lib.rs\n"
      }
    ]
  },
  "toolUseResult": {
    "mode": "files_with_matches",
    "filenames": ["src/main.rs", "src/lib.rs"],
    "numFiles": 2
  }
}
```

The **primary key** linking the two is `tool_use.id` ↔
`tool_result.tool_use_id`. The envelope also provides a convenience
back-reference: the tool-result-carrying user entry has
`sourceToolAssistantUUID` pointing at the `uuid` of the issuing
assistant entry.

## One assistant entry, multiple tool calls

An assistant entry can include multiple `tool_use` parts in its
`content` array. Each gets its own matching `tool_result` part in
the following user entry's `content` array.

```jsonc
// Assistant: two tool calls in one entry
{"content": [
  {"type": "tool_use", "id": "toolu_A", "name": "Read",  "input": {...}},
  {"type": "tool_use", "id": "toolu_B", "name": "Grep",  "input": {...}}
]}

// User: two results matched by ID
{"content": [
  {"type": "tool_result", "tool_use_id": "toolu_A", "content": "..."},
  {"type": "tool_result", "tool_use_id": "toolu_B", "content": "..."}
]}
```

Parallel tool calls — multiple `tool_use` parts in a single assistant
entry — are a normal pattern that agents use to fan out reads.

## Tool result `content` shape

The `content` field on a `tool_result` is either a string or an array
of text-carrying objects:

```jsonc
// String form
{"type": "tool_result", "tool_use_id": "...", "content": "file contents"}

// Array form
{"type": "tool_result", "tool_use_id": "...", "content": [
  {"text": "line 1"},
  {"text": "line 2"}
]}
```

To recover the output, join array-form parts with `\n`.

## The top-level `toolUseResult`

The tool-result-carrying user entry may also have a top-level
`toolUseResult` field (sibling of `message`, not nested inside it).
This is a **structured summary** of the tool's output, populated for
tools that have structured outputs.

Whether `toolUseResult` is present depends on the tool. Tools with
structured outputs emit it; tools whose output is a single blob of text
or a status string leave it absent.

| Tool        | Top-level `toolUseResult`? | Inline `tool_result.content`? |
|-------------|----------------------------|-------------------------------|
| `Read`      | no                         | yes — file contents as string |
| `Write`     | no                         | yes — success string          |
| `Edit`      | no                         | yes — diff or success string  |
| `Bash`      | no                         | yes — stdout/stderr as string |
| `Grep`      | **yes**                    | yes — human-readable summary  |
| `Glob`      | **yes**                    | yes                           |
| `TodoWrite` | **yes**                    | yes                           |
| `Task`      | **yes**                    | yes — agent summary           |
| `WebSearch` | **yes**                    | yes                           |
| `WebFetch`  | **yes** (when the fetch succeeds) | yes                    |

MCP-provided tools follow their server's conventions rather than
Claude Code's. We have not seen `toolUseResult` populated for MCP tools
in the samples we've inspected.

### Observed `toolUseResult` shapes

**`Grep`:**
```json
{
  "mode": "files_with_matches",
  "filenames": ["packages/.../MainHeader.svelte", "..."],
  "numFiles": 79
}
```

Alternative `mode` values: `"content"` (shows matching lines),
`"count"` (per-file match counts), mirroring the tool's `output_mode`
parameter.

**`Glob`** (expected shape):
```json
{"filenames": ["src/a.rs", "src/b.rs"], "durationMs": 12}
```

**`TodoWrite`** (expected shape):
```json
{"todos": [{"content": "...", "status": "completed", "activeForm": "..."}, ...]}
```

**`Task`** (expected shape):
```json
{
  "agentId": "a7bf2fd",
  "totalTokens": 12345,
  "totalToolUseCount": 7,
  "usage": {...},
  "result": "...summary text..."
}
```

### Spilled-to-disk outputs

Very large tool outputs may be spilled to files under
`projects/<project>/<session>/tool-results/` rather than inlined into
`tool_result.content`. In that case the content field contains a
reference. We haven't captured a concrete example yet — parsers that
assume content is always inline may break on very long outputs.

## Common tool `input` shapes

The built-in tool set is **not stable** — Anthropic ships new tools,
renames old ones, and adds/removes input fields between Claude Code
releases. The list below is illustrative, not canonical; it captures
what we've observed in Claude Code 2.1.x samples. Consult Anthropic's
tool documentation for the authoritative current shapes, and treat any
unfamiliar field on a known tool as additive drift rather than a
parsing error.

- **`Read`** — `{file_path, offset?, limit?, pages?}`
- **`Write`** — `{file_path, content}`
- **`Edit`** — `{file_path, old_string, new_string, replace_all?}`
- **`Bash`** — `{command, description?, timeout?, run_in_background?}`
- **`Grep`** — `{pattern, path?, glob?, type?, output_mode?, -i?, -A?, -B?, -C?, -n?, head_limit?, offset?, multiline?}`
- **`Glob`** — `{pattern, path?}`
- **`WebFetch`** — `{url, prompt}`
- **`WebSearch`** — `{query, allowed_domains?, blocked_domains?}`
- **`Task`** / **`Agent`** — `{description, prompt, subagent_type?}`
- **`TodoWrite`** — `{todos: [{content, status, activeForm}]}`
- **`NotebookEdit`** — `{notebook_path, cell_id?, new_source, cell_type?, edit_mode?}`

Tools provided by MCP servers appear as `mcp__<server>__<tool>`.
The `input` schema is controlled by the MCP server, not Claude Code —
the list above has nothing to say about them.

## Consumer concerns

### Cross-boundary pairing

When watching a live session, a `tool_use` may appear in one read
window and its `tool_result` in a later one. A consumer that emits
turns eagerly needs a "late join" step that merges the result back
into the earlier assistant turn once it arrives.

### Permission prompts leave no trace

Claude Code can gate tool execution behind a permission prompt. The
prompt itself and the user's accept/deny decision are **not** recorded
in the JSONL. The only signal is a gap in timestamps between the
`tool_use` and the `tool_result`. A denied tool call still eventually
produces a `tool_result` (with `is_error: true` or a denial message).

### Synthesized user entries

The user entry carrying `tool_result`s was not typed by a human. A
transcript-rendering UI should usually fold it into the preceding
assistant turn rather than show it as "the user said …". Detection:
the entry is `type: "user"`, the only content-part kinds are
`tool_result`, and there's a `sourceToolAssistantUUID` envelope field.
