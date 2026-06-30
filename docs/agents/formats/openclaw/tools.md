# Tool calls, results, and file operations

OpenClaw records a tool invocation as a `toolCall` content block inside an
assistant message, and the result as a **separate** `toolResult` message
entry. There is no inline result block and — importantly — **no stored
diff**. This shapes how a derived `Path` can represent file changes.

## Call and result are separate entries

The call is an assistant content block
([messages.md §toolCall](messages.md#toolcall-llm-coresrctypests251-258)):

```json
{ "type": "toolCall", "id": "call_1", "name": "edit_file",
  "arguments": { "path": "src/x.ts", "old": "…", "new": "…" } }
```

The result is its own `message` entry of role `toolResult`
(`llm-core/src/types.ts:306-314`):

```json
{ "type": "message", "id": "0190ab13", "parentId": "0190ab12",
  "timestamp": "2026-06-30T12:00:05.500Z",
  "message": { "role": "toolResult", "toolCallId": "call_1",
    "toolName": "edit_file",
    "content": [ { "type": "text", "text": "edited 1 file" } ],
    "isError": false, "timestamp": 1751284805500 } }
```

| Field | Shape | Notes |
|---|---|---|
| `toolCallId` | string | Links back to `toolCall.id`. The correlation key. |
| `toolName` | string | Tool name (mirrors the call's `name`). |
| `content` | `(text\|image)[]` | What the model sees as the result. |
| `details` | `unknown` (optional) | Arbitrary structured payload; per-tool, **unconstrained on the wire**. A tool *could* stash structured data (even a diff) here, but the core type defines nothing. |
| `isError` | bool | **The only error signal.** On failure, `isError:true` with the error text placed in `content`; there is no separate error-string field. |
| `timestamp` | int (epoch ms) | Inner message timestamp ([messages.md](messages.md#the-two-timestamp-encodings)). |

This call-in-assistant / result-as-separate-entry split is the same design
as Pi, and different from Claude (`tool_use`/`tool_result` paired inside one
turn) and Gemini (inline result). A projector writing OpenClaw JSONL must
emit the `toolCall` block **and** a separate `toolResult` entry correlated
by id.

## File operations: tool-input only, no raw diff

**OpenClaw does not persist a structured patch or before/after content for
file edits.** A file edit is just a tool call whose `arguments` name the
path (and, for an edit tool, the old/new text). To know *what changed* you
must interpret each tool's argument schema, which is tool-specific.

The diagnostic trajectory exporter confirms this: `buildTranscriptEvents`
(`src/trajectory/export.ts`) emits `tool.call` events as
`{ toolCallId, name, arguments, blockIndex }` straight from the assistant
content blocks — there is **no diff extraction** anywhere.

The only structured "files touched" view is a **server-side derivation**,
not something stored on entries — `SessionFileEntrySchema`
(`gateway-protocol/src/schema/sessions.ts:78-89`), served by
`sessions.files.list`:

```ts
{ path, name, kind: "modified" | "read", missing: boolean, size?, updatedAtMs?, content? }
```

It classifies each touched path as `modified` vs `read` (a browser rollup
adds `mixed`) but provides **no hunks** — at most the file's current
`content`, never a before/after pair.

### Consequence for toolpath derivation

A derived `Path` can carry **structural / tool-input-derived** file changes
only — there is **no `raw` (unified-diff) perspective** available from the
transcript. This is materially weaker than Codex (whose `patch_apply_end`
carries the diff or full content) and opencode (git tree↔tree diffs), and
parallels opencode's *fallback* behavior for gitignored paths. Recover the
touched-file list from `toolCall.arguments` (or `sessions.files.list` if the
gateway is queried), and mark changes structural with no raw perspective.

## Tool name classification

Tool names are free-form strings. A toolpath provider will want a
classifier (read / write / shell / search / network / delegation) keyed on
the lowercased `name`, the way `toolpath-pi` does — OpenClaw does not ship a
canonical category enum in the transcript.
