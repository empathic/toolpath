# Cline CLI session format (draft)

Reference for the on-disk format produced by the Cline **CLI**
(`cline`, repo [`cline/cline`](https://github.com/cline/cline), Apache-2.0),
as intended input for a future `toolpath-cline` provider. The VS Code
extension keeps a different store
(`globalStorage/saoudrizwan.claude-dev/tasks/<id>/…`) that is out of
scope here. Compiled from:

1. **Observed**: `cline` `3.0.68` from `nixpkgs#cline`, run as
   `cline --json -t 30 "say hi"` under a temporary `HOME` on 2026-10-15.
   Auth failed (no Cline account), but the CLI wrote a complete session
   directory, both SQLite databases and the provider settings.
2. **Read from the binary**: the bundled Bun executable's strings
   (`libexec/cline/bin/cline`) for file naming and content-block types.
   Marked *binary*.
3. **Read**: `cline --help`, `cline history --help`, `cline history --json`.

Date: **2026-10-15**.

## Install route and version

- `nix shell nixpkgs#cline` → `cline 3.0.68` (a Bun single-file binary;
  honours `HOME`, observed).
- Vendor: `npm i -g @cline/cli` (survey).
- `--config <dir>` (default `~/.cline`) and `--data-dir <dir>` (default
  `~/.cline/data`) relocate state without touching `HOME` (read: `--help`).

## Storage root

Observed after one failed headless run:

```
~/.cline/
  data/
    settings/providers.json          { version: 1, lastUsedProvider, providers: { <id>: { settings: { provider, model }, updatedAt, tokenSource } } }
    sessions/
      <id>/
        <id>.json                    session manifest (whole-file JSON)
        <id>.messages.json           conversation (whole-file JSON)
        <id>.compaction.json         (binary: name only; not observed)
        <stem>.messages.json         subagent transcripts beside the root's (binary)
    db/sessions.db                   SQLite index: sessions, subagent_spawn_queue, schedules, schedule_executions
    db/teams.db                      SQLite: team_events, team_runtime_snapshot, team_tasks, …
    cache/feature-flags.json
    locks/hub/…
    logs/{cline.log,hub-daemon.log}
  hooks/                             (default --hooks-dir; not created)
  worktrees/                         (--worktree; not created)
```

Sessions are **not** grouped by project; `cwd`/`workspace_root` live in
the manifest and the index.

### Identifiers

Session id = `${Date.now()}_${nanoid(5, "0-9a-z")}` (binary), e.g.
`1792091853538_a1oo2` (observed). Conversation id inside the run is
`conv_<ms>_<rand>` (binary; appears in `--json` events as `taskId`, not
in the files). Message ids are UUIDv4; error placeholders are
`error_<uuid>` (observed).

## `<id>.json` — session manifest

Observed:

```json
{
  "version": 1,
  "session_id": "1792091853538_a1oo2",
  "source": "cli",
  "pid": 41791,
  "started_at": "2026-10-15T19:17:33.540Z",
  "ended_at": "2026-10-15T19:17:33.807Z",
  "exit_code": 1,
  "status": "failed",
  "interactive": false,
  "provider": "cline",
  "model": "apodex/apodex-1.1-mini:free",
  "cwd": "/…/harness-cline/proj",
  "workspace_root": "/…/harness-cline/proj",
  "team_name": "team-L7pqp",
  "enable_tools": true, "enable_spawn": true, "enable_teams": true,
  "prompt": "say hi",
  "metadata": {
    "sessionHistoryOrigin": { "mode": "user", "version": "3.0.68" },
    "title": "say hi",
    "totalCost": 0,
    "aggregatedAgentsCost": 0,
    "usage":          { "inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheWriteTokens": 0, "totalCost": 0 },
    "aggregateUsage": { …same shape… }
  }
}
```

`status` values seen in the binary: `idle`, `running`, `failed`
(observed), presumably `completed`. `usage` is the root agent;
`aggregateUsage` includes spawned subagents.

## `<id>.messages.json` — conversation

Observed:

```json
{
  "version": 1,
  "updated_at": "2026-10-15T19:17:33.793Z",
  "agent": "lead",
  "sessionId": "1792091853538_a1oo2",
  "origin": { "source": "cli", "mode": "user", "sessionId": "1792091853538_a1oo2", "version": "3.0.68" },
  "messages": [
    { "id": "f2bd4651-…", "role": "user",
      "content": [ { "type": "text", "text": "<user_input mode=\"act\">say hi</user_input>" } ],
      "ts": 1792091853572 },
    { "id": "error_26c7dbe3-…", "role": "assistant",
      "content": [ { "type": "text", "text": "Unauthorized: …" } ],
      "ts": 1792091853791,
      "metadata": { "displayOnly": true, "displayRole": "error" },
      "modelInfo": { "id": "apodex/apodex-1.1-mini:free", "provider": "cline" } }
  ],
  "system_prompt": "You are Cline, an AI coding agent. …"
}
```

Notes:

- User text is wrapped in `<user_input mode="act|plan|yolo">…</user_input>`
  (observed; the system prompt documents it). A derive should unwrap it
  and keep `mode` as an attribute.
- `ts` is epoch milliseconds.
- `metadata.displayOnly: true` marks rows never sent to the model (error
  placeholders); skip them for turns, keep as `ConversationEvent`.
- Content block types in the binary: `text`, `reasoning`, `thinking`,
  `redacted_thinking`, `image`, `tool_use`, `tool_result`, `tool_call`.
  Result correlation keys present: `tool_use_id`, `toolUseId`,
  `isError`/`is_error`. The exact assistant `tool_use` block and the
  per-message `metrics` object (survey, 3p) were **not observed** —
  the run never reached a model.
- The whole file is rewritten on each update (`writeFileSync`, binary);
  there is no append-only log.

## `db/sessions.db`

Observed `.schema` (abridged):

```sql
CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY, source TEXT NOT NULL, pid INTEGER NOT NULL,
  started_at TEXT NOT NULL, ended_at TEXT, exit_code INTEGER,
  status TEXT NOT NULL, status_lock INTEGER NOT NULL DEFAULT 0,
  interactive INTEGER NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
  cwd TEXT NOT NULL, workspace_root TEXT NOT NULL, team_name TEXT,
  enable_tools INTEGER NOT NULL, enable_spawn INTEGER NOT NULL, enable_teams INTEGER NOT NULL,
  parent_session_id TEXT, parent_agent_id TEXT, agent_id TEXT, conversation_id TEXT,
  is_subagent INTEGER NOT NULL DEFAULT 0, prompt TEXT, metadata_json TEXT,
  transcript_path TEXT NOT NULL DEFAULT '', hook_path TEXT NOT NULL,
  messages_path TEXT,                  -- ABSOLUTE path to <id>.messages.json
  updated_at TEXT NOT NULL
);
CREATE TABLE subagent_spawn_queue (…);  CREATE TABLE schedules (…);  CREATE TABLE schedule_executions (…);
```

`cline history --json` is a straight dump of this table (observed), with
`messagesPath` absolute. The manifest and the row carry the same fields
(snake_case in files, camelCase in `history --json`).

## Session resolution / resume

- `cline --id <session-id> [prompt]` resumes by id; `cline history` lists
  (`--json`, `--limit`, `--page`); `cline history export <id>` writes a
  standalone HTML (read: `--help`).
- What resume reads (inferred): the `sessions` row (or manifest) for
  provider/model/cwd and `messages_path` → `<id>.messages.json` for the
  conversation. Because `messages_path` is absolute, moving `~/.cline`
  breaks resume unless the row is rewritten.

## Headless mode

Default invocation is headless act-mode with auto-approve:
`cline [--json] [-p] [--auto-approve false] [-c <cwd>] [-m <model>]
[-P <provider>] [-k <key>] [-s <system>] [-t <seconds>] "<prompt>"`.
`--json` streams NDJSON events: `hook_event` (`agent_start`,
`agent_error`, …), `agent_event` (`iteration_start`, `error`),
`run_result` (`finishReason`, `iterations`, `usage`, `aggregateUsage`,
`durationMs`, `text`, `model`), `error` (observed). `--acp` for editors,
`-i/--tui` interactive, `-z/--zen` background hub.

## What a `toolpath-cline` provider needs

| Cline construct | `toolpath-convo` mapping |
|---|---|
| `session_id` | `ConversationView.id` |
| manifest `cwd` / `workspace_root` | `SessionBase.working_dir` |
| manifest `provider`/`model`, `origin.version` | `ProducerInfo { name: "cline", version }`, default `Turn.model` |
| `parent_session_id` / `is_subagent` (index) | delegation links |
| message role user (unwrapped `<user_input>`) | `Turn { role: User }` |
| message role assistant | `Turn { role: Assistant, model: modelInfo.id }`; `thinking`/`reasoning` → `thinking`; `text` → `text`; `tool_use` → `tool_uses[]` |
| `tool_result` block | `ToolInvocation.result` by `tool_use_id` |
| `metadata.displayOnly` rows | `ConversationEvent { event_type: "display." + displayRole }` |
| manifest `metadata.usage` | `ConversationView.total_usage`; per-turn from `metrics` if present (unverified) |
| `system_prompt` | `Turn { role: System }` or event |

Derive cost: **easy** (two JSON files; Anthropic-style blocks). Project
cost: **medium-hard**: write manifest + messages file **and** upsert the
`sessions` row with an absolute `messages_path`, matching `status`
semantics (`status_lock`), or resume will not find it. `teams.db` can be
left alone (inferred: only team features read it).

## Open questions

1. Assistant `tool_use` block shape and whether `tool_result` sits in a
   user-role message (Anthropic layout) — needs one authenticated run.
2. Per-message `metrics` / token fields (survey claims them; not seen).
3. `<id>.compaction.json` contents.
4. Whether `cline --id` consults the DB row or the manifest first.

Needs credentials to go further: a Cline account **or** any provider key
via `-P <provider> -k <key>` (e.g. `-P anthropic`), for one real turn.

## References

- CLI reference: <https://docs.cline.bot/cline-cli/cli-reference>
- Repo (CLI under `cli/`): <https://github.com/cline/cline/tree/v3.0.68/cli>
- Local evidence: `<scratchpad>/harness-cline/home/.cline/data/`,
  `<scratchpad>/harness-cline/strings.txt`
