# Factory Droid session format (draft)

Reference for the on-disk format produced by Factory's `droid` CLI
(<https://docs.factory.ai/cli>), as intended input for a future
`toolpath-droid` provider. Compiled from:

1. **Observed**: one credential-less `droid exec --output-format json
   "say hi"` under a temporary `HOME`, droid `0.242.0` (darwin/arm64
   release binary, SHA-256 verified against the vendor checksum),
   2026-10-15. Auth failed, but droid still created a session file, a
   settings sidecar and the session index.
2. **Read from the binary**: zod schemas and SQL migration text embedded in
   the `droid` executable (`strings` output). Droid is closed source;
   there is no public schema. Claims from here are marked *binary*.
3. **Read from docs/help**: `droid --help`, `droid exec --help`,
   `droid search --help`.

Nothing below has been checked against a session that contains real
model turns. Shapes for `message` lines are from the binary's own
validators, not from a sample. Date: **2026-10-15**.

## Install route and version

- Vendor: `curl -fsSL https://app.factory.ai/cli | sh` downloads
  `https://downloads.factory.ai/factory-cli/releases/<VER>/<platform>/<arch>[-baseline]/droid`
  plus `droid.sha256`, installs to `~/.local/bin/droid`. `VER` is pinned
  inside the script (`0.242.0` on 2026-10-15). (read: install script)
- Not in nixpkgs (`droid`, `factory`, `factory-droid` absent). The survey
  says `nix-ai-tools` has a `droid` package; not verified here.
- For a credential-free probe, download the release binary directly into
  a scratch dir and run it with `HOME=<tmp>`; droid honours `HOME`
  (observed).

## Storage root

```
~/.factory/
  host.json                         { schemaVersion: 1, hostId: <uuid>, createdAt: <ms> }
  settings.json                     user settings (not created by exec; named in --help)
  sessions/
    <project-key>/                  cwd with "/" → "-", so it always starts with "-"
      <uuid>.jsonl                  transcript, one JSON object per line
      <uuid>.settings.json          per-session settings + token totals (whole-file JSON)
    btw/<uuid>.jsonl                "btw" forks (binary: SQL comment)
    <uuid>.jsonl                    global root (binary: SQL comment; not observed)
  cache/session-index/index.db      SQLite search/listing index (derived, rebuildable)
  state/{background-tasks,task-invocations,background-processes}.json
  logs/droid-log-single.log         JSON-ish lines with sessionId, traceId
  droids/*.md                       bundled subagent definitions
  telemetry/droid-installation-id
  bin/libsqlite3-*.dylib            droid pins its own SQLite (needs jsonb(), 3.45+)
```

(observed, except where marked)

### Project key

The transcript directory is `sanitizePathToDirectoryName(cwd)`: the
absolute, **realpath'd** cwd with every `/` replaced by `-`. Example
(observed):

```
/private/tmp/…/scratchpad/harness-droid/proj
  ──▶  -private-tmp-…-scratchpad-harness-droid-proj
```

The index's SQL comment (binary) says the key is stored rather than
recomputed because `realpathSync` makes it non-pure: a symlinked or
moved cwd sanitizes differently later. Lossy for paths containing `-`,
exactly like Pi's encoder. A reader that resolves `<id>` should scan all
project dirs (and `btw/`, and the root) rather than recompute the key.

## Transcript JSONL

### Line 1: `session_start`

Observed:

```json
{"type":"session_start","id":"14d8c814-f277-4d6f-867a-682e616776ee","title":"say hi","owner":"bobby","version":2,"cwd":"/private/tmp/…/harness-droid/proj","hostId":"14f819ce-b20f-40b2-9d50-99208744b4e7"}
```

Full writer shape (binary):

```ts
{
  type: "session_start",
  id: string,                 // session uuid == file stem
  title: string,
  isSessionTitleManuallySet?: true,
  owner: string,              // OS user
  organizationId?: string,
  parent?: string,            // forked-from session id
  callingSessionId?: string,  // parent session for subagents
  callingToolUseId?: string,  // the tool_use that spawned this session
  version: 2,
  cwd: string,
  hostId?: string,
  privacyLevel?: "private" | …
}
```

The binary rejects a file whose first line is not `session_start`
("Invalid session file: first line is not session_start event").

### `message` lines (binary; not observed)

Two validators exist in the binary. The permissive one (used by the
session index) is:

```ts
{
  type: "message",
  timestamp?: string | number,
  message: {
    role: "user" | "assistant" | …,
    content: string | ContentBlock[],
    visibility?: …, isUserVisible?: boolean, hiddenFromUserViews?: boolean,
    hookEventName?: string          // hook-injected messages
  }
  // .passthrough(): other keys preserved
}
```

The strict one (used by the daemon/turn machinery) adds the id chain:

```ts
{
  type: "message",
  id: string,                 // required
  parentId?: string,
  seq?: number,
  timestamp: string,          // required, non-empty
  message: { role: "user" | "assistant", content: string | ContentBlock[] },
  tokens?: number,
  compactionSummaryId?: string
}
```

Content blocks (binary, Anthropic-style):

| `type` | Fields |
|---|---|
| `text` | `text` |
| `thinking` | `thinking` |
| `tool_use` | `id`, `name`, `input` (search index reads `id`/`name`) |
| `tool_result` | `tool_use_id` (inferred from the Anthropic shape; the validator only requires `content`), `content: string | object[]` |
| `document` | `source: { type: "text", data }` |

Tool call ↔ result: both live inside `message.content` blocks, so the
pairing is Claude-Code-style (`tool_use.id` ↔ `tool_result.tool_use_id`)
rather than Pi-style separate entries. **Inferred** from the block
shapes; needs a real session to confirm which message carries the
`tool_result` (a `user`-role message, as in Claude Code, is the likely
answer).

### Other line types

- `agent_turn_outcome` (binary): `{ type, turnId, reason, resultKind:
  "structured" | "text" | "null", result?, schemaFingerprint? }` —
  closes a turn; `structured` carries `--json-schema` output.
- The binary also counts `"span"`, `"summary"`, `"compaction"` and
  `"settings"` literals, but no transcript-line validator for them was
  found; `compactionSummaryId` on `message` says compaction summaries
  are referenced by id. **Open.**

## `<uuid>.settings.json`

Observed (whole-file JSON, rewritten on change):

```json
{
  "assistantActiveTimeMs": 0,
  "model": "gpt-6-sol",
  "reasoningEffort": "high",
  "interactionMode": "auto",
  "autonomyLevel": "off",
  "autonomyMode": "normal",
  "compactionThresholdCheckEnabled": true,
  "allowSubagentsInScripts": false,
  "tags": [ { "name": "exec" } ],
  "toolExecutionMode": "direct_only",
  "toolExecutionModeModelId": "gpt-6-sol",
  "tokenUsage": { "inputTokens": 0, "outputTokens": 0, "cacheCreationTokens": 0,
                  "cacheReadTokens": 0, "thinkingTokens": 0, "factoryCredits": 0 },
  "childInclusiveTokenUsageBySessionId": {},
  "inclusiveTokenUsage": { …same shape… }
}
```

Token usage is **session-level only** here. Whether `message` lines carry
per-turn usage is unknown (`tokens?: number` on the strict validator is a
single integer). `childInclusiveTokenUsageBySessionId` rolls subagent
sessions up into `inclusiveTokenUsage`.

## `cache/session-index/index.db`

SQLite, droid-pinned build, migrations 1–6 (`baseline`,
`transcript-summary-fingerprint`, `tags-role-key`, `daemon-session-fields`,
`schema-version`, `spans-and-search`). Tables (observed `.schema`):

- `sessions(session_id PK, project_key NOT NULL, cwd, title, org_id, owner,
  parent, calling_session_id, calling_tool_use_id, mission, tags JSONB,
  has_user_visible_messages, archived_at, first_event_at, last_event_at,
  messages_count, host_id, worktree, artifacts, settings_write_seq)`.
  `_at` columns are epoch **milliseconds**.
- `session_ingest(session_id PK, transcript_mtime_ms, transcript_size,
  transcript_offset, …)` — incremental ingest cursor per transcript.
- `session_spans(session_id, span_index, line_start, line_end, text,
  vector BLOB, …)` — chunked text + int8 embeddings for `droid search`.
- `session_term_filters(session_id, filter BLOB)` — bloom filters.

It is a **derived cache**: the SQL comments say every row is written by
something that was looking at the transcript. A reader should treat the
JSONL + settings as truth and the index as an optional accelerator; a
projector does **not** need to write it (`droid search --reindex`
rebuilds it; the picker's behaviour on an unindexed file is untested).

## Session resolution / resume

- `droid --resume [sessionId]` / `droid resume [sessionId] [prompt…]` —
  picker without an id; `--last` resumes the most recent in the current
  folder; "session link" is also accepted (read: `--help`).
- `droid exec -s <id> "<prompt>"` continues a session headlessly
  (`--session-id` *requires* a prompt); `--fork <id>` copies into a new
  session.
- `droid exec` prints `session_id` in its JSON result even on failure
  (observed), so a caller can learn the id without parsing the store.
- What resume reads: the transcript (first line must be `session_start`)
  plus the settings sidecar (model, autonomy). **Inferred** from the
  validators; not exercised (needs auth).

## Headless mode

`droid exec [prompt]` — `-o text|json|stream-json|stream-jsonrpc|acp`,
`--input-format stream-json|stream-jsonrpc`, `-f <file>`, `--auto
low|medium|high` (read-only by default), `--skip-permissions-unsafe`,
`-m <model>`, `-r <effort>`, `--only-tools/--add-tools/--remove-tools`,
`--cwd`, `--tag`, `--mission`, `--json-schema`-style structured output via
`agent_turn_outcome.resultKind: structured`. Observed failure result:

```json
{"type":"result","subtype":"failure","is_error":true,"duration_ms":1,"num_turns":0,
 "result":"Authentication failed. Please log in using /login or set a valid FACTORY_API_KEY environment variable.",
 "session_id":"14d8c814-…","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"factory_credits":0}}
```

## What a `toolpath-droid` provider needs

| Droid construct | `toolpath-convo` mapping |
|---|---|
| `session_start.id` | `ConversationView.id` |
| `session_start.cwd` (+ `hostId`, `owner`) | `SessionBase.working_dir`; `ProducerInfo { name: "droid", version: ? }` — the CLI version is in `logs/`, not the transcript |
| `session_start.parent` | predecessor link |
| `callingSessionId` / `callingToolUseId` | `DelegatedWork` back-link from the parent's `tool_use` |
| `message` role user | `Turn { role: User }` |
| `message` role assistant | `Turn { role: Assistant }`; `thinking` blocks → `thinking`; `text` → `text`; `tool_use` → `tool_uses[]` |
| `tool_result` block | `ToolInvocation.result`, paired by `tool_use_id` |
| `agent_turn_outcome` | `stop_reason` / `ConversationEvent` |
| `*.settings.json.tokenUsage` | `ConversationView.total_usage` (session-level; per-turn stays `None`) |
| `*.settings.json.model` | default `Turn.model` when the message carries none |

Derive cost: **easy** once a real transcript confirms block placement.
Project cost: **medium** — write `session_start` + `message` lines, the
settings sidecar with the right `model`/`tags`, and either skip the index
(preferred) or run `droid search --reindex`. Per-turn token usage is
lossy on round-trip (session-level totals only).

## Open questions

1. Real `message` line sample: which keys the CLI actually writes
   (`id`/`parentId`/`timestamp`?), and where `tool_result` blocks live.
2. Do `span`/`summary`/`compaction` appear as transcript lines?
3. Does the picker list a hand-written session that is absent from
   `index.db`?
4. Does resume require the cloud (`syncSessionCreate` exists in the
   binary; the local run worked offline up to auth)?
5. `nix-ai-tools#droid` currency.

Needs credentials to go further: a Factory account or `FACTORY_API_KEY`
for one real exec run (that would answer 1–4 in a minute).

## References

- Local evidence: `<scratchpad>/harness-droid/home/.factory/` (observed
  tree), `<scratchpad>/harness-droid/strings.txt` (binary strings).
- Install script: <https://app.factory.ai/cli>
- CLI reference: <https://docs.factory.com/droid-cli/cli-reference>
