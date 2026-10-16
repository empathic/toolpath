# Hermes Agent session format (draft)

Reference for the on-disk format produced by Nous Research's
[Hermes Agent](https://hermes-agent.nousresearch.com) (repo
[`NousResearch/hermes-agent`](https://github.com/NousResearch/hermes-agent),
MIT), as intended input for a future `toolpath-hermes` provider. Compiled
**entirely from source** — the PyPI sdist `hermes_agent-0.19.0.tar.gz`
(2026-10-15) — because the package is not in nixpkgs and installing its
dependency tree was out of scope. No session was created or observed.
Every claim below is *read* from the file named unless marked *inferred*.

Date: **2026-10-15**, `SCHEMA_VERSION = 22` (`hermes_state.py:155`).

## Install route and version

- Not in nixpkgs (`hermes`, `hermes-agent` absent). PyPI `hermes-agent`
  0.19.0; vendor route is `pip`/`uv tool install hermes-agent` (survey).
- Source read from `<scratchpad>/harness-hermes/hermes_agent-0.19.0/`.
- Isolation: `HERMES_HOME=<dir>` overrides the root
  (`hermes_constants.py:get_hermes_home`, order: context override →
  `HERMES_HOME` → `~/.hermes`).

## Storage root

```
$HERMES_HOME/  (default ~/.hermes/)
  state.db              SQLite, WAL (apply_wal_with_fallback) — sessions, messages, FTS
  config.yaml           provider/model (hermes_cli/config.py)
  .hermes_history       readline history
  kanban.db             task board (hermes_cli/kanban_db.py)
  checkpoints/          git-style checkpoints (cli.py)
  errors.log
```

`DEFAULT_DB_PATH = get_hermes_home() / "state.db"` (`hermes_state.py:153`).

## `state.db` — SQLite layout

From `SCHEMA_SQL` (`hermes_state.py:757-909`), `DEFERRED_INDEX_SQL` and
`FTS_SQL`:

```sql
CREATE TABLE schema_version (version INTEGER NOT NULL);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                -- "YYYYMMDD_HHMMSS_<6 hex>" (cli.py:4100-4102)
  source TEXT NOT NULL,               -- "cli" | gateway platform ("telegram", "discord", …)
  user_id TEXT, session_key TEXT, chat_id TEXT, chat_type TEXT, thread_id TEXT,
  display_name TEXT, origin_json TEXT,  -- gateway peer identity (record_gateway_session_peer)
  expiry_finalized INTEGER DEFAULT 0,
  model TEXT, model_config TEXT, system_prompt TEXT,
  parent_session_id TEXT,             -- FK sessions(id): compression lineage / subagents
  started_at REAL NOT NULL, ended_at REAL, end_reason TEXT,   -- epoch seconds, float
  message_count INTEGER DEFAULT 0, tool_call_count INTEGER DEFAULT 0,
  input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
  cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
  reasoning_tokens INTEGER DEFAULT 0,
  cwd TEXT, git_branch TEXT, git_repo_root TEXT,
  billing_provider TEXT, billing_base_url TEXT, billing_mode TEXT,
  estimated_cost_usd REAL, actual_cost_usd REAL, cost_status TEXT, cost_source TEXT, pricing_version TEXT,
  title TEXT, api_call_count INTEGER DEFAULT 0,
  handoff_state TEXT, handoff_platform TEXT, handoff_error TEXT,
  compression_failure_cooldown_until REAL, compression_failure_error TEXT,
  compression_fallback_streak INTEGER NOT NULL DEFAULT 0,
  profile_name TEXT, rewind_count INTEGER NOT NULL DEFAULT 0,
  archived INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,  -- insertion order IS the read order (get_messages ORDER BY id)
  session_id TEXT NOT NULL REFERENCES sessions(id),
  role TEXT NOT NULL,                    -- "user" | "assistant" | "tool" | "system" (OpenAI style; inferred from tool_call_id/tool_name)
  content TEXT,                          -- text, or JSON for multipart (decoded by _decode_content)
  tool_call_id TEXT,                     -- on role="tool": the call this answers
  tool_calls TEXT,                       -- JSON on assistant rows (see below)
  tool_name TEXT,
  effect_disposition TEXT,
  timestamp REAL NOT NULL,
  token_count INTEGER, finish_reason TEXT,
  reasoning TEXT, reasoning_content TEXT, reasoning_details TEXT,   -- provider-specific thinking
  codex_reasoning_items TEXT, codex_message_items TEXT,              -- OpenAI Responses passthrough
  platform_message_id TEXT, observed INTEGER DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 1,     -- 0 = soft-deleted by rewind
  compacted INTEGER NOT NULL DEFAULT 0,
  api_content TEXT                       -- exact bytes sent to the API when != content
);

CREATE TABLE session_model_usage (      -- per (session, model, billing, task) token/cost rollup
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  model TEXT NOT NULL, billing_provider TEXT NOT NULL DEFAULT '', billing_base_url TEXT NOT NULL DEFAULT '',
  billing_mode TEXT NOT NULL DEFAULT '', task TEXT NOT NULL DEFAULT '',
  api_call_count INTEGER NOT NULL DEFAULT 0,
  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_cost_usd REAL NOT NULL DEFAULT 0, actual_cost_usd REAL NOT NULL DEFAULT 0,
  cost_status TEXT, cost_source TEXT, first_seen REAL, last_seen REAL,
  PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task)
);

CREATE TABLE state_meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE gateway_routing (scope, session_key, entry_json, updated_at, PRIMARY KEY (scope, session_key));
CREATE TABLE compression_locks (session_id PK, holder, acquired_at, expires_at);
CREATE TABLE async_delegations (delegation_id PK, origin_session, parent_session_id, state, …, task_json, result_json, …);

CREATE VIRTUAL TABLE messages_fts USING fts5(content);             -- + insert/delete/update triggers
CREATE VIRTUAL TABLE messages_fts_trigram USING fts5(content, tokenize='trigram');
```

Indexes: `idx_sessions_source`, `idx_sessions_parent`,
`idx_sessions_started (started_at DESC)`, `idx_messages_session
(session_id, timestamp)`, `idx_messages_session_active`,
`idx_sessions_session_key`, `idx_sessions_gateway_peer`, and the FTS
triggers — **the triggers maintain the FTS tables automatically** on
plain `INSERT INTO messages`, so a projector that inserts rows does not
have to touch `messages_fts*`.

### `messages.tool_calls` JSON

Written by `run_agent.py:2019-2026`: for SDK objects,
`[{"name": tc.function.name, "arguments": tc.function.arguments}, …]`;
for dict messages the raw OpenAI `tool_calls` list
(`[{"id", "type": "function", "function": {"name", "arguments"}}]`) is
stored as is. So the column has **two shapes**; the first lacks the call
`id`, which means pairing a `role="tool"` row (`tool_call_id`) back to its
call can be positional rather than by id. `get_messages` json-decodes it
and falls back to `[]` on error (`hermes_state.py:4505-4560`).

### Reconstructed example

```sql
INSERT INTO sessions (id, source, model, cwd, started_at) VALUES
  ('20261015_151730_a1b2c3', 'cli', 'anthropic/claude-sonnet-4.6', '/Users/me/proj', 1792091850.1);
INSERT INTO messages (session_id, role, content, timestamp) VALUES
  ('20261015_151730_a1b2c3', 'user', 'list the files', 1792091851.0);
INSERT INTO messages (session_id, role, content, tool_calls, finish_reason, timestamp) VALUES
  ('20261015_151730_a1b2c3', 'assistant', NULL,
   '[{"id":"call_1","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"ls\"}"}}]',
   'tool_calls', 1792091852.0);
INSERT INTO messages (session_id, role, content, tool_call_id, tool_name, timestamp) VALUES
  ('20261015_151730_a1b2c3', 'tool', 'a.rs\nb.rs\n', 'call_1', 'terminal', 1792091852.5);
INSERT INTO messages (session_id, role, content, finish_reason, timestamp) VALUES
  ('20261015_151730_a1b2c3', 'assistant', 'Two files: a.rs, b.rs.', 'stop', 1792091853.0);
```

(reconstructed from the DDL and `append_message`; tool name `terminal` is
a guess — Hermes toolsets are not enumerated here.)

## Session resolution / resume

- `hermes [chat] -r/--resume <SESSION>` — by id **or title**
  (`_parser.py:150`); `resolve_session_id` accepts a **unique prefix**
  (`hermes_state.py:3260`). `-c/--continue [NAME]` resumes by name or the
  most recent. `--no-restore-cwd` stops it `cd`-ing into `sessions.cwd`
  (`cli.py:_restore_session_cwd`).
- On resume the CLI reuses the id (`cli.py:4097`), loads `get_session`
  (model, system prompt, cwd) and `get_messages(session_id)` (active rows
  only, `ORDER BY id`); rewound rows (`active = 0`) are hidden.
- `hermes sessions list [--source cli] [--limit N] [--workspace NEEDLE]
  [--json]`, `hermes sessions export --session-id <id|prefix> --format
  jsonl|md|qmd|html|trace -o <path|->` (JSONL = one JSON object per
  session from `db.export_session`, `main.py:14724`).
- Session creation (`run_agent.py:614`): `create_session(session_id,
  source, model, model_config, system_prompt, user_id=None,
  parent_session_id, cwd, profile_name)`.

## Headless mode

- `hermes -z/--oneshot "<prompt>"` — prints only the final text, no
  session id line; `--usage-file PATH` writes a JSON usage report
  (`_parser.py:102-128`). The run still goes through the agent, so it
  creates a `sessions` row (inferred from `run_agent.py:614`).
- `hermes chat -q "<query>" [-Q/--quiet]` — single query, quiet mode
  prints final response **and session info**.
- `--accept-hooks`, `-m/--model`, `--provider`, `-t/--toolsets`,
  `-w/--worktree`.

## What a `toolpath-hermes` provider needs

| Hermes construct | `toolpath-convo` mapping |
|---|---|
| `sessions.id` | `ConversationView.id` |
| `sessions.cwd`, `git_branch`, `git_repo_root` | `SessionBase { working_dir, vcs_branch }` |
| `sessions.model` | default `Turn.model`; `ProducerInfo { name: "hermes" }` (version not stored — open) |
| `sessions.source` + `user_id`/`chat_id`/`display_name` | `DeriveConfig.user_actor` → `human:<source>/<user_id>` (reuse PR #127's channel actor) |
| `sessions.parent_session_id` | predecessor (compression lineage) or delegation parent |
| `messages` role user | `Turn { role: User }` |
| `messages` role assistant | `Turn { role: Assistant }`; `reasoning*` → `thinking`; `tool_calls` → `tool_uses[]`; `finish_reason` → `stop_reason` |
| `messages` role tool | `ToolInvocation.result` by `tool_call_id` (fallback: positional) |
| `messages` role system | `Turn { role: System }` |
| `active = 0` rows | `ConversationEvent { event_type: "rewind" }` |
| `session_model_usage` / `sessions.*_tokens` | `ConversationView.total_usage` (session-level; per-turn only via `token_count`) |
| `async_delegations` | `DelegatedWork` |

Derive cost: **medium** — SQLite, OpenAI-style rows, two `tool_calls`
shapes, multi-channel actors. Project cost: **medium** — insert
`sessions` + `messages`; FTS is trigger-maintained; keep `message_count`
/`tool_call_count` consistent (`append_message` increments them); write
`schema_version` = 22 or let Hermes migrate (`_reconcile_columns` adds
missing columns on open, `hermes_state.py:1509`).

## Open questions

1. Role vocabulary and `content` JSON for multipart/image messages
   (`_decode_content`).
2. Which `tool_calls` shape the CLI path produces in practice (SDK
   objects → no `id`).
3. Hermes version is not recorded per session; where to get
   `ProducerInfo.version`.
4. Whether `--oneshot` persists a session (inferred yes).
5. Per-turn token usage: `messages.token_count` only.

Needs credentials to go further: a provider key (OpenRouter/Anthropic/…)
and a working `pip`/`uv` install, for one `hermes chat -q` run to observe
rows. Everything above is derivable from source without either.

## References

- Sdist: <https://pypi.org/project/hermes-agent/0.19.0/>
- `hermes_state.py` (schema, `append_message`, `get_messages`,
  `resolve_session_id`), `hermes_cli/_parser.py` (flags), `cli.py`
  (session id, resume), `run_agent.py` (`create_session`, `tool_calls`
  serialisation), `hermes_constants.py` (`HERMES_HOME`).
- Docs: <https://hermes-agent.nousresearch.com/docs/>
