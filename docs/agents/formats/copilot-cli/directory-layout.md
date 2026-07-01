# Directory layout

The Copilot CLI keeps **all** of its configuration and session data under a
single root: `~/.copilot/` `[official]`
([config-dir reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)).
There is no XDG split — settings, state, and history all live in one place.

## Root and overrides

| Variable | Effect | Source |
|---|---|---|
| `COPILOT_HOME` | Replaces the entire `~/.copilot` root path. | `[official]` |
| `COPILOT_CACHE_HOME` | Relocates the cache separately from the rest. | `[official]` |

We found **no evidence** of `XDG_CONFIG_HOME` / `~/.config/github-copilot/`
support `[unverified]` — the docs only mention `~/.copilot` and the two
variables above. A `toolpath-copilot` path resolver should honor `COPILOT_HOME`
first, then fall back to `$HOME/.copilot`.

## Top-level inventory

Everything below is `[official]` from the config-directory reference unless
noted. Formats in the third column.

| Item | Kind | Format | Purpose |
|---|---|---|---|
| `config.json` | file | JSON | Automatically managed application state — authentication, installed plugins, other internal data. |
| `settings.json` | file | **JSONC** | Primary user-editable configuration. (Comments allowed — a strict JSON parser will choke.) |
| `copilot-instructions.md` | file | Markdown | Personal custom instructions applied to all sessions. |
| `lsp-config.json` | file | JSON | LSP servers configured at the user level. |
| `mcp-config.json` | file | JSON | MCP servers available at the user level. |
| `permissions-config.json` | file | JSON | Saved tool/directory permission decisions, organized by project location. |
| `session-store.db` (+ `-shm`, `-wal`) | file | **SQLite (WAL)** | Cross-session index — checkpoint indexing and full-text search. Opened in WAL mode, so the `-shm`/`-wal` sidecars appear. See [session-store-db.md](session-store-db.md). |
| `session-state/` | dir | — | Session history, one subdirectory per session ID. See [session-state.md](session-state.md). |
| `command-history-state.json` | file | JSON | `[observed, 1.0.67]` Reverse-search (Ctrl+R) command history — a **file** (the config-dir reference calls it a directory; the real install writes `command-history-state.json`). Managed automatically. |
| `logs/` | dir | — | Per-session log files (default target of `--log-dir`). |
| `agents/` | dir | `*.agent.md` | Personal custom agents. |
| `skills/` | dir | `SKILL.md` | Personal custom skills. |
| `instructions/` | dir | `*.instructions.md` | Additional instruction files. |
| `extensions/`, `hooks/`, `installed-plugins/`, `plugin-data/` | dirs | — | Extensions, hooks, plugins, and their data. |
| `ide/` | dir | — | Lock files and state for IDE integrations. |
| `mcp-oauth-config/` | dir | — | OAuth tokens for MCP servers. |
| `mcp-secrets/` | dir | — | Secret-placeholder fallback storage for MCP. |

> **`[observed, 1.0.67]`** A fresh install after one session held only
> `config.json`, `command-history-state.json`, `session-store.db` (+ WAL
> sidecars), and the `ide/`, `logs/`, `session-state/` directories. The
> `settings.json` / `mcp-config.json` / `permissions-config.json` /
> `copilot-instructions.md` / `agents/` / `skills/` entries are **created on
> first use**, so don't assume they exist.

### What a derive crate cares about

Of the above, a forward provider (native → `Path`) only needs:

- **`session-state/<id>/`** — the per-session source of truth (events,
  workspace metadata, checkpoints). This is where derivation reads.
- **`session-store.db`** — useful as a *discovery* index (list sessions, get
  auto-generated summaries and repo/branch without parsing every
  `events.jsonl`), but not the source of truth for content.

`config.json`, `settings.json`, `mcp-*`, `permissions-config.json`, and the
`agents`/`skills`/`hooks`/`extensions` trees are configuration, not
conversation; a provider can ignore them. They matter only if we later add an
`export`/projector path that wants to write a resume-ready layout.

## Legacy layout migration

`[reverse-eng, Medium]` Older sessions were stored under
`~/.copilot/history-session-state/` and are **auto-migrated** into
`session-state/` when resumed. The exact version where the new layout landed is
uncertain (one uncited summary put it around v0.0.342 `[unverified]`). A robust
discovery routine should glance at `history-session-state/` as a secondary
location if `session-state/` comes up empty.
