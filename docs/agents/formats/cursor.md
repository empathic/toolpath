# Cursor session format

Reference for the on-disk format produced by
[Cursor](https://cursor.com) — what a `toolpath-cursor` provider would
need to read. Compiled from direct inspection of real sessions on this
machine (Cursor `3.6.-main`, recorded 2026-06-01); Cursor is closed
source, so there is no upstream type definition to cross-check against
and every claim here is empirical.

Date: **2026-06-01**. Revisit when the Cursor minor version bumps or a
`_v` field on any object increments.

## Storage roots

Cursor splits its data across two roots: an Anysphere-specific tree at
`~/.cursor/` and a VS Code-flavored Electron user-data tree under
`~/Library/Application Support/Cursor/`. Conversations are mirrored
across both — leanly in the former, fully in the latter.

### `~/.cursor/` — Anysphere user tree

```
~/.cursor/
  .gitignore
  argv.json                          Electron startup flags
  extensions/                        VS Code-compatible extensions
  plugins/                           Cursor-specific plugin pack
  skills-cursor/                     Built-in skill bundles (automate, babysit, canvas, …)
  ai-tracking/
    ai-code-tracking.db              SQLite — per-commit + per-file AI authorship stats
  projects/
    <project-slug>/                  One folder per workspace
      agent-transcripts/
        <composer-uuid>/
          <composer-uuid>.jsonl      ← the agent's I/O transcript
      canvases/
      mcps/
      terminals/
        <id>.txt                     Snapshot of a terminal at request time
```

Project slugs use one of two conventions:

| Project type | Slug |
|---|---|
| Real on-disk path | Absolute path with `/` → `-`, leading `/` stripped (`Users-ben-projects-temp-cursortest`) |
| `/var/folders/…` tmp | Same scheme but very long (`var-folders-jk-…-T-<uuid>`) |

Untitled / remote workspaces use a numeric millisecond timestamp
instead and have no `agent-transcripts/` subfolder.

`agent-transcripts/<composer-uuid>/<composer-uuid>.jsonl` is **only
written when the composer ran in agent mode** (`unifiedMode: "agent"`,
`isAgentic: true`). Plain chat sessions don't get a JSONL. There's
exactly one transcript file per composer; the inner directory name and
filename are both the same UUIDv4 as the composer id.

### `~/Library/Application Support/Cursor/` — Electron user data

```
~/Library/Application Support/Cursor/
  3.6.-main.sock                     IPC socket (suffix is Cursor minor version + channel)
  machineid                          36-byte machine UUID (telemetry)
  Preferences                        Chromium prefs
  argv.json
  languagepacks.json
  User/
    settings.json                    VS Code settings.json (Cursor-aware)
    keybindings.json
    snippets/
    History/                         Per-file local edit history (VS Code)
    workspaceStorage/
      <workspace-id>/                Per-workspace state
        workspace.json               { "folder": "file:///abs/path" }
        state.vscdb                  SQLite — per-workspace UI + AI state
        state.vscdb-shm / -wal
        anysphere.cursor-retrieval/
          embeddable_files.txt
          high_level_folder_description.txt
    globalStorage/
      state.vscdb                    ← primary cross-workspace SQLite
      state.vscdb-shm / -wal
      storage.json                   VS Code storage shim
      anysphere.cursor-commits/
        checkpoints/<request-uuid>/  Per-agent-request file snapshot
          metadata.json
          diffs/<file-uuid>          Structured diff (lossless add/modify/delete)
          files/<file-uuid>          Often 0 bytes (placeholder; content is in cursorDiskKV)
  logs/<YYYYMMDDThhmmss>/window<N>/  Renderer logs
  sentry/                            Crash reports
  Cache/ Code Cache/ GPUCache/ …     Chromium caches (uninteresting)
```

Workspace ids are 32 hex chars when there's a real folder (an MD5-like
hash of the folder URI — the exact normalization is undocumented and
doesn't match a naive `md5(uri)`; treat as opaque) and numeric
millisecond timestamps for untitled/remote workspaces.

## The two conversation representations

A single agent conversation is materialized **twice**, in two stores
that overlap but neither subsumes the other:

| Store | Granularity | Contains | Lossy of |
|---|---|---|---|
| `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl` | One line per logical turn | The model's serialized I/O: user prompt + each assistant turn's text + `tool_use` blocks | **Tool results, thinking, file diffs, status, timestamps, model name, token counts** |
| `globalStorage/state.vscdb` → `cursorDiskKV` (`composerData:` + `bubbleId:`) | One row per UI message bubble | Full per-bubble state: tool params + results + diffs, thinking blocks, file snapshots, model info, checkpoint refs, capability metadata | The "as sent to the model" wire form (rebuilt from bubbles) |

For provenance fidelity, **the SQLite bubble store is the source of
truth**. The JSONL transcript is fast, file-keyed-by-project, and
human-readable, but it's the agent's I/O log, not the UI/state log —
it drops everything Cursor's UI knows that the model didn't see.

## `~/.cursor/projects/.../<id>.jsonl` — the agent transcript

One JSON object per line, terminated by `\n`. No header, no envelope —
the first line is already a turn.

```json
{ "role": "user",      "message": { "content": [ … ] } }
{ "role": "assistant", "message": { "content": [ … ] } }
```

| Field | Type | Notes |
|---|---|---|
| `role` | `"user"` \| `"assistant"` | No `tool` / `system` roles in any session observed |
| `message.content` | array | One or more content items |
| `message.content[].type` | `"text"` \| `"tool_use"` | The only two variants observed |
| `message.content[].text` | string | Present on `text` items |
| `message.content[].name` | string | Tool name on `tool_use` items (Cursor's agent-side names, see below) |
| `message.content[].input` | object | Tool arguments on `tool_use` items, **already parsed** (not a JSON string) |

The user's first message is wrapped in `<user_query> … </user_query>`:

```json
{"role":"user","message":{"content":[
  {"type":"text","text":"<user_query>\nCreate a basic python interpreter and runtime in rust.\n</user_query>"}
]}}
```

There is **no `tool_result` content item**. The model emits a
`tool_use` and then, in the next line's `assistant` turn, narrates the
outcome — Cursor presumably feeds the tool result back to the model
in memory but never serializes it into the transcript. A reader that
needs results must cross-reference the bubble store.

### Agent-side tool names (observed in JSONL)

These are Cursor's friendly tool names as the agent receives them:

| Name | Input shape |
|---|---|
| `Glob` | `{ target_directory, glob_pattern }` |
| `Read` | `{ path, … }` |
| `Write` | `{ path, contents }` |
| `StrReplace` | `{ path, old_string, new_string }` |
| `Shell` | `{ command, description }` |

The bubble store records the *same* call under a different
**internal** name (`edit_file_v2`, `run_terminal_command_v2`,
`read_file_v2`, `glob_file_search`, …) plus a numeric `tool` id —
see the [tool catalogue](#tool-catalogue) below.

### Redaction artifact

Every assistant `text` item we sampled has the literal string
`"[REDACTED]"` as its payload (often as the entire text, sometimes
following a one-sentence summary like *"Exploring the workspace…"*).
This appears to be a client-side redaction of inline assistant text
(rationale: Cursor's TOS / privacy mode strips reasoning from the
on-disk log). The bubble store keeps the real text. A reader should
treat `[REDACTED]` as a sentinel — don't try to render it as the
model's words.

## `globalStorage/state.vscdb` — the bubble store

A SQLite database with two tables:

```sql
CREATE TABLE ItemTable     (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
CREATE TABLE cursorDiskKV  (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
```

`ItemTable` holds VS Code-style global preferences (`composer.composerHeaders`,
`cursorAuth/accessToken`, theme data, …). `cursorDiskKV` is Cursor's
own key-value store and holds everything conversation-related. The
values are UTF-8 JSON unless otherwise noted.

### `cursorDiskKV` key namespaces

| Prefix | Shape of suffix | Per-row payload |
|---|---|---|
| `composerData:<composer-uuid>` | UUIDv4 | Full per-composer/session metadata |
| `bubbleId:<composer-uuid>:<bubble-uuid>` | composer + bubble UUIDv4 | One UI message bubble (user prompt, assistant text, tool call, thinking…) |
| `composer.content.<hash>` | 64 hex chars | Content-addressed file blob (raw bytes) |
| `agentKv:blob:<hash>` | 64 hex chars | Same scheme, duplicate / agent-backend mirror |
| `inlineDiff:<workspace-id>:<diff-uuid>` | hex + UUID | Structured per-edit diff (apply/reject record) |
| `ofsContent:<composer-uuid>:<file-uri>` | composer + full URI | Snapshot of file content as the agent saw it (often 0 bytes, used as a presence marker) |

Plus `ItemTable.composer.composerHeaders` — the cross-workspace
composer index (see below).

## `composer.composerHeaders` — the session index

Stored in `ItemTable`, not `cursorDiskKV`. One blob, one JSON object
with an `allComposers` array. Each entry is a short header (no
turn-level content) used by the sidebar to enumerate sessions without
loading their bubbles.

```json
{
  "allComposers": [
    {
      "type": "head",
      "composerId": "724686cd-875e-47da-a90b-dbc3e523efb8",
      "name": "Python interpreter and runtime in Rust",
      "subtitle": "Edited tokens.rs, lexer.rs, README.md, interpreter.rs, Cargo.toml",
      "createdAt": 1780325474978,
      "lastUpdatedAt": 1780325508923,
      "conversationCheckpointLastUpdatedAt": 1780325958617,
      "unifiedMode": "agent",
      "forceMode": "edit",
      "hasUnreadMessages": false,
      "contextUsagePercent": 30.4685,
      "totalLinesAdded": 2034,
      "totalLinesRemoved": 0,
      "filesChangedCount": 13,
      "hasBlockingPendingActions": false,
      "hasPendingPlan": false,
      "isArchived": false,
      "isDraft": false,
      "isWorktree": false,
      "isSpec": false,
      "isProject": false,
      "isBestOfNSubcomposer": false,
      "numSubComposers": 0,
      "referencedPlans": [],
      "trackedGitRepos": [],
      "workspaceIdentifier": {
        "id": "93c175a1e1761d404ef54f2a5f463464",
        "uri": {
          "$mid": 1,
          "fsPath": "/Users/ben/projects/temp/cursortest",
          "external": "file:///Users/ben/projects/temp/cursortest",
          "path": "/Users/ben/projects/temp/cursortest",
          "scheme": "file"
        }
      }
    }
  ]
}
```

Notable bits:

- `type: "head"` is the only value observed; presumably reserves room
  for non-head entries (sub-composers, branched threads).
- `name` is the conversation title — Cursor's autogenerated summary
  of the first user message, not the message itself.
- `unifiedMode`: `"agent"` (the full coding agent), `"chat"` (Q&A),
  with `forceMode` (`"edit"`, …) as an override.
- `workspaceIdentifier.uri.$mid: 1` is the VS Code `URI` tag — the
  fields are reserialized verbatim from `URI.toJSON()` and round-trip
  through `URI.from(…)`.
- The list contains composers that have **no `bubbleId:` rows** —
  drafts the user opened and abandoned. Always cross-check with the
  bubble keyspace before assuming a session has body.

## `composerData:<uuid>` — per-session metadata

A single fat JSON blob per composer. Top-level field census from a
real agent session (`_v: 16`):

| Field | Type | Meaning |
|---|---|---|
| `_v` | int | Schema version of this row (currently `16`) |
| `composerId` | UUIDv4 | Matches the `bubbleId:` and JSONL filename |
| `name` | string | Title (same as `composer.composerHeaders`) |
| `subtitle` | string | "Edited X, Y, Z" autosummary |
| `createdAt` / `lastUpdatedAt` | int | Unix milliseconds |
| `conversationCheckpointLastUpdatedAt` | int | Last time a `cursor-commits` checkpoint was written |
| `isAgentic` | bool | True when run in agent mode |
| `status` | string | `"completed"`, etc. |
| `unifiedMode` | string | `"agent"` \| `"chat"` |
| `forceMode` | string | `"edit"` \| `"ask"` … |
| `agentBackend` | string | `"cursor-agent"` (the Cursor in-house agent) |
| `modelConfig` | object | `{ modelName, maxMode, selectedModels: [{modelId, parameters}] }` |
| `fullConversationHeadersOnly` | array | Bubble-render manifest (see below) |
| `conversationMap` | object \| array | Older turn layout (often empty / `{}` in `_v ≥ 16`) |
| `capabilities` | array | `[{type: int, data: object}]` — feature gates active for this composer |
| `capabilityContexts` | array | Context attached per-capability |
| `todos` | array | Plan-mode todo list |
| `trackedGitRepos` | array | Repos under provenance tracking |
| `subComposerIds` / `subagentComposerIds` | array of UUID | Spawned sub-composers / sub-agents |
| `isBestOfNParent` / `isBestOfNSubcomposer` | bool | "Best of N" parallel-sampling support |
| `isProject` / `isSpec` / `isDraft` | bool | Composer kind discriminators |
| `blobEncryptionKey` | string (base64, 32 bytes) | AES key — present even when the blobs we observed were plaintext; reserved for future encrypted-at-rest content |
| `speculativeSummarizationEncryptionKey` | string (base64, 32 bytes) | Separate key for summarization payloads |
| `totalLinesAdded` / `totalLinesRemoved` / `filesChangedCount` | int | Aggregate diff stats |
| `usageData` | object | Per-model usage accounting |
| `contextUsagePercent` | float | How full the model context window is |
| `promptTokenBreakdown` | object | Token attribution per prompt section |
| `originalFileStates` | object | Map of `fsPath` → content-hash captured at first edit (used for undo) |
| `addedFiles` / `removedFiles` / `newlyCreatedFiles` / `newlyCreatedFolders` / `deletedFiles` | array | File-mutation tracking |
| `latestChatGenerationUUID` | UUIDv4 | Cross-ref to `aiService.generations` in the workspace `state.vscdb` |
| `richText` / `text` | string | Current draft in the input box |
| `queueItems` | array | Queued user messages |
| `pendingCreateWorktree` / `isCreatingWorktree` / `isUndoingWorktree` / `isApplyingWorktree` / `worktreeStartedReadOnly` | bool | Background-worktree lifecycle |
| `generatingBubbleIds` | array | Bubbles still streaming |
| `stopHookLoopCount` | int | Loop guard for `stop` hook |
| `applied` | bool / object | "Changes applied" state for the diff bundle |
| Many more empty arrays | array | UI state snapshot — see "Fidelity gotchas" |

### `fullConversationHeadersOnly`

Cursor's flat list of bubbles, used to render placeholders before
hydrating bodies:

```json
[
  { "bubbleId": "47e47ea6-…", "type": 1, "grouping": { "isRenderable": true, "hasText": true, "isShortPlainText": true }, "contentHeightHint": 42 },
  { "bubbleId": "0dd1e1e7-…", "type": 2, "grouping": { "isRenderable": true, "capabilityType": 30, "hasThinking": true, "thinkingDurationMs": 856 } },
  { "bubbleId": "dc0c46ad-…", "type": 2, "grouping": { "isRenderable": true, "hasText": true, "isKeptFinalAiVisibleOutsideWorkedForGroup": true }, "contentHeightHint": 42 }
]
```

| Field | Meaning |
|---|---|
| `bubbleId` | Joins to `bubbleId:<composer>:<bubbleId>` |
| `type` | `1` = user, `2` = assistant (same as in the bubble body) |
| `grouping.isRenderable` | UI hint; non-renderable headers exist for some internal turns |
| `grouping.capabilityType` | If the bubble is a tool-call / thinking-only bubble, this tags the capability up-front (15 = tool, 30 = thinking) |
| `grouping.hasText` / `hasThinking` / `thinkingDurationMs` | Render-time hints |
| `contentHeightHint` | Cached pixel height for the bubble box |

`fullConversationHeadersOnly` typically contains more entries than
there are `bubbleId:<composer>:*` rows, because some headers describe
turn fragments that get collapsed into the previous bubble's body
rather than stored on their own. **Iterate bubble rows, not headers,
when reconstructing a conversation.**

## `bubbleId:<composer>:<bubble>` — a single message

`_v: 3` at time of writing. Bubbles are big — a hundred or so fields,
most of them empty arrays representing per-bubble UI state slots —
but the load-bearing ones are:

| Field | Type | Meaning |
|---|---|---|
| `_v` | int | Schema version |
| `bubbleId` | UUIDv4 | Self |
| `type` | int | `1` = user, `2` = assistant |
| `createdAt` | ISO-8601 string | (Note: composer uses ms; bubbles use ISO. Don't unify the field names.) |
| `text` | string | Plaintext message body (often `""` on tool-call-only bubbles) |
| `richText` | string (escaped JSON) | Lexical editor document — the canonical source for user-typed input with formatting |
| `capabilityType` | int \| null | `15` = tool call/result, `30` = thinking, `null` = plain text |
| `conversationState` | string | Cursor-internal turn tag; `"~"` is the most common value observed |
| `unifiedMode` | int | Composer mode at the time of this bubble (`2` = agent, observed) |
| `isAgentic` | bool | Whether the bubble was produced under agent mode |
| `requestId` | string | Joins to a server-side request log (often `""`) |
| `checkpointId` | UUID | Joins to a `cursor-commits/checkpoints/<request-uuid>/` directory |
| `tokenCount` | object | `{ inputTokens, outputTokens }` — the per-bubble spend. **Reliability unverified**: community Cursor exporters read usage with fallbacks across `tokenCount`, a snake_case `usage` object, `contextWindowStatusAtCreation`, and `promptDryRunInfo`, which suggests `tokenCount` alone is not always sufficient — but we have too little real Cursor data to say how often it's populated. Confirm against live sessions before relying on it. |
| `modelInfo` | object | `{ modelName }` (only on bubbles that produced model output) |
| `toolFormerData` | object \| absent | **The tool call.** See below. |
| `toolResults` | array | Always empty in observed `_v: 3` rows — superseded by `toolFormerData.result` |
| `allThinkingBlocks` | array | Reasoning trace items for thinking bubbles |
| `codeBlocks` | array | Parsed code blocks the model emitted |
| `codeBlockData` | object | Per-code-block render state |
| `attachedCodeChunks` / `attachedFolders` / `attachedFoldersNew` / `attachedFoldersListDirResults` | array | Context attached by `@`-mention |
| `attachedFileCodeChunksMetadataOnly` | array | Slimmer file refs |
| `attachedHumanChanges` / `humanChanges` | bool / array | User-side edits taken into account |
| `gitDiffs` | array | Git diffs referenced from this turn |
| `diffsSinceLastApply` | array | Edit-batching state |
| `diffHistories` | array | Per-file diff history |
| `cursorRules` / `cursorCommands` | array | Active rule / command set |
| `pastChats` / `summarizedComposers` | array | Reference to other composers cited as context |
| `commits` / `pullRequests` | array | VCS objects referenced |
| `webReferences` / `aiWebSearchResults` / `externalLinks` / `docsReferences` | array | Network context |
| `interpreterResults` | array | REPL outputs |
| `consoleLogs` | array | Browser-side console captures |
| `images` | array | Pasted/uploaded images |
| `mcpDescriptors` | array | MCP-server tool descriptions in scope |
| `supportedTools` | array | Effective tool list for this turn |
| `notepads` / `knowledgeItems` / `projectLayouts` | array | Cursor "knowledge" features |

There are another two dozen empty-array fields that look like reserved
slots in Cursor's React state machine. A permissive deserializer
should accept them but is not obliged to interpret them.

### `toolFormerData` — the tool call

The single field that matters for diff/file fidelity:

```json
{
  "tool": 38,
  "toolIndex": 0,
  "modelCallId": "",
  "toolCallId": "tool_afc43977-dcf3-4517-9db3-9ff1fb6bb44",
  "status": "completed",
  "name": "edit_file_v2",
  "params":  "{\"relativeWorkspacePath\":\"/Users/ben/.../src/ast.rs\",\"noCodeblock\":true,\"cloudAgentEdit\":false}",
  "result":  "{\"beforeContentId\":\"composer.content.e3b0c44…b855\",\"afterContentId\":\"composer.content.1931772…1b4e\"}",
  "additionalData": {
    "precomputedDiff": { "lines": [ {"type": "added", "content": "use std::fmt;", "modifiedLineNumber": 1}, … ] }
  }
}
```

| Field | Notes |
|---|---|
| `tool` | Numeric tool id — same enum as `capabilityType` for tool bubbles, see [catalogue](#tool-catalogue) |
| `toolIndex` | Sequence within a multi-tool turn |
| `toolCallId` | `tool_<uuid>` — the only stable cross-bubble correlation key |
| `modelCallId` | Often `""`; only populated for some backends |
| `status` | `"completed"` \| `"error"` \| (`"running"` for streaming bubbles) |
| `name` | Internal tool name (`edit_file_v2`, `run_terminal_command_v2`, `read_file_v2`, `glob_file_search`) — distinct from the agent-side names in the JSONL |
| `params` | **JSON string** — re-parse on read |
| `result` | **JSON string** when present; `null` when `status == "error"` |
| `additionalData` | Per-tool extras (`startedAtMs`, `status`, `precomputedDiff`, …) |

For `edit_file_v2`, the `result` carries `beforeContentId` and
`afterContentId` — both `composer.content.<hash>` keys you can fetch
from `cursorDiskKV` to get the full pre/post file content. Lossless.

For `run_terminal_command_v2`, the structure is richer:

```json
{
  "tool": 15,
  "name": "run_terminal_command_v2",
  "params": {
    "command": "cd /Users/ben/projects/temp/cursortest && cargo build 2>&1",
    "cwd": "",
    "options": { "timeout": 30000 },
    "parsingResult": {
      "executableCommands": [
        { "name": "cd",    "args": [{"type":"word","value":"/Users/ben/projects/temp/cursortest"}], "fullText": "cd /Users/ben/projects/temp/cursortest" },
        { "name": "cargo", "args": [{"type":"word","value":"build"}], "fullText": "cargo build" }
      ],
      "hasRedirects": true,
      "allRedirectsAreDevNull": true,
      "redirects": [ { "operator": ">&", "destinationFds": [2], "targetNodeType": "number", "targetText": "1" } ]
    },
    "requestedSandboxPolicy": {
      "type": "TYPE_WORKSPACE_READWRITE",
      "networkAccess": false,
      "additionalReadwritePaths": ["/Users/ben/projects/temp/cursortest"],
      "enableSharedBuildCache": true
    },
    "commandDescription": "Build Rust project to find compile errors"
  },
  "result": {
    "output":    "   Compiling pyruntime v0.1.0 …\nerror[E0308]: mismatched types\n…",
    "exitCode":  101,
    "rejected":  false,
    "notInterrupted": true
  }
}
```

Pair tool-call bubbles to their downstream effect via `toolCallId` and
`checkpointId` (see below).

## Tool catalogue

Cursor's wire-level tool inventory is defined by the
`aiserver.v1.<Name>Params` protobuf messages embedded in the
workbench bundle (`Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js`);
71 distinct tools at time of writing. A second 47-entry list of
`*ToolCall` discriminators in the bundle's switch statements adds
UI-side aliases and control-flow markers.

### Observed numeric ids

The bubble's `toolFormerData.tool` field carries an application-
level integer that's *not* the protobuf field number. Anysphere
assigns these on the cloud side; the workbench bundle doesn't
expose the full enum literally. These are the ids we've directly
observed:

| Numeric `tool` | Internal `name` | JSONL friendly | Purpose |
|---|---|---|---|
| 0 | `update_current_step` | — | Planning-mode step advance |
| 15 | `run_terminal_command_v2` | `Shell` | Run a shell command in a sandbox |
| 38 | `edit_file_v2` | `Write` / `StrReplace` | Create/modify files (one call per edit; the JSONL collapses Write and StrReplace to two distinct shapes that both deserialize into this tool) |
| 40 | `read_file_v2` | `Read` | Read a file slice |
| 41 | `ripgrep_raw_search` | `Grep` | Content search via `ripgrep` |
| 42 | `glob_file_search` | `Glob` | Glob the workspace for files |
| 48 | `task_v2` | `Task` | Sub-agent dispatch (delegation) |

Other ids appear depending on which tools the session exercised;
a permissive parser must accept any `u32`. Switch on `name`
when the numeric id isn't in this table — toolpath-cursor's
`provider::tool_category(tool, name)` does exactly that.

### Full name inventory by IR category

Mapping every name observed in the bundle into toolpath's
[`ToolCategory`](https://docs.rs/toolpath-convo/latest/toolpath_convo/enum.ToolCategory.html)
ontology. Names not in the table classify to `None` — the
`ToolInvocation` still carries `name` + `input`, so consumers can
still render them; they just don't participate in
category-driven invariants.

| Category | Names |
|---|---|
| `Shell` | `run_terminal_command_v2`, `run_terminal_commands`, `run_terminal_cmd`, `run_test`, `write_shell_stdin`, `Shell`, `shell` |
| `FileWrite` | `edit_file_v2`, `edit_file`, `delete_file`, `new_edit`, `new_file`, `save_file`, `reapply`, `undo_edit`, `apply_agent_diff`, `create_rm_files`, `add_test`, `delete_test`, `fix_lints`, `fix_lints_subagent`, `Write`, `StrReplace`, `Edit`, `edit`, `delete` |
| `FileRead` | `read_file_v2`, `read_file`, `read_chunk`, `list_dir`, `list_dir_v2`, `read_project`, `get_project_structure`, `get_symbols`, `get_tests`, `gotodef`, `summarize_code`, `read_lints`, `read_with_linter`, `read_semsearch_files`, `blame_by_file_path`, `Read`, `read`, `ls` |
| `FileSearch` | `glob_file_search`, `ripgrep_raw_search`, `ripgrep_search`, `grep_search`, `search`, `search_symbols`, `semantic_search`, `semantic_search_full`, `sem_search`, `deep_search`, `deep_search_subagent`, `tool_call_file_search`, `Glob`, `Grep`, `glob`, `grep` |
| `Network` | `web_search`, `web_fetch`, `fetch_pull_request`, `fetch`, `call_mcp_tool` |
| `Delegation` | `task_v2`, `task`, `task_subagent`, `spec_subagent`, `background_composer_followup`, `start_grind_execution`, `start_grind_planning`, `Task` |
| `None` (UI / control / planning / reporting / MCP control plane / VCS write) | `update_current_step`, `create_plan`, `todo_read`, `todo_write`, `read_todos`, `update_todos`, `get_mcp_tools`, `list_mcp_resources`, `read_mcp_resource`, `mcp`, `mcp_auth`, `ask_question`, `communicate_update`, `send_final_summary`, `switch_mode`, `set_run`, `await`, `await_task`, `end`, `partial`, `truncated`, `reflect`, `add_ui_step`, `report_bug`, `report_bugfix_results`, `record_ci_investigation_findings`, `ai_attribution`, `set_active_branch`, `edit_pr_labels`, `update_pr_code_tour`, `pr_management`, `knowledge_base`, `fetch_rules`, `update_project`, `replace_env`, `setup_vm_environment`, `generate_image`, `computer_use`, `record_screen`, `create_diagram` |

`capabilityType: 30` is reserved for "thinking" bubbles — these have
no `toolFormerData` and instead populate `allThinkingBlocks`. Cursor
has many more capability types in the wire format (the `capabilities`
array on a composer enumerates them — observed numbers include 15, 16,
19, 21, 23, 24, 32, 33); a robust parser should treat the integer
opaquely and switch on `name` when present.

### Refreshing the inventory

To re-derive the protobuf-level tool list after a Cursor.app upgrade:

```bash
grep -oE 'aiserver\.v1\.[A-Za-z0-9]+Params' \
  '/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js' \
  | sort -u
```

And the UI-side switch discriminators:

```bash
grep -oE 'case"[A-Za-z]+ToolCall"' \
  '/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js' \
  | sort -u
```

Diff the output against `provider::tool_category` in
`crates/toolpath-cursor/src/provider.rs` to find new tools to
classify.

## Content-addressed file blobs

Tool results refer to file contents by short identifier:

```json
"result": "{\"beforeContentId\":\"composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\",\"afterContentId\":\"composer.content.06b602c609ad69695e118d96fe639083f07a71c879f856c8741b137924e6ac3e\"}"
```

The hash after `composer.content.` is a 64-hex-char content
identifier. To fetch the bytes:

```sql
SELECT value FROM cursorDiskKV WHERE key = 'composer.content.<hash>';
```

Observed behavior:

- The all-zeros hash for new files (`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`)
  **is** the SHA-256 of the empty string. The corresponding row has a
  zero-byte value. Cursor uses it as the canonical "no prior content"
  sentinel.
- For non-empty values, the hash does **not** match a plain
  `sha256(value)`. Cursor evidently hashes a normalized form (line
  endings, BOM, length-prefix, or something equivalent — the
  normalization isn't published and reverse-engineering it isn't
  necessary, since the hash is *only* used as a lookup key). Treat as
  opaque.
- Every `composer.content.<hash>` we read back was UTF-8 plaintext.
  `blobEncryptionKey` is present on the composer but unused on these
  blobs in the wild.

A parallel keyspace `agentKv:blob:<hash>` mirrors the same data with
the same hash — likely the `cursor-agent` backend's own copy. Treat
the two as a single content store with two index prefixes.

## `inlineDiff:<workspace-id>:<diff-id>` — apply/reject records

One per edit the agent applied to the working tree. Plaintext JSON:

```json
{
  "diffId": "8fca5937-4044-4bee-9658-2a94638d7887",
  "uri": {
    "$mid": 1,
    "external": "file:///Users/ben/projects/temp/cursortest/Cargo.toml",
    "path":     "/Users/ben/projects/temp/cursortest/Cargo.toml",
    "scheme":   "file"
  },
  "originalTextLines": [],
  "generationUUID": "6bc193ea-a00c-4ad4-9fcb-a39d600a8b87",
  "composerMetadata": { … }
}
```

`generationUUID` joins to a row in the workspace `state.vscdb`'s
`aiService.generations` list — the user-visible "generation" the
edit belongs to.

## `ofsContent:<composer>:<file-uri>` — presence markers

One row per file the agent referenced under a composer, keyed by the
composer UUID + the *full* `file://` URI. The values are typically
zero bytes; they serve as a "this file was in scope" set rather than a
content cache. Don't try to read them as content.

## `cursor-commits/checkpoints/<request-uuid>/` — file snapshots

Cursor runs an internal "commits" feature (Anysphere extension
`anysphere.cursor-commits`) that snapshots the workspace at every
agent request so the user can undo. Layout:

```
checkpoints/
  <request-uuid>/
    metadata.json
    diffs/<file-uuid>     One file per touched file, plain JSON
    files/<file-uuid>     Same UUIDs as diffs/; often 0 bytes
```

### `metadata.json`

```json
{
  "agentRequestId": "185252c2-21c9-4834-b1e3-1c38c4207865",
  "requestFiles": [
    { "fsPath": "/Users/ben/projects/temp/cursortest/Cargo.toml",
      "fileUuid": "80a79b69-7444-493a-918f-a814b9e7ba0f",
      "gitInfo": { "noRepoFound": true } },
    …
  ],
  "startTrackingDateUnixMilliseconds": 1780325958677,
  "fileSizeBytes": 88466,
  "workspaceId": "93c175a1e1761d404ef54f2a5f463464"
}
```

`agentRequestId` cross-references to the workspace `state.vscdb`'s
`aiService.generations[].generationUUID` — i.e. the "Generation"
that produced this checkpoint, which in turn ties back to the
composer's `latestChatGenerationUUID`.

### `diffs/<file-uuid>`

Per-file structured diff. Adds carry the full new content line-by-
line; modifies carry old + new ranges; deletes carry a marker.

```json
{
  "fsPath": "/Users/ben/projects/temp/cursortest/Cargo.toml",
  "fileUuid": "80a79b69-7444-493a-918f-a814b9e7ba0f",
  "fileSizeBytes": 280,
  "numLines": 17,
  "diffChanges": [
    {
      "originalStartLineNumberOneIndexed": 1,
      "originalEndLineNumberExclusiveOneIndexed": 2,
      "modifiedStartLineNumberOneIndexed": 1,
      "modifiedEndLineNumberExclusiveOneIndexed": 18,
      "addedLines": ["[package]","name = \"pyruntime\"", … ],
      "tokenizedAddedLines": [1000001, 1000002, …, 1, 1000007, …, 1]
    }
  ],
  "gitInfo": { "noRepoFound": true },
  "kind": "KIND_MODIFIED"
}
```

`kind` values: `KIND_MODIFIED` (also used for adds with empty
original range), `KIND_DELETED` (observed in upstream behavior; not
in this fixture). `tokenizedAddedLines` is a parallel array of
opaque small integers that Cursor uses to compute AI-authorship
percentages — not needed for diff reconstruction.

`files/<file-uuid>` is normally zero bytes — the content lives in
`cursorDiskKV` under `composer.content.<hash>` (referenced via the
`bubbleId.toolFormerData.result.afterContentId`) rather than being
duplicated here.

## `ai-tracking/ai-code-tracking.db` — authorship stats

Cursor's per-line AI-vs-human authorship tracker, used to attribute
each commit. Schema:

```sql
CREATE TABLE ai_code_hashes (
  hash             TEXT PRIMARY KEY,    -- per-line hash
  source           TEXT NOT NULL,       -- "tab" | "composer" | "human" | …
  fileExtension    TEXT,
  fileName         TEXT,
  requestId        TEXT,
  conversationId   TEXT,
  timestamp        INTEGER,
  model            TEXT,
  createdAt        INTEGER NOT NULL
);

CREATE TABLE scored_commits (
  commitHash         TEXT NOT NULL,
  branchName         TEXT NOT NULL,
  scoredAt           INTEGER NOT NULL,
  linesAdded         INTEGER,  linesDeleted          INTEGER,
  tabLinesAdded      INTEGER,  tabLinesDeleted       INTEGER,
  composerLinesAdded INTEGER,  composerLinesDeleted  INTEGER,
  humanLinesAdded    INTEGER,  humanLinesDeleted     INTEGER,
  blankLinesAdded    INTEGER,  blankLinesDeleted     INTEGER,
  commitMessage      TEXT,     commitDate            TEXT,
  v1AiPercentage     TEXT,     v2AiPercentage        TEXT,
  PRIMARY KEY (commitHash, branchName)
);

CREATE TABLE tracking_state (
  key TEXT PRIMARY KEY, value TEXT NOT NULL
);

CREATE TABLE conversation_summaries (
  conversationId TEXT PRIMARY KEY,
  title          TEXT,  tldr   TEXT,  overview TEXT,
  summaryBullets TEXT,  model  TEXT,  mode     TEXT,
  updatedAt      INTEGER NOT NULL
);

CREATE TABLE tracked_file_content (
  gitPath        TEXT PRIMARY KEY,
  content        TEXT NOT NULL,
  conversationId TEXT,  model TEXT,  fileExtension TEXT,
  createdAt      INTEGER NOT NULL
);

CREATE TABLE ai_deleted_files (
  gitPath       TEXT NOT NULL,
  composerId    TEXT,  conversationId TEXT, model TEXT,
  deletedAt     INTEGER NOT NULL,
  PRIMARY KEY (gitPath, deletedAt)
);
```

`conversation_summaries.conversationId` joins to the composer UUID.
`scored_commits` is the input to the dashboard / per-PR "AI %"
badge. Useful as a cross-reference, but every fact it carries is also
derivable from the bubble store + git itself, so a reader can ignore
it for raw conversation reconstruction.

## Workspace `state.vscdb` — light per-workspace state

Each `workspaceStorage/<id>/state.vscdb` has the same two-table
schema as the global one (`ItemTable` + `cursorDiskKV`). Most rows
are VS Code's own; the Cursor-specific ones we care about:

| Key | Table | Shape |
|---|---|---|
| `composer.composerData` | `ItemTable` | `{ selectedComposerIds: [<uuid>], lastFocusedComposerIds, hasMigratedComposerData, hasMigratedMultipleComposers }` — which composer the sidebar is showing |
| `aiService.generations` | `ItemTable` | `[{ unixMs, generationUUID, type: "composer", textDescription }]` — the per-request log |
| `aiService.prompts` | `ItemTable` | `[{ text, commandType }]` — recent prompts |
| `anysphere.cursor-retrieval` | `ItemTable` | Workspace-scoped retrieval config |

`cursorDiskKV` in the workspace DB is empty in every workspace we
sampled — the global one is the home of all bubble data.

## Round-trip fidelity

Pitfalls for anyone parsing and re-emitting the format.

1. **The JSONL transcript is not lossless.** It contains no tool
   results, no thinking, no timestamps, no model name, no token
   counts, and an explicit `[REDACTED]` sentinel for assistant text.
   For provenance, use the SQLite bubble store as the source of truth.
2. **`toolFormerData.params` and `toolFormerData.result` are JSON
   strings**, not JSON objects — re-parse on read; preserve verbatim
   when round-tripping (`String`, not `Value`).
3. **`toolFormerData.result` may be `null`** when `status` is
   `"error"`. Don't assume non-null.
4. **`toolFormerData.tool` and `capabilityType` are integers** of an
   undocumented enum that grows with Cursor releases. Use a permissive
   `enum + Other(int)` representation; switch on `name` when present.
5. **Timestamps disagree on type.** `composerData.createdAt` is Unix
   ms (integer); `bubbleId.createdAt` is an ISO-8601 string. Normalize
   on read.
6. **`fullConversationHeadersOnly` lists more bubbles than exist as
   `bubbleId:` rows.** Iterate the rows when reconstructing the
   conversation; consult headers only for render hints.
7. **Composer headers in `composer.composerHeaders` can reference
   composers with no `bubbleId:` rows** — drafts the user abandoned.
8. **Content-blob hashes are opaque.** The empty-content hash is
   SHA-256 of `""`; non-empty hashes are some normalized variant.
   Don't try to verify by recomputing — just use the hash as a lookup
   key.
9. **`composer.content.<hash>` and `agentKv:blob:<hash>` are the same
   content under two prefixes.** Either is sufficient; the latter is
   the `cursor-agent` backend's own copy.
10. **`blobEncryptionKey` and `speculativeSummarizationEncryptionKey`
    are present on every composer** but unused on the blobs we
    observed. Some fields (e.g. `inlineDiff:*` in newer Cursor
    versions) may eventually be encrypted with them — treat
    base64-looking values as potential ciphertext if a field name
    suggests so.
11. **Every bubble has ~70 fields, most of them empty arrays.**
    Don't strip empty arrays on write — Cursor's reader is unlikely
    to tolerate missing keys cleanly. Round-trip via a typed struct
    with `#[serde(default)]` everywhere plus a `#[serde(flatten)]
    extra` catch-all.
12. **`unifiedMode` is a string on the composer (`"agent"`) and an
    integer on a bubble (`2`)**. They're the same enum but spelled
    differently across rows.
13. **Workspace ids are opaque.** Don't try to recompute them from
    the folder URI — the normalization is undocumented. Read the
    `id` from `composer.composerHeaders[*].workspaceIdentifier.id`
    or from `workspaceStorage/<id>/workspace.json`'s `folder` field.
14. **The JSONL filename equals the composer UUID** equals the
    `composerData:<uuid>` row's `composerId`. Use that as the join
    key across stores.
15. **Cursor writes both stores asynchronously.** A read during an
    active session can see a partial bubble or a composer header
    pointing at a bubble id that doesn't yet exist in `cursorDiskKV`.
    Skip-on-missing is the safe reader policy.

## How `toolpath-cursor` would map this

The provider should treat the SQLite bubble store as the primary
source. Suggested mapping into `ConversationView` + `toolpath::v1::Path`:

| Cursor construct | Where it lands |
|---|---|
| `composerData.composerId` | `ConversationView.id`, `path.id = path-cursor-<first-8>` |
| `composerData.name` / `subtitle` | `path.meta.title` (with `subtitle` falling back when `name` is missing) |
| `composerData.createdAt` | `Turn.timestamp` on the first user turn, `path.meta.created_at` |
| `composerData.modelConfig.modelName` | Default `Turn.model`; per-bubble `modelInfo.modelName` wins |
| `composerData.agentBackend` (`"cursor-agent"`) | `path.meta.source = "cursor"` + `path.meta.extra["cursor"]["backend"]` |
| `workspaceIdentifier.uri.fsPath` | `Turn.environment.working_dir`, `path.base.uri` |
| `bubbleId` with `type: 1` | `Turn { role: User }` → Step with `actor: "human:user"` |
| `bubbleId` with `type: 2`, no `toolFormerData`, `capabilityType: null` | `Turn { role: Assistant, model }` → Step with `actor: "agent:<model>"` |
| `bubbleId` with `capabilityType: 30`, `allThinkingBlocks: [...]` | `Turn.thinking` on the next assistant turn (consistent with other providers) |
| `bubbleId` with `toolFormerData` | `Turn.tool_uses[]` with `tool_call_id = toolFormerData.toolCallId`, `name = toolFormerData.name`, `input = parse(params)`, `result = parse(result)`, `status` mirrored |
| `toolFormerData.result.{beforeContentId, afterContentId}` (edits) | `ArtifactChange` on the tool-call's turn, with `raw` perspective synthesized from `additionalData.precomputedDiff.lines` and the blob bodies looked up via `composer.content.<hash>` |
| `toolFormerData.result.{output, exitCode}` (shell) | `Turn.tool_uses[].result` exit/output; `path.meta.extra["cursor"]["sandbox_policy"]` carries the per-command sandbox if needed |
| `cursor-commits/checkpoints/<req>/diffs/*` | Sibling `ArtifactChange`s when a tool result didn't carry inline content (fallback) |
| `bubbleId.tokenCount` | `Turn.token_usage` (last-write-wins per turn) |
| Unknown `tool` integers / unknown bubble fields | `Turn.extra["cursor"]` (typed catch-all) |

### Fidelity guarantees a derive should aim for

- **File changes** are lossless when the bubble carries either an
  inline `precomputedDiff.lines` array (which is granular enough to
  re-emit a unified diff) or `beforeContentId`/`afterContentId`
  (where both blobs exist in the content store).
- **Tool I/O** round-trips via `toolFormerData.toolCallId` as the
  call-id; both sides of the pair live in the same bubble (Cursor
  does not split tool call and tool result into separate bubbles).
- **Wire-level round-trip** is feasible if the deserializer keeps
  every empty array, every unknown key, and stores `params`/`result`
  as raw `String`.
- **JSONL ↔ bubble correspondence**: the JSONL's nth `tool_use`
  call corresponds to the nth tool bubble in document order — but
  with Cursor's friendly tool names mapping to the bubble's internal
  ones. Use that as a sanity check, not a join key.

## Projecting bubbles Cursor will render

The reader is generous; the renderer is not. A `composerData`/`bubbleId:*`
row that round-trips through `toolpath-cursor`'s deserializer cleanly
will silently fail to render in Cursor.app's chat unless every field
below is present. Determined empirically by diffing a native edit
bubble against ours when neither code path threw but only one
rendered.

### `composerData.<extra>` (required by composer load)

Missing any one of these makes the composer hang on "Loading chat"
or fail to register the agent.

| Field | Value | Why |
|---|---|---|
| `selectedModels[0].parameters` | `[]` (must serialize, not omit) | `Pgs` factory calls `.map()` on it; `undefined.map` throws |
| `capabilities` | 8 entries: `{type: 15, data: {bubbleDataMap: "{}"}}` then `{type: N, data: {}}` for N ∈ {19, 33, 32, 23, 16, 24, 21} | Cursor's DI binds capability handlers; missing → `$di$dependencies` undefined |
| `context` | `@-mention` skeleton (`fileSelections: []`, `folderSelections: []`, …, `mentions: {...}`) | Input-chip renderer dereferences `context.fileSelections` etc. |
| `conversationMap`, `codeBlockData`, `originalFileStates`, `usageData` | `{}` each | `Pbs.loadFromStorage` calls `Object.entries(...)` on each; throws on `undefined` |

### `bubbleId:*` (required by edit-bubble rendering)

For `capabilityType: 15` (tool) bubbles. Missing fields silently break
the edit diff renderer even when the composer loads.

| Field | Value | Notes |
|---|---|---|
| `result` | `{beforeContentId, afterContentId}` — both, pointing at present blobs | Renderer calls `cursorDiskKVGet(beforeContentId)` and `cursorDiskKVGet(afterContentId)` and diffs the resolved text |
| `toolFormerData.params` | proto3 JSON of `editFileV2Params` — `relativeWorkspacePath`, `noCodeblock: true`, `cloudAgentEdit: false` | All three present; defaults aren't inferred |
| `toolFormerData.additionalData` | `{}` (empty object, not absent) | `precomputedDiff` here is only read at live-edit time via the runtime `editToolCallDisplay` map; at-rest bubbles don't need it |
| `toolFormerData.modelCallId` | `""` | Always emitted on native, parser destructures unconditionally |
| `isAgentic` | `false` on tool bubbles | Native uses `true` only for assistant text bubbles |
| `requestId` | `""` | Native always emits |
| `allThinkingBlocks`, `toolResults` | `[]` (must serialize) | `Object.entries`-style indexers in the bubble renderer |
| 48 empty arrays + 6 booleans | exhaustively listed below | All `[]` / `false`; absent → undefined deref |
| `context`, `modelInfo` | **omit on tool bubbles** | Native tool bubbles don't carry them; harmless on text/user bubbles |

The 48 empty arrays: `aiWebSearchResults`, `approximateLintErrors`,
`assistantSuggestedDiffs`, `attachedCodeChunks`,
`attachedFileCodeChunksMetadataOnly`, `attachedFolders`,
`attachedFoldersListDirResults`, `attachedFoldersNew`,
`capabilities`, `capabilityContexts`, `codeBlocks`,
`codebaseContextChunks`, `commits`, `consoleLogs`, `contextPieces`,
`cursorCommands`, `cursorRules`, `deletedFiles`, `diffHistories`,
`diffsForCompressingFiles`, `diffsSinceLastApply`, `docsReferences`,
`documentationSelections`, `editTrailContexts`, `externalLinks`,
`fileDiffTrajectories`, `gitDiffs`, `humanChanges`, `images`,
`interpreterResults`, `knowledgeItems`, `lints`, `mcpDescriptors`,
`multiFileLinterErrors`, `notepads`, `pastChats`, `projectLayouts`,
`pullRequests`, `recentLocationsHistory`, `recentlyViewedFiles`,
`relevantFiles`, `suggestedCodeBlocks`, `summarizedComposers`,
`supportedTools`, `uiElementPicked`,
`userResponsesToSuggestedCodeBlocks`, `webReferences`,
`workspaceUris`.

The 6 booleans (all `false`): `attachedHumanChanges`,
`cursorCommandsExplicitlySet`, `existedPreviousTerminalCommand`,
`existedSubsequentTerminalCommand`, `isRefunded`,
`pastChatsExplicitlySet`. Plus `todos: []`.

### Source-of-truth for "did this bubble render"

The error surfaces in the **Electron dev console**, not in
`renderer.log`. Cursor's bubble loader wraps everything in
`try { … } catch { Xk(err) }` and the catch handler routes to
`console.error` (DevTools only). When debugging, open `Help → Toggle
Developer Tools → Console` and look for `[composer] Error loading
composer data` or `[composer] Error parsing toolFormerData`.

### `toolCallBinary` and `toolCallId` format

Native bubbles carry a base64-encoded `aiserver.v1.ToolCall` protobuf
in `toolFormerData.toolCallBinary` and use `tool_<uuid>`-shaped
`toolCallId`s. Neither is required for at-rest diff rendering — the
reader guards the binary decode with `if (n.toolCallBinary)` and the
`toolCallId` is treated as an opaque string. Cursor-agent-CLI
sessions that hand off live to the IDE will reject foreign id formats,
but that's not a path `toolpath-cursor` exports through.

## References

- Cursor app: <https://cursor.com>
- VS Code workspace storage layout (the host shell):
  <https://code.visualstudio.com/api/extension-capabilities/common-capabilities#storage>
- The `anysphere.cursor-commits` extension surface is closed source;
  the on-disk layout above is from direct inspection.
- The `cursor-agent` backend (`agentBackend: "cursor-agent"`) is
  Cursor's in-house coding agent runtime; behavior here was confirmed
  on Cursor `3.6.-main` recorded 2026-06-01. Other backends
  (`composer-claude`, etc., historically) may store data differently
  and need re-sampling when encountered.
