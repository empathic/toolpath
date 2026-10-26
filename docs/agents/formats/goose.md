# Goose session format (draft)

Reference for the on-disk format produced by Block's
[goose](https://block.github.io/goose) CLI (repo
[`block/goose`](https://github.com/block/goose), Apache-2.0), as intended
input for a future `toolpath-goose` provider. Compiled from:

1. **Observed**: `goose` `1.49.0` from `nixpkgs#goose-cli`, run with
   `GOOSE_PATH_ROOT=<scratch>` on 2026-10-15: `goose info`, `goose session
   list -f json`, `goose run -t "say hi"` (no provider configured, so no
   model call). The `sessions.db` it created is the schema source below.
2. **Read**: `crates/goose/src/config/paths.rs` at tag `v1.49.0` (path
   resolution), `goose --help`, `goose session --help`, `goose run --help`,
   `goose session import --help`.
3. **Not read**: the message JSON (`messages.content_json`) — the Rust
   `Message`/`MessageContent` types in `crates/goose/src/conversation/`.
   Shapes for that column are marked *inferred* or *3p* (survey).

Date: **2026-10-15**, schema_version **16**.

## Install route and version

- `nix shell nixpkgs#goose-cli` → `goose 1.49.0` (the plain `nixpkgs#goose`
  is an unrelated package, 3.28.0).
- Vendor: `curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | bash`, or brew.

**Isolation warning.** goose does **not** derive its directories from
`$HOME` on macOS: with `HOME` pointed at a temp dir it still created
`~/.config/goose`, `~/.local/share/goose/sessions/sessions.db` and
`~/.local/state/goose/logs` in the real home (observed; `etcetera`
strategy). Set `GOOSE_PATH_ROOT=<dir>` instead (read: `paths.rs` line 41);
then config, data and state live under `<dir>/config`, `<dir>/data`,
`<dir>/state` (observed).

## Storage root

Default (macOS/Linux, read: `goose info`):

```
~/.config/goose/config.yaml                      provider, model, extensions
~/.local/share/goose/sessions/sessions.db        SQLite, WAL mode (+ -shm, -wal)
~/.local/state/goose/logs/cli/<YYYY-MM-DD>/<YYYYMMDD_HHMMSS>.log
```

With `GOOSE_PATH_ROOT=R`: `R/config/config.yaml`, `R/data/sessions/sessions.db`,
`R/state/logs/…` (observed). Legacy `*.jsonl` session files in the
sessions dir are auto-imported into the DB since 1.10 (survey, 3p;
`crates/goose/src/session/legacy.rs` exists at v1.49.0 — read: file
listing only).

## `sessions.db` — SQLite layout

Observed `.schema` (schema_version 16):

```sql
CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                 -- "YYYYMMDD_HHMMSS" style (3p; see Identifiers)
  name TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  user_set_name BOOLEAN DEFAULT FALSE,
  session_type TEXT NOT NULL DEFAULT 'user',
  working_dir TEXT NOT NULL,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  extension_data TEXT DEFAULT '{}',    -- JSON
  total_tokens INTEGER, input_tokens INTEGER, output_tokens INTEGER,
  cache_read_tokens INTEGER, cache_write_tokens INTEGER,   -- last-request snapshot (inferred)
  accumulated_total_tokens INTEGER, accumulated_input_tokens INTEGER,
  accumulated_output_tokens INTEGER, accumulated_cache_read_tokens INTEGER,
  accumulated_cache_write_tokens INTEGER, accumulated_cost REAL,  -- session totals
  schedule_id TEXT, recipe_json TEXT, user_recipe_values_json TEXT,
  provider_name TEXT, model_config_json TEXT,
  goose_mode TEXT NOT NULL DEFAULT 'auto',
  archived_at TIMESTAMP, project_id TEXT, parent_session_id TEXT
);

CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT,                     -- goose's own message id (nullable on old rows)
  session_id TEXT NOT NULL REFERENCES sessions(id),
  role TEXT NOT NULL,                  -- "user" | "assistant" (inferred)
  content_json TEXT NOT NULL,          -- JSON MessageContent[]
  created_timestamp INTEGER NOT NULL,  -- epoch seconds (inferred from name)
  timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  tokens INTEGER,
  metadata_json TEXT
);

CREATE TABLE usage_ledger (            -- one row per model call
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  created_timestamp INTEGER NOT NULL,
  model TEXT, input_tokens INTEGER, output_tokens INTEGER, total_tokens INTEGER,
  cache_read_tokens INTEGER, cache_write_tokens INTEGER,
  cost REAL, cost_source TEXT, is_compaction INTEGER DEFAULT 0
);

CREATE INDEX idx_messages_session ON messages(session_id);
CREATE INDEX idx_messages_timestamp ON messages(timestamp);
CREATE INDEX idx_messages_message_id ON messages(message_id);
CREATE INDEX idx_messages_session_created ON messages(session_id, created_timestamp, id);
CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC);
CREATE INDEX idx_sessions_type ON sessions(session_type);
CREATE INDEX idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX idx_usage_ledger_session ON usage_ledger(session_id);
-- plus provider_inventory_entries / provider_inventory_models (model catalogue cache)
```

Message order: `(created_timestamp ASC, id ASC)` per
`idx_messages_session_created`.

### `messages.content_json` (inferred / 3p)

The survey's third-party readers match `LIKE '%toolRequest%'`, so the
column is goose's `MessageContent` enum serialised with serde: a JSON
array of tagged objects such as `{"type":"text","text":…}`,
`{"type":"toolRequest","id":…,"toolCall":{…}}`,
`{"type":"toolResponse","id":…,"toolResult":{…}}`, `{"type":"thinking",…}`.
Tool request and response are **separate messages** (assistant then user),
paired by `id`. **Not verified** — read
`crates/goose/src/conversation/message.rs` before building types.

### Identifiers

Session ids: the CLI's `--path` help says a legacy filename
`20250325_200615.jsonl` maps to id `20250325_200615`, so ids are
`YYYYMMDD_HHMMSS` (read: `--help`). Whether newer sessions use a
different generator is **open**. `message_id` format not seen.

## Session resolution / resume

- `goose session -r` resumes the most recent; `-r -n <name>` by name;
  `-r --session-id <id>` by id; `--fork` copies into a new session;
  `--edit` opens the conversation in `$EDITOR` before resuming (read:
  `--help`).
- `goose run -r --session-id <id> -t "<prompt>"` resumes headlessly;
  `--no-session` runs without touching the DB.
- Resume reads the `sessions` row (working_dir, provider/model config,
  extension_data) and `messages` for the id (inferred).
- `goose session list -f json [-w <cwd>]` lists; `goose session export
  --session-id <id> --format json|yaml|markdown`.
- **`goose session import <file>`** accepts a goose JSON export or a
  **Claude Code / Codex / Pi `.jsonl` transcript** (read: `--help`;
  `crates/goose/src/session/import_formats/{claude_code,codex,pi}.rs`
  exist at v1.49.0).

## Headless mode

`goose run -t "<text>" | -i <file|->` with `-q/--quiet`,
`--output-format text|json|stream-json`, `--max-turns N`,
`--max-tool-repetitions N`, `--no-session`, `--provider`/`--model`
overrides, `--recipe`, `--system`. Without a configured provider it exits
with "No model configured. Run 'goose configure' first." and writes
nothing but a log (observed). `goose acp` / `goose serve` expose ACP.

## What a `toolpath-goose` provider needs

| goose construct | `toolpath-convo` mapping |
|---|---|
| `sessions.id` | `ConversationView.id` |
| `sessions.working_dir` | `SessionBase.working_dir` |
| `sessions.provider_name` / `model_config_json` | `ProducerInfo { name: "goose" }`, default `Turn.model` |
| `sessions.parent_session_id` | predecessor / fork link |
| `messages` role user | `Turn { role: User }` |
| `messages` role assistant | `Turn { role: Assistant }` with `text`/`thinking`/`toolRequest` → `tool_uses[]` |
| `toolResponse` content in the following user message | `ToolInvocation.result`, paired by `id` |
| `usage_ledger` rows between two assistant messages | `Turn.token_usage` (per call; sum per turn) |
| `sessions.accumulated_*` | `ConversationView.total_usage` (cross-check) |
| `extension_data` | `ConversationEvent` or ignore |

Derive cost: **medium** (rusqlite like `toolpath-opencode`, plus a
`MessageContent` type to read from upstream). Project cost: two options —
**medium** by inserting rows (`sessions` + `messages` + optional
`usage_ledger`; goose owns the DB and the schema moves, version 16 today),
or **cheap** by writing a Claude Code / Pi JSONL that toolpath already
projects and running `goose session import <file>`, then resuming by the
id goose prints. The second route reuses existing projectors and keeps
toolpath out of goose's schema; verify what id/working_dir import assigns.

## Open questions

1. `content_json` shape and the tool request/response pairing (read
   `conversation/message.rs`).
2. Session id generator for new sessions (still `YYYYMMDD_HHMMSS`?) and
   `message_id` format.
3. Does `goose session import` of a Pi/Claude JSONL preserve
   `working_dir` and set a resumable id? (Credential-free to test: import
   does not call a model.)
4. `created_timestamp` unit (seconds vs millis).

Needs credentials to go further: any provider key (or a local Ollama) for
one `goose run` that produces real `messages` and `usage_ledger` rows.
Import-based projection (3) can be tested without any.

## References

- Path resolution: <https://github.com/block/goose/blob/v1.49.0/crates/goose/src/config/paths.rs>
- Session manager (DDL/migrations): <https://github.com/block/goose/blob/v1.49.0/crates/goose/src/session/session_manager.rs> (not read here)
- Import formats: <https://github.com/block/goose/tree/v1.49.0/crates/goose/src/session/import_formats>
- Local evidence: `<scratchpad>/harness-goose/root/data/sessions/sessions.db`
