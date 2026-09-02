# Plugin: an MCP server and a session monitor

**Status:** Design proposal
**Date:** 2026-09-02

## Goal

Two additions to the Claude Code plugin (`plugins/claude-code`, plugin name
`path`), each a component type the plugin reference already defines:

1. **An MCP server**, so an agent drives toolpath through typed tools with
   enforced output caps rather than through a slash command that shells out
   and relies on prose to keep query output small. Bundled in the plugin's
   `.mcp.json`, it also makes toolpath usable from any MCP client, not only
   Claude Code.
2. **A monitor**, so Claude notices agent sessions landing on the machine
   without being asked to look. Declared in `monitors/monitors.json`, it runs
   a scoped `p cache sync` on a timer and emits one line per newly ingested
   session, which Claude Code delivers as a notification.

Both build on the CLI as it is. The plugin keeps bootstrapping the `path`
binary through `scripts/ensure-path.sh`; neither addition ships a second
binary.

## What exists

- The plugin is commands-only: `/path:share`, `/path:query`, `/path:resume`,
  `/path:link-pr`, plus `ensure-path.sh`, which resolves or installs the
  binary and carries `MIN_VERSION` (currently `0.15.0`), the oldest CLI the
  commands are written against.
- `/path:query` already embodies the tool contract this proposal formalises:
  the wrapped-step data model, the scoping flags, the "look up the kind schema
  before guessing a field" rule, and the token discipline (aggregate before
  enumerating, project to scalars, cap lists). Today all of that lives in the
  command's prompt and is enforced by nothing.
- The CLI has the right internal seams. `path-cli` is a library crate
  (`lib.rs`) whose `run()` dispatches to `cmd_query::run(QueryArgs, pretty,
  &Config)`, `cmd_show::run(ShowSource, ansi, &Config)`,
  `cmd_share::run(ShareArgs)`, and `cmd_cache::run(CacheOp, &Config)`. The
  sync engine takes a `SyncObserver` with `begin`/`tick`/`failed`/`end`, and
  its stat-level change detection means a no-op sync reads no session bodies.
- Nothing in the repo declares an MCP server or a monitor. The only MCP
  mentions are in `docs/agents/formats/`, describing MCP tool calls inside
  recorded sessions.

## Part 1: the MCP server

### Shape: a subcommand, not a crate

`path mcp` starts a stdio MCP server inside the existing binary. Reasons:

- The plugin already knows how to find and install `path`; a second binary
  would need its own release asset, checksum, and bootstrap path.
- The tool handlers call the same `cmd_*` entry points the CLI does, so the
  slash commands and the tools cannot drift apart in semantics. Where a
  `cmd_*::run` prints, it is split into a `_to_writer` (or a value-returning)
  form the CLI wraps, which is the same refactor `--no-cache` did for imports.
- Dependency: `rmcp` (the official Rust SDK, 3.x at the time of writing),
  behind a default-on `mcp` feature so the emscripten build keeps excluding
  it the way it excludes `share`, `resume`, and `auth`.

The server speaks stdio only. No HTTP transport in this proposal; the plugin
starts it per session and Claude Code owns its lifetime.

### Tools

Names are what a reader of `/path:query` already knows. Every tool returns
JSON text; every tool that can return a lot has a hard byte cap and reports
truncation explicitly rather than silently, which is the one thing a prompt
cannot promise.

| Tool | Arguments | Returns | Notes |
| --- | --- | --- | --- |
| `query` | `filter` (jaq, default `.`), `source?`, `id?[]`, `project?`, `project_under?`, `kind?`, `no_sync?`, `max_bytes?` (default 32 KiB, ceiling 256 KiB) | `{ "result": <json>, "truncated": bool, "bytes": n }` | Same planner and implicit scoped sync as `path query`. Output over the cap is cut at a JSON value boundary where possible and flagged. |
| `kinds` | `name?` | the kind list, or one kind's schema | Mirrors `path kind`. Cheap; the server's description tells the model to call it before writing a filter that touches structural fields. |
| `list_sessions` | `harness`, `project?`, `limit?` (default 20) | rows of `{ session_id, project, started, first_user_message }` | Mirrors `p list <provider> --format tsv`, as JSON. |
| `show` | `harness`, `session`, `project?`, `detail?` (`summary` default, `full`) | markdown | Mirrors `path show`. `full` is capped like `query`. |
| `cache_sync` | `types?[]`, `project_under?` | `{ synced: n, skipped: n, failed: [ { id, error } ] }` | Mirrors `p cache sync`. The observer collects instead of drawing. |
| `share` | `harness`, `session`, `project?`, `repo?`, `name?`, `public?`, `anon?`, `url?`, `timeout?` | `{ url, cache_id }` | Mirrors `path share` with the picker skipped (all three selectors required, as the slash command already insists). Annotated `openWorldHint: true`, `destructiveHint: false`, `idempotentHint: false`, so a client that gates on annotations gates this one. |

Deliberately absent:

- **`resume`.** It `execvp`s a harness over the current process; that is a
  terminal action, not a tool result. `/path:resume` keeps its current shape
  (import, export into the harness store, hand the user `/resume <id>`).
- **Anything that writes into a harness's on-disk store** (`p export
  <harness> --project`). Overwriting a live session file from a tool call is
  the failure `/path:resume` already guards against by hand.

### Resources

Two read-only resources, for clients that prefer resources to tools:

- `toolpath://kinds/<name>` — the kind's `schema.json`, as bundled for
  `p validate`.
- `toolpath://cache/<cache-id>` — one cached document, JSON, capped like
  `query`.

### Plugin wiring

`plugins/claude-code/.mcp.json`:

```json
{
  "mcpServers": {
    "toolpath": {
      "command": "${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh",
      "args": ["exec", "mcp"]
    }
  }
}
```

`ensure-path.sh exec` already resolves-or-installs and then execs the binary,
so the server inherits the bootstrap for free, including the
`MIN_VERSION` check, which must rise to the release that ships `path mcp`.
Tools surface to Claude as `mcp__plugin_path_toolpath__<tool>`; any hook in
the plugin that targets them must use that scoped name (bare server keys never
fire, per the reference).

The four slash commands stay. `/path:query`'s prompt shrinks to "use the
`query` tool; call `kinds` first", and its token-discipline section becomes
documentation of what the server enforces rather than instructions the model
has to remember. That is the concrete win: the caps move from prose to code.

### Versioning

`path-cli` bumps minor (a feature). `MIN_VERSION` in `ensure-path.sh` moves
to that release. Plugin `plugin.json` and the marketplace entry bump in
lockstep, as `CLAUDE.md` requires. `scripts/test-plugin.sh` gains a case
that starts the server over stdio, lists tools, and calls `kinds`.

## Part 2: the monitor

### What it watches

Monitors run "a shell command for the lifetime of the session and deliver
every stdout line to Claude as a notification". The natural line for
toolpath to emit is "a session landed": another harness finished a task,
another Claude window's session changed, a codex rollout appeared. The
sync engine already computes exactly that set, cheaply, on every run — the
stat gate skips unchanged artifacts without reading them — but it reports on
stderr in a form meant for a terminal.

### CLI change: `p cache sync --format lines`

A stdout format for machines, one line per artifact the sync derived or
re-derived, nothing for a no-op run:

```
synced claude claude-abc123 /Users/bobby/work/proj "fix the picker ordering"
synced codex codex-9f2e... /Users/bobby/work/proj "add a --format flag"
failed gemini gemini-... /Users/bobby/work/other: <error>
```

Fields: verb, artifact type, cache id, project directory (the doc's
`path.base`), and the first user message when the provider exposes it
(`ConversationMetadata.first_user_message`). Implemented as one more
`SyncObserver`: `Progress` draws, `Lines` prints. The summary that
`render_summary` produces stays on stderr, so a human running the same flag
still sees totals.

One more flag, `--exclude <cache-id>` (repeatable), so the monitor can leave
out the session it is running inside. Without it the monitor would report
its own transcript growing every interval, which is noise, not news.

### Declaration

`plugins/claude-code/monitors/monitors.json`:

```json
[
  {
    "name": "session-sync",
    "description": "Agent sessions landing on this machine (toolpath cache sync)",
    "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/session-monitor.sh",
    "when": "always"
  }
]
```

and `scripts/session-monitor.sh`:

```sh
#!/usr/bin/env bash
# Poll the toolpath cache into freshness and print one line per session that
# changed. Runs for the life of the Claude Code session; every stdout line is
# a notification, so print nothing in the steady state.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
self="$("$here/ensure-path.sh" current-session)"   # "unknown" outside Claude Code
exclude=()
[ "$self" != unknown ] && exclude=(--exclude "claude-$self")
while :; do
  "$here/ensure-path.sh" exec p cache sync --format lines \
    --project-under "${CLAUDE_PROJECT_DIR:-$PWD}" "${exclude[@]}" 2>/dev/null
  sleep 120
done
```

Scoped to the project subtree because that is what a session is likely to
care about, and because it keeps a machine with many projects from paying for
all of them in every window. The interval is a knob; two minutes is
conservative. The stat gate makes a quiet interval cost a manifest read and a
directory stat per harness, not a derive.

### What Claude does with a line

Nothing automatic. A notification that a codex session just landed in this
project is context: the next `/path:query` can reach it, `/path:share` can
publish it, and a question like "what did the other agent do" has an answer
to fetch. The monitor's `description` is what the task panel and the
notification summary show, so it names the mechanism plainly.

### Constraints carried over from the reference

- Monitors are experimental; the manifest key is `experimental.monitors` if
  it is declared inline, and `claude plugin validate` warns on the top-level
  spelling. Use the `monitors/monitors.json` file.
- They run unsandboxed, at the trust level of hooks, only in interactive CLI
  sessions, and are skipped where the Monitor tool is unavailable. The
  command is read-only over the machine's session stores and writes only to
  `~/.toolpath`, so that trust level is not being spent on anything new.
- `${user_config.*}` is rejected and `CLAUDE_PLUGIN_OPTION_*` is not passed
  to monitors. The interval and scope therefore live in the script, or in
  `~/.toolpath/config.toml` once `p cache sync` reads its interval from
  there — a follow-up, not part of this proposal.
- Disabling the plugin mid-session does not stop a running monitor, and a
  plugin update keeps the old script path until the session restarts. The
  script tolerates both: it only ever calls `ensure-path.sh` next to itself.
- Project-scope `@skills-dir` plugins do not load monitors. The marketplace
  install path does.

### Concurrency with the CLI's own syncs

`path query` auto-syncs its scope; `share` records provenance; the monitor
syncs on a timer. The manifest was built for this — advisory lock,
read-merge-save, checkpoints every ten writes, records unioned across
concurrent runs — so overlapping syncs cost duplicate derives at worst, never
lost records. Two Claude windows on the same project run two monitors and
converge on the same cache.

## Rollout

Three PRs, each independent and each landing something usable on its own:

1. **`p cache sync --format lines` and `--exclude`** in `path-cli`. A
   `Lines` observer, tests on the line format, and a fixture-driven check
   that a no-op sync prints nothing. Patch bump.
2. **`path mcp`** in `path-cli` behind the `mcp` feature: the six tools, two
   resources, output caps, and the `_to_writer` refactors of the `cmd_*`
   entry points it needs. Tests drive it through an `rmcp` client over
   in-process pipes and assert `query` matches `path query` byte for byte
   below the cap. Minor bump.
3. **Plugin**: `.mcp.json`, `monitors/monitors.json`, `session-monitor.sh`,
   the shrunk `/path:query` prompt, `MIN_VERSION`, plugin and marketplace
   version bumps, `test-plugin.sh` coverage, and the plugin README and
   `site/pages/plugin.md` updates.

A fourth, smaller PR follows on the docs side: `docs/agents/using-toolpath.md`
as the single page an agent reads to drive the CLI or the server, and an
`llms.txt` on the site that points at it. Today that knowledge is spread over
`CLAUDE.md`, the README, and the command prompts.

## Open questions

- **Cap semantics.** Cutting JSON at a value boundary is easy for arrays and
  hard for a single huge string. Proposal: arrays are truncated by element
  and strings by character, both flagged, and the tool description tells the
  model to project before it enumerates, as `/path:query` already does.
- **Where `share` authenticates.** The server runs with the user's
  `~/.toolpath/credentials.json`, the same as the CLI. A tool call that would
  fall through to anonymous upload should refuse rather than guess, matching
  the configured-remote rule in `share_config.rs`.
- **Monitor scope on a machine with no project.** Outside a checkout
  (`CLAUDE_PROJECT_DIR` unset, cwd somewhere generic) the subtree filter
  matches everything under `$PWD`, which for a home directory is every
  session. Either the script refuses to start there, or it narrows to the
  current harness only. Refusing is simpler and honest.
- **Whether the monitor should be `on-skill-invoke:query` instead of
  `always`.** Starting it lazily halves the number of idle syncs on a machine
  with many windows, at the cost of the unprompted awareness that is its
  reason to exist. Default to `always`; measure the cost.
