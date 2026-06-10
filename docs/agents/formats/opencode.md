# opencode session format

Reference for the on-disk format produced by
[opencode](https://opencode.ai) (repo: [`anomalyco/opencode`](https://github.com/anomalyco/opencode)),
as intended input for a future `toolpath-opencode` provider. Compiled
from:

1. Direct inspection of a real session on this machine — opencode
   `1.3.10`, session recorded 2026-04-21 building a 3D "pickle"
   OpenGL demo under `local/the-pickle/`.
2. The TypeScript definitions in `anomalyco/opencode` at
   `packages/opencode/src/session/{message-v2,session.sql,schema}.ts`
   (the drizzle table definitions and the Zod Part/Message schemas).
3. The snapshot subsystem in `packages/opencode/src/snapshot/index.ts`.

Unlike Codex or Gemini, opencode **does not use JSONL**. Conversations
live in a SQLite database (`opencode.db`). Every session, message,
and part is a row; message/part payloads are JSON text columns.
Per-step filesystem snapshots live in a sibling bare git repository
addressed by content hash.

Date: **2026-04-21**. Revisit when the schema migrations advance past
`0010` (tracked in `__drizzle_migrations`), the opencode minor
version bumps, or the `event` / `event_sequence` tables start getting
populated.

## Storage root

opencode follows XDG base-dir conventions. On macOS and Linux:

```
$XDG_DATA_HOME/opencode/           (defaults to ~/.local/share/opencode/)
  auth.json                         OAuth + API-key credentials per provider (sensitive)
  opencode.db                       Primary SQLite DB — sessions, messages, parts, projects
  opencode.db-shm                   SQLite WAL shared-memory sidecar
  opencode.db-wal                   SQLite WAL journal
  log/
    YYYY-MM-DDThhmmss.log           Per-run process logs (rotating)
    dev.log                         Long-running dev-mode log
  snapshot/
    <project-id>/                   Project-scoped snapshot directory
      <sha1(worktree)>/             Bare git repo, one per workspace root
        HEAD, config, index, objects/, refs/, packed-refs, …
  bin/                              Cached tool binaries
```

```
$XDG_CONFIG_HOME/opencode/          (defaults to ~/.config/opencode/)
  package.json                      Pins `@opencode-ai/plugin` — user plugin workspace
  bun.lock
  node_modules/                     Bun-managed plugin deps
```

Path resolution lives in
[`packages/opencode/src/global/index.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/global/index.ts).
Override the data directory with the `OPENCODE_TEST_HOME` env var.
The `$XDG_CACHE_HOME/opencode/` (`~/.cache/opencode/`) and
`$XDG_STATE_HOME/opencode/` (`~/.local/state/opencode/`) directories
exist too, but hold transient caches and flock state — not
conversation content.

## `auth.json`

OAuth tokens and API keys, keyed by provider id:

```json
{
  "anthropic": {
    "type": "oauth",
    "refresh": "sk-ant-ort01-…",
    "access":  "sk-ant-oat01-…",
    "expires": 1776792000000
  },
  "openai":   { "type": "api", "key": "sk-…" },
  "opencode": { "type": "oauth", … }
}
```

A reader should **never** ingest this file into a derived Toolpath
document — the tokens are live credentials.

## `opencode.db` — SQLite layout

WAL-mode SQLite managed by [drizzle-orm](https://orm.drizzle.team/).
Migration history lives in `__drizzle_migrations` (SHA-tagged); at
the time of writing the DB is at migration `10`.

Tables:

| Table | Purpose |
|---|---|
| `project` | One row per git repo opencode has opened |
| `workspace` | Named workspaces / worktrees within a project |
| `session` | One conversation (one opencode launch or resume) |
| `message` | A user or assistant message within a session |
| `part` | A typed fragment of a message (text, tool, reasoning, …) |
| `todo` | Assistant-maintained TODO list per session |
| `permission` | Per-project permission ruleset |
| `session_share` | Share-URL credentials per session |
| `account`, `account_state`, `control_account` | OAuth accounts for the optional opencode control-plane service |
| `event`, `event_sequence` | Reserved for sync; **observed empty** on this machine |
| `__drizzle_migrations` | Schema migration tracking |

The `event` / `event_sequence` tables exist because sessions, messages,
and parts emit typed `SyncEvent`s (see
`message-v2.ts`: `message.updated`, `message.removed`,
`message.part.updated`), but persistence of those events appears
gated on the remote-sync layer being active. A reader should not rely
on them being populated.

### `project`

```sql
CREATE TABLE project (
  id                text PRIMARY KEY,   -- SHA-1 hex of the repo's oldest root commit
  worktree          text NOT NULL,      -- Absolute path to the worktree
  vcs               text,               -- "git" | null
  name              text,
  icon_url          text,
  icon_color        text,
  time_created      integer NOT NULL,   -- Unix millis
  time_updated      integer NOT NULL,
  time_initialized  integer,
  sandboxes         text NOT NULL,      -- JSON string[] — sandbox paths
  commands          text                -- JSON { start?: string }
);
```

`project.id` is derived **not** from the worktree path but from
`git rev-list --max-parents=0 HEAD` — the SHA of the first root
commit of the repo (sorted lexicographically if the repo has
multiple roots from unrelated merges). opencode caches the id in
`<worktree>/.git/opencode` so re-opening the same repo is fast.
For non-git directories, a sentinel `ProjectID.global` is used.

This means a project survives being renamed or moved on disk, and a
fresh clone of the same repo gets the same project id — so snapshots
and sessions from before the rename/move remain discoverable. See
[`project.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/project/project.ts).

### `workspace`

```sql
CREATE TABLE workspace (
  id          text PRIMARY KEY,         -- "wrk_" + sortable id
  branch      text,
  project_id  text NOT NULL,
  type        text NOT NULL,            -- workspace variant
  name        text,
  directory   text,
  extra       text                      -- JSON catch-all
);
```

A single project can host multiple workspaces — e.g. one per git
branch or worktree. Sessions link back via `session.workspace_id`.

### `session`

```sql
CREATE TABLE session (
  id                  text PRIMARY KEY,    -- "ses_" + sortable id
  project_id          text NOT NULL,
  workspace_id        text,
  parent_id           text,                -- For forked / child sessions
  slug                text NOT NULL,       -- Human-readable slug, e.g. "crisp-nebula"
  directory           text NOT NULL,       -- cwd at session creation
  title               text NOT NULL,       -- Auto-summarized or user-set
  version             text NOT NULL,       -- opencode version, e.g. "1.3.10"
  share_url           text,                -- Set when the session is shared
  summary_additions   integer,             -- Aggregate file-change stats
  summary_deletions   integer,
  summary_files       integer,
  summary_diffs       text,                -- JSON SnapshotFileDiff[]
  revert              text,                -- JSON { messageID, partID?, snapshot?, diff? }
  permission          text,                -- JSON Permission.Ruleset override
  time_created        integer NOT NULL,
  time_updated        integer NOT NULL,
  time_compacting     integer,
  time_archived       integer
);
```

`summary_diffs` is the aggregate of all file changes across the
session, rebuilt from the last snapshot (see `summary.ts`). It can
stay null on cold sessions or sessions with zero tool activity.

`revert` records a point the session has been "rewound" to — set
when the user reverts a step via the UI; the writer still appends
new messages past the revert boundary, but the UI hides them.

### `message`

```sql
CREATE TABLE message (
  id            text PRIMARY KEY,   -- "msg_" + sortable id
  session_id    text NOT NULL,
  time_created  integer NOT NULL,
  time_updated  integer NOT NULL,
  data          text NOT NULL        -- JSON discriminated on `role`
);
CREATE INDEX message_session_time_created_id_idx
  ON message (session_id, time_created, id);
```

Message ordering within a session is `(time_created ASC, id ASC)` —
relying on `id` alone is OK because the ascending id generator
embeds a millisecond timestamp in its first 12 hex chars plus a
per-process counter.

Payload shape (from `message-v2.ts`):

```ts
// Common to every message (id, sessionID live on the row)
type Base = {}

// role: "user"
{
  role: "user"
  time: { created: number }              // Unix millis
  format?: OutputFormat                   // text | json_schema
  summary?: {                             // Populated for compaction / fork forks
    title?: string
    body?: string
    diffs: SnapshotFileDiff[]
  }
  agent: string                           // "build" default; user agent choice
  model: {
    providerID: string                    // "anthropic", "openai", "opencode", …
    modelID: string                       // "claude-sonnet-4-6", "big-pickle", …
    variant?: string
  }
  system?: string                         // Override system prompt
  tools?: Record<string, boolean>         // Enabled / disabled tools
}

// role: "assistant"
{
  role: "assistant"
  parentID: MessageID                      // Chain to the prior message
  time: {
    created:   number
    completed?: number                     // Null while still generating
  }
  error?:    NamedError                    // Auth, output-length, aborted, …
  agent:     string                        // Same semantics as user.agent
  mode:      string                        // DEPRECATED — kept for old rows
  modelID:   string
  providerID: string
  path:  { cwd: string, root: string }     // Where the agent was running
  summary?:  boolean                       // true if this message IS a summary
  cost:      number                        // USD
  tokens: {
    total?: number
    input:  number
    output: number
    reasoning: number
    cache: { read: number, write: number }
  }
  structured?: any                         // Set when format is json_schema
  variant?:  string
  finish?:   string                        // "stop" | "tool-calls" | "length" | …
}
```

The pickle fixture has 57 messages (8 user, 49 assistant), all with
`agent: "build"` (opencode's default build agent). The first user
message has `model.modelID: "big-pickle"` — a user-defined alias —
so be prepared for arbitrary strings there.

### `part`

```sql
CREATE TABLE part (
  id            text PRIMARY KEY,   -- "prt_" + sortable id
  message_id    text NOT NULL,
  session_id    text NOT NULL,
  time_created  integer NOT NULL,
  time_updated  integer NOT NULL,
  data          text NOT NULL        -- JSON discriminated on `type`
);
CREATE INDEX part_message_id_id_idx ON part (message_id, id);
CREATE INDEX part_session_idx       ON part (session_id);
```

Part ordering within a message is `(time_created ASC, id ASC)`. A
single assistant message unfolds into a stream of parts in the
order: `step-start → [text | reasoning | tool]+ → step-finish`,
possibly repeating across multiple reasoning/tool cycles.

The `data` blob is a tagged union. Discriminator is the `type`
field. Variants, with the fields stored in `data` — note that
`id`/`sessionID`/`messageID` (from the upstream `PartBase`) are
stripped at the SQL layer because they're redundant with the row's
columns.

## Part variants

12 variants are defined upstream in `message-v2.ts`. Observed in the
pickle fixture (205 parts): `text` (17), `reasoning` (49), `tool`
(41), `step-start` (49), `step-finish` (49). The remaining seven
appear in other configurations; their schemas are still reachable
via `anomalyco/opencode` so a round-trip-preserving reader should
type them as best-effort.

### `text`

```json
{ "type": "text", "text": "hello" }
```

Full upstream shape:

```ts
{
  type: "text"
  text: string
  synthetic?: boolean      // True for tool-output attachments etc.
  ignored?:   boolean      // Hidden from the model's input
  time?:  { start: number, end?: number }
  metadata?: Record<string, any>
}
```

### `reasoning`

```json
{
  "type": "reasoning",
  "text": "The user is just saying hello. This is a simple greeting…",
  "time": { "start": 1776792840743, "end": 1776792840743 },
  "metadata": {
    "anthropic": {
      "signature": "7d1d26c725ae11d59e65b94997787155f720131d550eeceb645dab745642fc3f"
    }
  }
}
```

Chain-of-thought. Text is **plaintext** (not encrypted like Codex's
`reasoning.encrypted_content`); `metadata` often carries
provider-specific signatures used to resume reasoning across turns
(Anthropic's `signature`, OpenAI's encrypted blob under `metadata.openai`,
etc.).

### `tool`

```json
{
  "type": "tool",
  "tool": "bash",
  "callID": "call_function_shfxzwrk1e88_1",
  "state": {
    "status": "completed",
    "input":  { "command": "ls -la local", "description": "Check if local exists" },
    "output": "total 0\ndrwxr-xr-x  4 ben staff 128 Apr 20 12:45 .\n…",
    "metadata": { "output": "…", "exit": 0, "description": "…", "truncated": false },
    "title": "Check if local directory exists",
    "time":  { "start": 1776792917419, "end": 1776792917458 }
  }
}
```

`state` is itself a tagged union on `status`:

| status | Fields |
|---|---|
| `pending`   | `input`, `raw` (verbatim tool-call string from the model) |
| `running`   | `input`, `title?`, `metadata?`, `time: { start }` |
| `completed` | `input`, `output`, `title`, `metadata`, `time: { start, end, compacted? }`, `attachments?: FilePart[]` |
| `error`     | `input`, `error`, `metadata?`, `time: { start, end }` |

A completed state's `metadata` is **tool-specific**: `bash` carries
`exit`, `description`, `truncated`; `edit` carries LSP diagnostics;
`write` echoes the written content length; `read` echoes the file
MIME and path. Treat it as `Record<string, any>` and preserve
verbatim.

`callID` is the provider's tool-call identifier and matches what
OpenAI / Anthropic return in their tool-call output correlation
fields. opencode uses it for its own pairing as well.

Observed tool names in the pickle fixture: `bash` (21), `edit` (14),
`write` (3), `read` (3). Upstream supports many more — anything the
configured agent's permissions allow: MCP tools, custom subagents,
LSP operations, etc.

### `step-start`

```json
{ "type": "step-start", "snapshot": "f12e27e948a1470be2e62c89bdeb9c71bd70c71a" }
```

Inserted at the beginning of each reasoning/tool cycle within an
assistant message. `snapshot` is the SHA-1 tree hash in the
project's snapshot git repo — the filesystem state as opencode saw
it right before the step began. Optional; absent when the step ran
too fast for the snapshot thread to settle.

### `step-finish`

```json
{
  "type": "step-finish",
  "reason": "stop",
  "snapshot": "f12e27e948a1470be2e62c89bdeb9c71bd70c71a",
  "tokens": {
    "total": 14471, "input": 12596, "output": 35, "reasoning": 0,
    "cache": { "read": 1840, "write": 0 }
  },
  "cost": 0
}
```

Closing marker for a step. `reason` mirrors the model provider's
finish reason (`stop`, `tool-calls`, `length`, `content-filter`, …).
`tokens` is a per-step delta; sum over all `step-finish` parts in a
session for a total. `cost` is USD for that step.

`reasoning` is an **additive** category, separate from `output` —
`total == input + output + reasoning + cache.read + cache.write` (verified
against real sessions; the Vercel AI SDK opencode uses reports
`reasoningTokens` separately from `outputTokens`). This differs from
Claude/OpenAI, where reasoning is already inside `output`. `toolpath-opencode`
therefore folds `reasoning` into the derived `output_tokens` so the IR's
`output` consistently means "all generated tokens" and the session total
isn't under-counted. So we don't discard the slice, the same folded
reasoning count is additionally recorded under
`token_usage.breakdowns["output"]["reasoning"]` — purely informational,
never summed into the total (output already counts it), preserving the
invariant `Σ(inner) = reasoning ≤ output`. It accumulates across all
`step-finish` parts in a turn exactly like the output total does, and is
omitted entirely when reasoning is 0.

### `snapshot`, `patch`

```json
{ "type": "snapshot", "snapshot": "<sha>" }
{ "type": "patch",    "hash": "<sha>", "files": ["src/a.rs", "src/b.rs"] }
```

Emitted when opencode explicitly records a snapshot outside the
normal step lifecycle, or when a patch is applied. The `hash` on
`patch` refers to a git object in the snapshot repo from which the
unified diff can be regenerated via `git show`.

### `file`

```ts
{
  type: "file"
  mime: string
  filename?: string
  url: string                  // data: URL or local file:// URL
  source?: FilePartSource      // Link back to the source range (file/symbol/resource)
}
```

An attached file — image, document, ingested context. `source` can
be a byte range in a file, a symbol from the LSP, or an MCP
resource. Rare in assistant output; common on user messages when the
user pastes or attaches content.

### `agent`

```ts
{ type: "agent", name: string, source?: { value: string, start: int, end: int } }
```

An `@agent-name` mention in a user message. `source` is the span in
the raw prompt where the mention occurred.

### `subtask`

```ts
{
  type: "subtask"
  prompt: string
  description: string
  agent: string               // Sub-agent name, e.g. "build", "review"
  model?: { providerID, modelID }
  command?: string
}
```

Represents a spawned sub-agent turn. The sub-agent's own session is
linked via `session.parent_id`.

### `retry`

```ts
{
  type: "retry"
  attempt: number
  error: APIError             // { message, statusCode?, isRetryable, … }
  time:  { created: number }
}
```

Emitted when a provider call retried after a transient failure.
Preserves the original error so the audit trail survives the retry.

### `compaction`

```ts
{
  type: "compaction"
  auto: boolean               // true = triggered by context-overflow
  overflow?: boolean
  tail_start_id?: MessageID   // First message of the post-compaction tail
}
```

Inserted when opencode compacts the conversation to stay under the
context window. Messages before `tail_start_id` are summarized into
a single synthetic user message; the history above the marker is
kept in the DB for reverts.

Compaction stays **within one session** — it sets the session row's
`time_compacting` timestamp but does not create a new session row.
(`session.parent_id` is for forked sub-agent sessions, not
compaction.) `tail_start_id` is a single anchor describing a
**contiguous** kept tail — everything from it forward survives, so
there is no non-contiguous "pinned" retention here. Message and part
ids are **not** reused across the boundary, so opencode compaction
carries no duplicate-id hazard.

## Tool catalogue

A reader should not enumerate tool names — any agent config can
expose arbitrary MCP / plugin tools. The observed-in-the-wild core
set is:

| Tool | Category | Notes |
|---|---|---|
| `bash`    | Shell      | `input: { command, description }`, `metadata: { exit, output, truncated }` |
| `read`    | FileRead   | `input: { filePath }`, `output` is an XML-tagged `<content>` block with line numbers |
| `write`   | FileWrite  | `input: { filePath, content }`, full file replace |
| `edit`    | FileWrite  | `input: { filePath, oldString, newString }`, in-place string replace |
| `glob`    | FileSearch | Pattern matching |
| `grep`    | FileSearch | Ripgrep-style content search |
| `webfetch`| Network    | HTTP GET + markdown extract |
| `websearch` | Network  | Provider-specific web search |
| `agent_*` | Delegation | Spawn / communicate with sub-agents |
| `mcp__<ns>__<tool>` | variable | External MCP server tools |

Upstream handlers live under
[`packages/opencode/src/tool/`](https://github.com/anomalyco/opencode/tree/dev/packages/opencode/src/tool).

## Snapshots

Opencode uses **git itself** as the filesystem-snapshot store. For
each (project, worktree) pair, opencode maintains a bare git
repository at:

```
$data/snapshot/<project-id>/<sha1(worktree)>/
```

- Outer dir: the project id (first-root-commit SHA).
- Inner dir: `sha1(worktree-absolute-path)` — so multiple worktrees
  or clones of the same project each get their own snapshot store.

The gitdir is populated by running the user's `git` binary with
`--git-dir <gitdir> --work-tree <worktree>` (see the
[`layer` Effect in `snapshot/index.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/snapshot/index.ts)).
Every `step-start` / `step-finish` / `snapshot` part references a
tree (or commit) SHA in that repo. Restoring a step:

```bash
git --git-dir=<snapshot-dir> --work-tree=<worktree> \
    checkout <snapshot-sha> -- .
```

Pruning: snapshots older than `7.days` get garbage-collected (see
`const prune = "7.days"` in `snapshot/index.ts`).

For a Toolpath derivation this is a gift: the unified diff between
any two steps is reproducible verbatim via
`git diff <from> <to>`, and full file content is available at any
step via `git show <sha>:<path>`. No diff reconstruction from tool
output is needed.

## Identifiers

From `packages/opencode/src/id/id.ts`. All opencode IDs share one
format:

```
<prefix>_<12-hex-char-timestamp><14-base62-chars-random>
```

| Prefix | Thing | Sort direction |
|---|---|---|
| `ses`   | Session        | descending (newest first) |
| `msg`   | Message        | ascending |
| `prt`   | Part           | ascending |
| `evt`   | Event          | — |
| `per`   | Permission     | — |
| `que`   | Question       | — |
| `usr`   | User           | — |
| `wrk`   | Workspace      | — |
| `tool`  | Tool binding   | — |
| `pty`   | PTY            | — |
| `ent`   | Entry          | — |

The 12-hex-char timestamp encodes `Date.now() * 0x1000 + counter`
(48 bits). For descending IDs the bits are inverted (`~now`) so
lexicographic sort orders newest-first — which is why session IDs
like `ses_24ee4deb6ffe…` don't look like timestamps but still sort
as expected. Recover the wall-clock millis via
`Identifier.timestamp(id)` on ascending IDs; for descending ones
(`ses_*`) invert the bits first.

## Logs

Plain-text files under `$data/log/`. Format:

```
LEVEL TIMESTAMP +DELTA service=NAME key=value …
```

Example:

```
INFO  2026-04-21T17:33:52 +39ms service=default directory=/Users/ben/empathic/oss/toolpath creating instance
INFO  2026-04-21T17:33:52 +3ms service=project directory=/Users/ben/empathic/oss/toolpath fromDirectory
INFO  2026-04-21T17:33:52 +5ms service=server method=GET path=/session request
```

- `LEVEL`: `INFO`, `WARN`, `ERROR`, `DEBUG`.
- `TIMESTAMP`: ISO-8601 second precision.
- `+DELTA`: millis since the previous log line in the same process.
- `service=NAME`: logger name (from `Log.create({ service })`).
- Remaining tokens: free-form `key=value` pairs.

Not needed for conversation reconstruction — every event that lands
in the log also lands in the DB. Useful for debugging writer
behavior (e.g. snapshot lock contention, retry loops).

## Round-trip fidelity

Pitfalls for anyone parsing and re-emitting the format:

1. **Message and part payloads are JSON-in-TEXT columns.** Round-trip
   requires canonical JSON handling (key order and whitespace are
   writer-determined). Prefer field-by-field equality over byte
   comparison.
2. **`PartBase` fields are stripped at the SQL layer.** Upstream
   TypeScript types declare `id`, `sessionID`, `messageID` on every
   part, but the `data` column does not include them — they live in
   `part.id`, `part.session_id`, `part.message_id`. Don't expect to
   find them inside `data`.
3. **`message.data` discriminates on `role`, `part.data` on `type`.**
   Both unions are open: opencode ships new variants with minor
   releases. Preserve unknown `role` and `type` values verbatim;
   do not fail the read.
4. **`tool.state` is itself a tagged union on `status`.** Treat
   unknown statuses as preserving `input` + raw payload.
5. **`ToolStateCompleted.metadata` is schema-free per tool.** Don't
   try to normalize it — `bash` carries one shape, `edit` another,
   MCP tools their own. Store as `Record<string, any>`.
6. **Part ordering is `(time_created ASC, id ASC)`, not insertion
   order.** The `id` component is the tiebreaker when multiple parts
   land in the same millisecond — which happens often for rapid
   reasoning streams.
7. **Messages chain via `assistant.parentID`, not a dedicated table.**
   User messages have no `parentID` field; the chain is implicit
   via timestamp order.
8. **`session.parent_id` marks forked / spawned sub-agent sessions.**
   The parent's conversation stays in its own session; only the
   sub-agent's own turns live here. If you want the full story,
   walk both.
9. **`summary_diffs` can be null on fresh sessions** and is
   recomputed lazily. It's a derived view over the snapshot repo,
   not a source of truth.
10. **The `event` / `event_sequence` tables may be empty.** Sync is
    optional; don't rely on an append-only event log being present.
11. **`auth.json` contains live credentials.** A reader that copies
    session data anywhere (file, pastebin, remote URL) MUST NOT
    include this file.
12. **Snapshot gitdir is keyed by SHA-1 of the worktree path.** If
    the project was moved between clones with different paths, the
    older snapshots are under an orphan gitdir. The project id
    itself is stable (first-root-commit SHA) so sessions survive,
    but their `snapshot` references only resolve under the old
    gitdir.

## What a `toolpath-opencode` provider needs

Minimum viable mapping, if we follow the Pi-style approach (build a
`ConversationView` and hand to `toolpath_convo::derive_path`):

| opencode construct | `toolpath-convo` mapping |
|---|---|
| `session.id` | `ConversationView.id` |
| `session.directory` + `project.worktree` | `Turn.environment.working_dir`, `path.base.uri` |
| `project.id` (first-root-commit SHA) | `path.base.ref_str` (stable-enough) |
| User `message` | `Turn { role: User }` |
| Assistant `message` | `Turn { role: Assistant, model: modelID }` |
| `user.system` | `Turn { role: System }` or `ConversationEvent` |
| `reasoning` part | `Turn.thinking` (plaintext — safe to render) |
| `text` part | appended to `Turn.text` |
| `tool` part (state: completed) | `Turn.tool_uses[] { input, result: state.output }` paired via `callID` |
| `tool` part (state: error) | same, with `result.is_error = true`, `result.content = state.error` |
| `step-start` / `step-finish` | attach `snapshot` SHA to the turn for file-artifact reconstruction |
| `patch` part | file-artifact sibling `ArtifactChange.raw` from `git diff <from> <to>` |
| `step-finish.tokens` | `Turn.token_usage` (delta) + summed into `ConversationView.total_usage` |
| `subtask` part | `Turn.delegations[]`, with sub-session linked via `session.parent_id` |
| `compaction` part | `ConversationEvent { event_type: "compaction" }` |
| `retry` part | `ConversationEvent { event_type: "retry" }` |
| `todo` row | `ConversationEvent { event_type: "todo" }` or top-level path meta |
| unknown part `type` | `ConversationEvent` preserving the raw payload |

For **file-change fidelity** we don't need any diff reconstruction
from tool output: the snapshot git repo carries both endpoints of
every step. Walk the session's parts in order, collect the
`(prev-snapshot, this-snapshot)` pairs, and emit `ArtifactChange`
entries with `raw` set to
`git diff <prev> <this> -- <file>` and `structural` carrying
the per-file add/update/delete classification from
`git diff --name-status`.

For **byte-equivalent round-trip** at the SQL layer, treat
`message.data` and `part.data` as opaque JSON strings; don't
re-canonicalize. Upstream uses
[Zod](https://zod.dev) with `z.record(z.string(), z.any())` catch-alls
on the provider-metadata fields, so novel vendor extensions already
survive by construction.

## References

- opencode site: <https://opencode.ai>
- opencode repo: <https://github.com/anomalyco/opencode>
  (default branch: `dev`)
- Session / message / part Zod schemas:
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts>
- Drizzle table definitions:
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/session.sql.ts>
- Project ID derivation:
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/project/project.ts>
- Project table schema:
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/project/project.sql.ts>
- Snapshot subsystem (git-backed):
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/snapshot/index.ts>
- ID format + prefixes:
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/id/id.ts>
- Global path resolution (XDG):
  <https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/global/index.ts>
- Tool handlers:
  <https://github.com/anomalyco/opencode/tree/dev/packages/opencode/src/tool>
