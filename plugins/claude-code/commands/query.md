---
description: Query your local agent-session history
argument-hint: "question"
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh:*)
---

## Context

- Toolpath CLI: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh"`

## Your task

Answer the user's question by querying the local Toolpath cache — the on-disk archive of their agent coding sessions (Claude Code, Gemini CLI, Codex, Copilot, opencode, Cursor, Pi) and derived git/GitHub history. Translate plain English into a jaq (jq-compatible) filter; run a filter verbatim if the user already wrote one.

User arguments: $ARGUMENTS

Always invoke the CLI through the wrapper (it resolves or installs the binary regardless of PATH):

```
"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" exec query [--source <s>] [--project <dir>] [-r|-c] '<filter>'
```

### Data model

`query` flattens every step of every cached document into one JSON array; the filter runs over that array. Each element wraps one step:

```json
{
  "cache_id": "claude-path-claude-code-6987afe8",
  "step":     { "id": "...", "actor": "agent:claude-opus-5", "timestamp": "2026-07-29T15:44:20.239Z" },
  "change":   [ { "artifact": null, "structural": { "type": "conversation.append", "role": "user", "text": "..." } } ],
  "dead_end": false,
  "path":     { "id": "...", "base": { }, "meta": { "source": "claude-code" } }
}
```

- `step.actor` is `human:<name>`, `agent:<model>` (e.g. `agent:claude-opus-5`, `agent:gpt-5.2-codex`), or `tool:<harness>` (e.g. `tool:claude-code`); to filter by harness use `path.meta.source` or `--source`, not the actor.
- `dead_end` marks steps not on the ancestry of the path head (abandoned work).
- Before a filter references structural fields, look up the schema: `exec kind` lists the bundled kinds, `exec kind agent-coding-session` prints the field reference for agent sessions. Never guess field names — a wrong guess wastes a query, and index errors can dump large values into the output.

### Scoping and freshness

- `--source claude|gemini|codex|copilot|opencode|cursor|pi|git|github` narrows by harness, `--project <dir>` by project, `--kind <selector>` by path kind, `--id <cache-id>` by document; `--input <file>` queries a file without touching the cache.
- `-r` prints raw strings (like `jq -r`); `-c` forces compact output.
- path-cli 0.16+ auto-syncs the queried scope from the installed harnesses before running. On older versions, if results look empty or stale, fill the cache first with `exec p cache sync` (0.16+) or `exec p import claude --project <absolute cwd> --force`, and inspect it with `exec p cache ls`. Always write `--project` as a literal absolute path — `$PWD` fails the permission check, and relative paths match nothing.

### Token discipline

Query output lands in your context — spend it deliberately:

- **Aggregate before you enumerate**: prefer counts, maxes, and `group_by` over listing steps; list rows only after an aggregate shows the interesting rows are few.
- **Project to scalars**: never emit raw `text`/`thinking`/`before`/`after` — use `length` or `.[0:200]` slices, and cap every list with `.[:10]`.
- **One call per question**: combine projections over the same selection into one object rather than querying the same steps twice.
- **Repeat queries**: the first query already synced the scope — pass `--no-sync` on subsequent queries (0.16+) to skip the re-sync and its progress lines.

### Example filters

```bash
# ids of abandoned (dead-end) steps
'map(select(.dead_end)) | map(.step.id)'

# sessions where a user prompt mentions "tailscale"
'[.[] | select(any(.change[]?.structural;
    .type == "conversation.append" and .role == "user"
    and ((.text // "") | test("tailscale"; "i"))))
  | .cache_id] | unique'

# steps that burned >50k input tokens in one message
'map(select(any(.change[]?.structural.token_usage; .input_tokens > 50000)))'

# step count per source document, largest first
'group_by(.cache_id) | map({id: .[0].cache_id, steps: length}) | sort_by(-.steps)'

# agent (vs. human) steps this month
'map(select((.step.actor | startswith("agent:")) and .step.timestamp > "2026-07")) | length'

# top 10 steps by input tokens
'[.[] | {id: .step.id, doc: .cache_id, u: (first(.change[]?.structural.token_usage // empty) // null)}
  | select(.u)] | sort_by(-(.u.input_tokens // 0)) | .[:10]
  | map({id, doc, in: .u.input_tokens, out: .u.output_tokens})'

# per-session token totals, biggest output first
'group_by(.cache_id) | map({doc: .[0].cache_id,
  in: ([.[] | .change[]?.structural.token_usage.input_tokens // 0] | add),
  out: ([.[] | .change[]?.structural.token_usage.output_tokens // 0] | add)}) | sort_by(-.out) | .[:10]'
```

### Report

Answer the question, and only the question.

- Interpret the results in prose with the values the user asked for — never dump raw JSON.
- Say nothing about how the answer was produced: no mention of the cache, Toolpath, jaq/jq, filters, syncing, or schema lookups. Refer to the data as the user's sessions or session history — "the Toolpath cache" means nothing to them.
- Don't show the filter you ran. The only exceptions: the user asked how, or they supplied a filter themselves and you had to correct it.
- If a filter errors, fix it and retry silently; never report the syntax error.
- Caveats about the data itself (e.g. token usage is recorded per message, not per tool call) are welcome when they change how the answer should be read.
