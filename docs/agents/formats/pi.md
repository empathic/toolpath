# Pi (pi.dev) conversation format

Reference for the on-disk format produced by [Pi](https://pi.dev), as
consumed by `toolpath-pi`. Pi calls itself a terminal coding agent and
writes sessions as JSONL files. This doc captures the format observed
in real Pi sessions plus the schema as defined in
`crates/toolpath-pi/src/types.rs`. Treat it as observed behaviour as
of **2026-04-27** (format version 3).

## Storage root

```
~/.pi/agent/sessions/
  --<encoded-cwd>--/                    Per-project session directory
    <date>_<uuid>.jsonl                 One session per file
    <uuid>.jsonl                        Either naming form is accepted
```

`<encoded-cwd>` is the absolute working directory with the leading `/`
dropped and remaining slashes replaced by hyphens, wrapped in `--…--`:

```
/Users/ben/empathic/oss/toolpath
  ──▶  --Users-ben-empathic-oss-toolpath--
```

Lossy for paths containing literal `-` characters; Pi has the same
limitation. The wrapping `--…--` makes the encoded form
unambiguously a Pi project dir.

## File shape

Each `.jsonl` file is one entry per line. The first line is always a
**session header** (`type: "session"`); every subsequent line is an
**entry**. Entries form a tree via `id` / `parentId`, though most
sessions are linear.

### Session header

```json
{
  "type": "session",
  "version": 3,
  "id": "demo-session-1",
  "timestamp": "2026-04-16T10:00:00Z",
  "cwd": "/Users/alex/demo-project",
  "parentSession": "/Users/.../parent.jsonl"
}
```

| Field | Type | Notes |
|---|---|---|
| `type` | `"session"` | Required discriminant. |
| `version` | int | Format version. Currently 3. |
| `id` | string | Session id. Used by `path p import pi --session <id>`. |
| `timestamp` | ISO-8601 | Session start. |
| `cwd` | string | Project working directory. Pi's project encoder operates on this. |
| `parentSession` | string | Optional. Path to a parent JSONL file when this session was forked. |

The header may carry forward-compat fields under `extra` (flattened
into the JSON object via serde).

### Entry types

Tagged by `type`. Every non-session entry shares an [`EntryBase`]
(`id` / `parentId` / `timestamp`) flattened into the payload.

| `type` | What it represents |
|---|---|
| `message` | The dominant entry type. Wraps an [`AgentMessage`] (User, Assistant, ToolResult, BashExecution, Custom, …). |
| `model_change` | Model/provider switch mid-session. |
| `thinking_level_change` | Pi-specific reasoning budget toggle. |
| `compaction` | Context compaction marker — older entries summarised away. |
| `branch_summary` | Branch checkpoint. |
| `custom` | Arbitrary structured event with `customType` + `data` map. |
| `custom_message` | Custom-typed message with displayable content. |
| `label` | Lightweight marker entry. |

Future entry types should round-trip via `extra` — Pi has added
variants between minor versions.

#### `compaction` entry fields

The `Compaction` entry (`crates/toolpath-pi/src/types.rs`) carries:

| Field | Type | Notes |
|---|---|---|
| `summary` | string | The summary that replaces the discarded prefix. |
| `firstKeptEntryId` | string | First entry **not** discarded — everything before it was summarized. A single contiguous-tail anchor. |
| `tokensBefore` | u64 | Context token count before compaction. |
| `details` | object? | Optional opaque detail. |
| `fromHook` | bool? | `true` if an **extension** supplied the summary (via the `session_before_compact` hook); `false`/absent for Pi's default compaction. **Not** an auto-vs-manual flag — manual `/compact` and automatic compaction both use Pi's default path and produce the same entry. (Legacy field name.) |

Compaction is an **in-file** marker on the existing id/parentId tree —
it does **not** start a new session or reuse entry ids, so there's no
duplicate-id hazard. (The separate `parentSession` header field links a
*forked/resumed* session to a parent file; that is unrelated to
compaction.)

**Projecting a foreign compaction into pi is lossy by format.** pi's entry
can't carry everything the cross-harness IR holds, so a `Compaction` from
another harness is coerced on the way in: `trigger` is dropped (no
auto-vs-manual concept — see `fromHook` above), `pre_tokens` becomes `0`
when unknown (`tokensBefore` is a mandatory `u64`), and `kept` is never
empty — a "wholesale" boundary that kept nothing gains the
`firstKeptEntryId` anchor. After a round-trip through pi you therefore
can't distinguish a real `0` from an unknown pre-token count, nor a
kept-nothing boundary from one that kept a single turn.

### Message roles

`message` entries wrap an `AgentMessage` discriminated by `role`:

| Role | Notes |
|---|---|
| `user` | Human input. Content may be a bare string or `[{type:"text",text:…}]`. |
| `assistant` | Model output. Content is an array of blocks: `text`, `thinking`, `toolCall`. Carries `model`, `provider`, `api`, `usage`, `stopReason`. |
| `toolResult` | Result of a previous `toolCall`. Carries `toolCallId` (matches the call's `id`), `toolName`, `content` (text or image blocks), `isError`. **Tool calls and results are always separate entries** in Pi's format. |
| `bashExecution` | Distinct shell-execution shape with `command` / `output` / `exitCode` / `cancelled` / `truncated`. |
| `custom` | Custom-typed message with `customType` and a `display` boolean. |
| `branchSummary` / `compactionSummary` | Inline summary roles attached to the corresponding entry types. |

### Assistant content blocks

```json
"content": [
  { "type": "thinking", "thinking": "…reasoning…" },
  { "type": "text", "text": "I'll write the file." },
  {
    "type": "toolCall",
    "id": "tc-1",
    "name": "write",
    "arguments": { "path": "hello.rs", "content": "fn main(){}" }
  }
]
```

Block types: `text`, `thinking`, `toolCall`, `image`. Order
significant — projectors should emit `thinking` first, then `text`,
then `toolCall` blocks (matching Pi's typical layout).

### Tool call → tool result correlation

Tool calls live inside an assistant message's content. Their results
appear as separate `toolResult`-role messages in subsequent entries,
correlated by `toolCallId == toolCall.id`:

```
m2: assistant content=[..., {toolCall id:"tc-1" name:"write" args:…}]
m3: toolResult toolCallId="tc-1" toolName:"write" content:[{text:"file written"}] isError:false
```

This is structurally different from Gemini's inline-result format and
from Claude's "tool_use_id" pairing inside a single message — Pi
splits them across two entries by design.

### Token usage

```json
"usage": {
  "input": 100,
  "output": 50,
  "cacheRead": 0,
  "cacheWrite": 0,
  "totalTokens": 150,
  "cost": {
    "input": 0.0,
    "output": 0.0,
    "cacheRead": 0.0,
    "cacheWrite": 0.0,
    "total": 0.0
  }
}
```

`usage` is **per API call** (per assistant message), not cumulative.
`totalTokens`'s formula is **version-dependent and not load-bearing for us**:
older Pi reported `input + output`, but Pi 0.2.0+ redefined its headline
token metric to `input + output + cacheWrite` (cacheRead deliberately
excluded so repeated cache hits don't dominate). `toolpath-pi` does **not**
read `totalTokens` — it reads the raw `input`/`output`/`cacheRead`/`cacheWrite`
fields and sums each independently, so it's correct regardless of which
`totalTokens` convention a session used. The `cost` breakdown is
Pi-specific; not present in real sessions where cost can't be computed.

### Stop reasons

`stopReason` values observed: `stop`, `length`, `toolUse`, `error`,
`aborted`. Unknown values round-trip through `StopReason::Other` so
new Pi versions don't break parsers.

Note the snake_case-vs-camelCase inconsistency: Claude's source
sessions use `tool_use` (snake_case) which round-trips through
`StopReason::Other("tool_use")` rather than mapping to
`StopReason::Known(KnownStopReason::ToolUse)` (`"toolUse"`). Pi's
reader accepts either form.

## Tool catalogue

Pi's classifier recognises (lowercased) tool names:

| Tool | Category |
|---|---|
| `read` | FileRead |
| `write`, `edit` | FileWrite |
| `bash`, `shell`, `run`, `exec` | Shell |
| `grep`, `glob`, `find`, `ls` | FileSearch |
| `webfetch`, `websearch`, `fetch` | Network |
| Names containing `task` or `agent` | Delegation |

Names are case-insensitive on classification; canonical native names
emitted by `toolpath-pi::provider::native_name` are lowercase
(`bash`, `read`, `edit`, etc.).

## Session resolution

`path p import pi --session <id>` resolves `<id>` against:

1. The header `id` field of every JSONL file in the project's
   sessions directory (line-1 peek).
2. Failing that, the file stem — both `<date>_<uuid>` and bare `<uuid>`
   forms accepted.

There is no equivalent of Gemini's `session-` filename prefix
constraint; Pi reads any `*.jsonl` file in the project directory.

## Round-trip fidelity gotchas

1. **Tool calls and results are separate entries.** Projectors
   producing Pi sessions must emit one `assistant` entry containing
   a `toolCall` block PLUS one separate `toolResult` entry. Emitting
   only the former (or duplicating the result inline) breaks
   correlation.
2. **`Compaction` / `BranchSummary` are first-class entry types**, not
   roles. The forward path stashes structure markers under
   `Turn.extra["pi"]["compaction"]` / `["branchSummary"]`; the
   projector reads those to decide whether to emit `Entry::Compaction`
   vs `Entry::Message`.
3. **Inner message `timestamp` is u64 epoch milliseconds**, not an
   ISO-8601 string. The outer `EntryBase.timestamp` IS the ISO string.
   Two timestamp fields per message — keep them in sync on round-trip.
4. **Bash executions get a synthetic `bash` ToolInvocation in the
   forward path** so cross-harness consumers see a uniform tool-call
   shape. The reverse maps it back to `AgentMessage::BashExecution`.
5. **Pi's project encoder is lossy on paths with `-`.** A cwd of
   `/foo-bar/baz` encodes the same as `/foo/bar/baz`. Pi accepts the
   ambiguity; round-tripping a session with such a cwd may land it in
   a different project directory than it came from.

## References

- Schema source: `crates/toolpath-pi/src/types.rs` (per-variant doc
  comments are authoritative).
- Path resolution: `crates/toolpath-pi/src/paths.rs`.
- Forward derivation: `crates/toolpath-pi/src/provider.rs::session_to_view`.
- Reverse projection: `crates/toolpath-pi/src/project.rs::PiProjector`.
- Pi homepage: <https://pi.dev>.

The Pi team does not publish a stable schema; treat this as a snapshot
and re-verify when a new Pi minor version appears.
