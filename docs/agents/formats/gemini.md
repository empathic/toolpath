# Gemini CLI conversation format

Reference for the on-disk format produced by Google's
[Gemini CLI](https://github.com/google-gemini/gemini-cli), as consumed by
`toolpath-gemini`. Everything here was established by reading real logs
on disk plus cross-checking with Gemini's own internal documentation
(`cli/session-management.md`, `cli/checkpointing.md`,
`cli/settings.md`, and the tool reference). The Gemini team does not
publish a stable schema, so treat this as observed behaviour as of
**2026-04-24** — forward-compat guards matter.

## Storage root

```
~/.gemini/
  google_accounts.json          Auth token metadata (not our concern)
  oauth_creds.json              OAuth creds
  installation_id               Anonymous install UUID
  settings.json                 User settings (see cli/settings.md)
  state.json                    Ephemeral CLI state
  trustedFolders.json           Folder trust policy
  projects.json                 Absolute-path → friendly-name map
  history/<project_hash>/       Shadow git repo (checkpointing feature, separate from chats)
  tmp/<slot>/                   Per-project slot (one per project)
    .project_root               Absolute project path, plain text
    logs.json                   Lightweight prompt log (array)
    chats/                      Conversation chat files ← this is what we read
    checkpoints/                /restore snapshots (out of scope here)
```

`<slot>` is either the **friendly name** from `projects.json` or the
**SHA-256 hex of the absolute project path**. Both layouts exist in
the wild; a resolver must try friendly-name first and fall back to
the hash.

## `projects.json`

```json
{
  "projects": {
    "/Users/ben/empathic/oss/toolpath": "toolpath",
    "/Users/ben/other/repo": "repo"
  }
}
```

Maps the absolute project root to a user-facing short name. The same
name appears in the `gemini` CLI's UI. When the friendly-name slot
doesn't exist, the hash slot is used as the fallback.

### Project hash

SHA-256 hex of the absolute project path, lowercase, no separators:

```
sha256("/Users/ben/empathic/oss/toolpath")
  = "384e9530e99733805bc2c98a596ab23e67d4c29a6ef263cdc1c89b3bcd022c69"
```

This value also appears as `projectHash` inside every chat file.

## `chats/` — conversation storage

Two-tier layout. A single "conversation" is a main chat file plus an
optional sibling directory for sub-agent chats.

```
chats/
  session-<timestamp>-<short-uuid>.json    Main chat file (kind: "main")
  <full-session-uuid>/                     Sub-agent bucket for that session
    <chat-name>.json                       Sub-agent chat (kind: "subagent")
    <chat-name>.json                       (potentially multiple)
```

- **Main file name**: `session-YYYY-MM-DDTHH-MM-<first-8-chars-of-uuid>.json`.
  Gemini uses `T` as the date/time separator and `-` instead of `:` in
  timestamps to produce a filesystem-safe name.

  **The `session-` prefix is mandatory.** `gemini --list-sessions` and
  `--resume` enumerate only files whose stem starts with `session-`;
  anything else in `chats/` is invisible to the CLI, even if the file's
  internal structure is otherwise valid. A writer that emits, say,
  `chats/<uuid>.json` will get silently skipped and the session cannot
  be resumed. Discovered empirically 2026-04-24 while implementing
  `path incept gemini`.
- **Sub-agent dir name**: the **full** `sessionId` UUID from the main
  file's inner content. E.g. main file carries
  `sessionId: "b26d7f99-0116-4d1d-b125-98c228a4b933"`, so its
  sub-agent dir would be `chats/b26d7f99-0116-4d1d-b125-98c228a4b933/`.
- **Sub-agent chat filenames** are short alphanumeric stems (e.g.
  `qclszz.json`), generated per invocation.

An orphan `<uuid>/` directory without a matching main file is
possible (e.g. if the user deleted the main file); consumers should
gracefully treat the first non-subagent file inside as the main.

### Session resolution (`--resume <id>`)

Gemini CLI accepts **two** identifier forms for `<id>` and resolves them
in this order:

1. **On-disk filename stem.** `<id>` directly matches `chats/<id>.json`.
   So `--resume session-2026-04-17T18-09-b26d7f99` works without ever
   reading a file.
2. **Inner `sessionId` UUID.** If the stem-match misses, the CLI scans
   `chats/session-*.json`, reads each file's top-level `sessionId`
   field, and uses the first file whose value matches `<id>`. This is
   why `--resume b26d7f99-0116-4d1d-b125-98c228a4b933` works even
   though no file is named `b26d7f99-….json` on disk.

The interactive `/resume` browser shows UUIDs in brackets (e.g.
`[b26d7f99-0116-4d1d-b125-98c228a4b933]`); users typically know a
session by its UUID, not its stem. Writers should therefore ensure the
inner `sessionId` field is the full UUID they want `--resume` to accept.

A reader that wants parity with the CLI must implement both resolution
paths — stem match first, then scan-and-match on inner `sessionId`.

## Chat file schema

One JSON object per file (not JSONL). Serialized verbatim on every
turn — Gemini rewrites the whole file rather than appending.

### Top level

```json
{
  "sessionId": "b26d7f99-0116-4d1d-b125-98c228a4b933",
  "projectHash": "384e9530e99733805bc2c98a596ab23e67d4c29a6ef263cdc1c89b3bcd022c69",
  "startTime":   "2026-04-17T18:09:18.567Z",
  "lastUpdated": "2026-04-17T18:12:52.535Z",
  "directories": ["/Users/ben/empathic/oss/toolpath"],
  "kind": "main",
  "summary": "…",
  "messages": [ … ]
}
```

| Field | Type | Notes |
|---|---|---|
| `sessionId` | string | Main files: full UUID. Sub-agent files: short alphanumeric (`qclszz`, 6-8 chars). **Does not** always match the UUID in the directory name — sub-agent files use a short local id. |
| `projectHash` | string | SHA-256 hex of the project root. |
| `startTime` | ISO-8601 UTC | Session start. |
| `lastUpdated` | ISO-8601 UTC | Last write time. Rewritten on every turn. |
| `directories` | array of paths | Workspace directories captured at session start. Sometimes absent (older sessions); sometimes explicit `[]`. |
| `kind` | `"main"` \| `"subagent"` | Canonically present on new files. Older files may omit it — treat absence as "main". |
| `summary` | string | Sub-agents only: the final result returned to the parent. |
| `messages` | array | The conversation, in order. |

### Message level

```json
{
  "id": "868b9468-8d87-4dcf-8d11-15557c20b810",
  "timestamp": "2026-04-17T18:10:02.229Z",
  "type": "gemini",
  "content": "I will create a new Rust project …",
  "thoughts": [ { … } ],
  "tokens":   { … },
  "model": "gemini-3-flash-preview",
  "toolCalls": [ { … } ]
}
```

| Field | Type | Notes |
|---|---|---|
| `id` | string (UUID) | Unique within the file. |
| `timestamp` | ISO-8601 UTC | |
| `type` | `"user"` \| `"gemini"` \| `"info"` \| **unknown** | See "Message types". |
| `content` | string \| array | Shape depends on type — see "Content shapes". |
| `thoughts` | array of `Thought` | Reasoning summary traces. Sometimes `[]`, sometimes absent. |
| `tokens` | `Tokens` object | Per-message token breakdown. Absent on user/info messages. |
| `model` | string | Gemini model id, e.g. `gemini-3-flash-preview`, `gemini-2.5-pro`. On assistant messages only. |
| `toolCalls` | array of `ToolCall` | Tool invocations in this turn. Sometimes `[]`, sometimes absent. |

### Message types

Three observed values; a fourth catch-all is necessary for forward-compat.

| `type` | Who | Content shape | Notes |
|---|---|---|---|
| `"user"` | Human | `[{"text": "…"}]` (Parts) | `content` is always an array of text parts. `toolCalls` absent. |
| `"gemini"` | Model | `"…"` string, or `""` when only tool calls matter | Carries `thoughts`, `tokens`, `model`, optional `toolCalls`. |
| `"info"` | CLI system | `"…"` string | System notifications: `"Request cancelled."`, error banners, rate-limit messages, etc. No `thoughts`/`tokens`/`toolCalls`. |

Future types (e.g. `"plan"` for plan mode) should be accepted without
crashing. A parser should model the role as open (`Other(String)` or
free-text) rather than a closed enum.

### Content shapes

Two variants observed, discriminated by JSON type:

```json
"content": "plain string"
```

```json
"content": [
  { "text": "first part" },
  { "text": "second part" }
]
```

User messages are **always** parts-form. Gemini and info messages are
**always** string-form. This is a soft convention, not enforced —
a permissive parser should accept either on any role.

Parts objects may carry other fields (no observed multimodal content
in text-only sessions, but the shape is clearly designed to accept
image/audio parts).

### Thoughts

Gemini 3 models emit reasoning summaries. Not present on Gemini 2.5
models.

```json
{
  "subject": "Defining Project Scope",
  "description": "I'm currently focused on the project's structure…",
  "timestamp": "2026-04-17T18:10:00.843Z"
}
```

All three fields are optional. Thoughts are additive — the model can
emit multiple per turn. They should be rendered as structured traces,
not concatenated into the visible text.

### Tokens

```json
{
  "input":    8665,
  "output":     94,
  "cached":      0,
  "thoughts":  243,
  "tool":        0,
  "total":    9002
}
```

| Field | Meaning |
|---|---|
| `input` | Prompt + context tokens sent to the model. |
| `output` | Generated tokens (excluding reasoning). |
| `cached` | Tokens reused from Gemini's prompt cache. |
| `thoughts` | Reasoning/thinking tokens (Gemini 3+). |
| `tool` | Tool-result tokens billed separately. |
| `total` | Sum of the above (not always exactly — Gemini's total occasionally includes overhead). |

All fields are optional. `input` → `input_tokens` and `cached` →
`cache_read_tokens` map cleanly to the common `TokenUsage` schema. The
standalone `tool` and `total` counters are Gemini-specific and are
preserved raw in a provider-namespaced extras bucket
(`Turn.extra["gemini"]["tokens"]`).

#### `thoughts` is additive reasoning — folded into `output_tokens`

`thoughts` is **not** a subset of `output`: the doc above states
`output` is "generated tokens *excluding reasoning*," and the recorded
numbers confirm it exactly. Across real sessions
`total == input + output + thoughts` to the token (e.g.
`8665 + 94 + 243 = 9002`; `9562 + 157 + 24 = 9743`), and `thoughts`
routinely *exceeds* `output` (243 vs 94 in the first example).

Google bills reasoning as output, so `thoughts` is a sibling category
of `output`, not a breakdown of it. To avoid **under-counting**
generated tokens, the derived `output_tokens` folds reasoning in:
`output_tokens = output + thoughts` (same convention as opencode, whose
`reasoning` is likewise additive and billed as output). That way the
IR's `output` consistently means "all generated tokens" and a Σ over a
path is the real generated total. `output_tokens` is left `None` only
when both `output` and `thoughts` are absent/zero.

The folded reasoning slice is **also** recorded under
`breakdowns["output"]["reasoning"] = thoughts`. This is informational:
`TokenUsage.breakdowns` is never summed into the total (output already
counts it), and the invariant `Σ(inner) = reasoning ≤ output` holds
because the same number is folded in. The entry is recorded whenever
`thoughts` is **present** (including a genuine `Some(0)`), preserving the
`Some(0)`-vs-absent distinction; only when `thoughts` is absent entirely
does the map stay empty and get omitted from serialization. (For the
worked example, `output_tokens = 94 + 243 = 337` with
`breakdowns["output"]["reasoning"] = 243`.)

Crucially, this record is what makes the **reverse path lossless**: on
projection (`Path → Tokens`) the projector reads
`breakdowns["output"]["reasoning"]` and un-folds reasoning back out of
the folded `output_tokens` (`output = output_tokens − reasoning`,
`thoughts = reasoning`). So `output` and `thoughts` round-trip
losslessly through the IR. Only the Gemini-extra-only `tool`/`total`
counters remain lossy on round-trip — they have no IR home.

The stored `Tokens` struct otherwise carries **no** nested modality
detail (no `candidatesTokensDetails` / `promptTokensDetails`, no
image/text/audio split). Should a future Gemini CLI version persist
genuine modality details
(e.g. `candidatesTokensDetails: [{modality: "IMAGE", tokenCount: …}]`,
which the API exposes but the CLI does not currently write to disk),
that would be a real per-modality split of `output` and could populate
`breakdowns["output"]["image"]` / `["text"]` — but only from those
recorded fields, never fabricated.

### Tool calls

```json
{
  "id": "run_shell_command_1776449402227_0",
  "name": "run_shell_command",
  "args": {
    "command": "mkdir -p ./local/test-python-parser && cargo init",
    "description": "Create and initialize."
  },
  "status": "success",
  "timestamp": "2026-04-17T18:10:02.229Z",
  "result": [
    {
      "functionResponse": {
        "id":   "run_shell_command_1776449402227_0",
        "name": "run_shell_command",
        "response": { "output": "Created binary (application) package" }
      }
    }
  ],
  "resultDisplay": "Created binary (application) package",
  "description": "Create the directory and initialize a new Rust project.",
  "displayName": "Shell",
  "renderOutputAsMarkdown": true
}
```

| Field | Type | Notes |
|---|---|---|
| `id` | string | Two id schemes observed: `<tool_name>_<unix_ms>_<idx>` and `<sessionId>#<turn>-<idx>`. |
| `name` | string | Canonical tool name — see "Tool catalogue". |
| `args` | object | Tool-specific. Never standardized. |
| `status` | string | `"pending"` → (`"executing"` →)? `"success"` \| `"error"` \| `"cancelled"`. |
| `timestamp` | ISO-8601 UTC | When the call was initiated. |
| `result` | array | Zero or more `FunctionResponse` entries. |
| `resultDisplay` | string \| object \| array | **Polymorphic** — see "resultDisplay shapes". |
| `description` | string | Model's rationale for the call. |
| `displayName` | string | UI label (e.g. `"Shell"`, `"WriteFile"`). |
| `renderOutputAsMarkdown` | bool | UI hint. |

Tool results are **inline**: the response lives on the same object as
the call. This is structurally different from Claude Code's format,
which splits tool calls and tool results into separate messages and
requires pairing by `tool_use_id`.

#### Function response body

```json
"response": { "output": "…text…" }
```

The `output` key is the common carrier for string results. More complex
tools (read_file on binary content, etc.) may use alternative keys —
treat `response` as an opaque `Value` and extract `.output` only when
it's a string.

#### `resultDisplay` shapes

At least three observed variants for the UI-friendly render payload:

1. **Bare string** — for simple text output like shell stdout:
   ```json
   "resultDisplay": "Created binary (application) package"
   ```

2. **Object with `fileDiff`** — for file-write tools. The full
   unified diff Gemini rendered in-UI:
   ```json
   "resultDisplay": {
     "fileDiff": "Index: main.rs\n===…\n--- main.rs\tOriginal\n+++ main.rs\tWritten\n@@ -1,3 +1,11 @@\n+use rustpython_parser::{parser, Mode};\n …"
   }
   ```

3. **Nested styled-text array** — for terminal-colored output like
   `cargo` progress:
   ```json
   "resultDisplay": [
     [
       { "text": "    Creating", "bold": true, "fg": "#00ff00",
         "italic": false, "underline": false, "dim": false,
         "inverse": false, "isUninitialized": false, "bg": "" },
       { "text": " binary (application) package", … }
     ],
     [ … next line … ]
   ]
   ```

A parser MUST accept any JSON value here. Typing it as `Option<String>`
crashes on real data.

## Tool catalogue

Canonical names confirmed via Gemini's own `tools/` internal docs plus
observed `toolCalls[].name` values. Keep in sync with
<https://geminicli.com/docs/reference/tools>.

| Tool | Category | Key args |
|---|---|---|
| `read_file` | FileRead | `file_path` |
| `read_many_files` | FileRead | `file_paths` |
| `list_directory` | FileRead | `path` |
| `get_internal_docs` | FileRead | `path` (within Gemini's own docs) |
| `read_mcp_resource` | FileRead | MCP-specific |
| `glob` | FileSearch | `pattern` |
| `grep_search` | FileSearch | `pattern` |
| `search_file_content` | FileSearch | `pattern` |
| `write_file` | FileWrite | `file_path`, `content` |
| `replace` | FileWrite | `file_path`, `old_string`, `new_string`, `instruction` |
| `edit` | FileWrite | alias for `replace` in some versions |
| `run_shell_command` | Shell | `command`, `description` |
| `web_fetch` | Network | `url` |
| `google_web_search` | Network | `query` |
| `task` | Delegation | `prompt`, `subagent_type` — spawns a sub-agent chat |
| `activate_skill` | Delegation | skill id — loads a skill pack |
| `enter_plan_mode` | Planning | — |
| `exit_plan_mode` | Planning | — |
| `write_todos` | Planning | todo list |
| `tracker_create_task` | Planning | |
| `tracker_list_tasks` | Planning | |
| `tracker_get_task` | Planning | |
| `tracker_update_task` | Planning | |
| `tracker_add_dependency` | Planning | |
| `tracker_visualize` | Planning | |
| `update_topic` | Planning | |
| `ask_user` | User I/O | question prompts |
| `save_memory` | Memory | appends to `~/.gemini.md` |
| `list_mcp_resources` | MCP | |
| `complete_task` | Control | Sub-agent self-termination signal |

Tool names are case-sensitive. Unknown tools should leave the
`ToolCategory` as `None` rather than crashing.

## Sub-agents

When Gemini invokes the `task` tool, the CLI spawns a sub-agent with
its own working state and writes a new chat file into
`chats/<parent-sessionId>/<short-stem>.json`. Each sub-agent gets:

- its own `sessionId` (short alphanumeric, NOT a UUID)
- `kind: "subagent"`
- `projectHash` matching the parent
- an initial user-role message carrying the `task` prompt
- subsequent gemini-role messages with its own tool calls
- a final `summary` field populated when the sub-agent terminates

Sub-agent files may contain `toolCalls[].id` values prefixed with the
short session id, e.g. `qclszz#0-0` (session `qclszz`, turn 0, index 0).

Sub-agent results surface to the parent conversation as the `task`
tool's `result[0].functionResponse.response.output`. The parent's
conversation flow continues after the sub-agent terminates.

### Pairing sub-agents to parent invocations

Each sub-agent file's `startTime` is the most reliable way to pair it
with the parent's `task` tool invocation. In document order: the first
`task` call spawns the first sub-agent (by `startTime`), and so on.

There is no explicit back-reference from a sub-agent file to the exact
parent `task.id`.

## `logs.json`

A lightweight per-project log of user prompts, used by
`gemini --list-sessions` for the previewing.

```json
[
  {
    "sessionId": "b26d7f99-0116-4d1d-b125-98c228a4b933",
    "messageId": 0,
    "type": "user",
    "message": "can you write a quick python parser in ./local/test-python-parser/ in rust?",
    "timestamp": "2026-04-17T18:09:58.455Z"
  }
]
```

Redundant with the full chat files — a parser does not need it for
reconstruction. `messageId` is a per-session counter.

## Checkpointing (out of scope for Toolpath)

Gemini has a separate feature controlled by `general.checkpointing.enabled`
in `settings.json`. When enabled, destructive tool calls trigger:

- a shadow git snapshot in `~/.gemini/history/<project_hash>/`
- a conversation-state JSON in `~/.gemini/tmp/<slot>/checkpoints/`
  named like `<timestamp>-<filename>-<tool>.json`

These are separate from the `chats/` hierarchy and not relevant to
conversation ingestion. Toolpath consumers should ignore the
`checkpoints/` and `history/` directories.

## Session rotation — none

Unlike Claude Code, Gemini does **not** rotate chat files on context
overflow or plan-mode transitions. Each session is exactly one
`session-*.json` file (+ optional sub-agent dir). If a new session
starts, a new `session-*.json` is created. There is no chain index to
follow.

## Compaction — in-memory only, never persisted

Gemini CLI **does** compress context — automatically when token usage
crosses a configurable threshold, and manually via `/compress` (aliases
`summarize`/`compact`). Both go through the same
`tryCompressChat` / `ChatCompressionService.compress` path, differing
only by a `force` flag (manual forces; auto gates on the threshold).

But compression is **purely in-memory**. No summary, boundary, or
marker is ever written to the session file — and, per a known
gemini-cli bug (issues #20803 / #21335), the on-disk file isn't even
updated to the compressed state: it retains the **full pre-compression
history**. So a derivation reading the session file always sees the
complete, uncompressed conversation with no compaction event in it.

Net for us: still **no compaction provenance to model** and no
duplicate-id hazard — but the reason is "compresses but persists
nothing," not "no compaction mechanism." (The `summary` field on the
format remains a sub-agent's reported result — see
[§Sub-agents](#sub-agents) — not a context summary.)

## Timestamps and encoding

- All timestamps are ISO-8601 UTC with millisecond precision and a
  `Z` suffix.
- All strings are UTF-8.
- JSON output uses `\n` (LF) newlines, standard number formatting,
  pretty-printing with 2-space indentation.
- Key ordering is not canonical — Gemini's Node.js serializer preserves
  insertion order, which varies between messages in the same file.

## Round-trip fidelity gotchas

These behaviours matter for anyone parsing and re-emitting the JSON:

1. **Absent vs empty array must be distinguished.** `directories`,
   `thoughts`, and `toolCalls` all appear sometimes as absent and
   sometimes as explicit `[]`. Use `Option<Vec<T>>` (or equivalent)
   to preserve the distinction. Conflating them WILL produce
   round-trip divergence.
2. **`resultDisplay` is polymorphic.** Never type it as a string.
3. **Unknown `type` values must not crash the parser.** Gemini has
   added at least one new role (`info`) post-1.0; more will come.
4. **`kind` may be absent on older files.** Treat absence as `"main"`.
5. **Message-level and chat-level unknown fields occur.** Catch them
   via `#[serde(flatten)]` or equivalent. Dropping them violates
   fidelity.
6. **Nulls are meaningful on `Option` fields.** A field explicitly
   set to `null` is different from absence; if your serializer drops
   `null` on re-emit, you lose that signal.
7. **Main chat files MUST be named `session-*.json`.** See "Session
   resolution" above — the CLI filters `chats/*.json` by stem prefix
   before even opening files, so a mis-named main is silently
   unreachable from `--list-sessions` / `--resume`. This is the single
   most load-bearing filename convention in the format.
8. **Inner `sessionId` is load-bearing for `--resume`.** Because
   `--resume <uuid>` matches against the file's inner `sessionId`
   field (not the stem), writers targeting a specific resume identity
   must set `sessionId` to the intended UUID. A mismatched
   `sessionId` makes the session resumable by stem only.

## Feature-dependent fields

Not every Gemini build emits the same fields. Known variations:

- **`general.plan.enabled`** controls whether plan-mode messages
  appear (`type: "plan"` or equivalent).
- **`experimental.topicUpdateNarration`** changes how `update_topic`
  calls are framed.
- **Gemini 3 reasoning** — only Gemini 3+ emits `thoughts[]` and
  `tokens.thoughts`.
- **MCP servers** — custom MCP tools surface with their configured
  names, not in the catalogue above.

## Model aliases

CLI-level aliases (resolved before writing to the `model` field):

| Alias | Resolves to |
|---|---|
| `auto`, `pro` | `gemini-2.5-pro` or `gemini-3-pro-preview` |
| `flash` | `gemini-2.5-flash` |
| `flash-lite` | `gemini-2.5-flash-lite` |

The `model` field inside the chat file always holds the concrete
model id, never the alias.

## References

- Gemini CLI repository: <https://github.com/google-gemini/gemini-cli>
- Official docs: <https://geminicli.com>
- Tool reference: <https://geminicli.com/docs/reference/tools>
- Session management: `cli/session-management.md` (shipped inside the
  CLI; retrieve via `get_internal_docs`)
- Checkpointing: `cli/checkpointing.md` (same)
- Settings reference: `cli/settings.md` (same)

The Gemini team does not publish a stable schema. Assume this
document drifts; re-verify when a new CLI minor version appears.
