# Peripheral files

Everything under `~/.claude/` that isn't a session JSONL. Most of these
are supporting state that a conversation reader can ignore, but they
matter if you're building anything broader than a transcript viewer.

## `~/.claude/history.jsonl`

Global user-prompt history, across all projects. JSONL, same line
framing as sessions.

### Line shape

```json
{
  "display": "The following fails with an error: bazel build --compilation_mode opt //packages/galaxy/...",
  "pastedContents": {},
  "timestamp": 1759167114955,
  "project": "/Users/alex/Devel/empathic/empathic",
  "sessionId": "..."
}
```

Fields:
- **`display`** — user's prompt text as shown in the UI.
- **`pastedContents`** — object mapping paste-slot IDs to pasted
  content (e.g. files the user dragged in). Usually empty.
- **`timestamp`** — Unix milliseconds. Note: JSONL sessions use
  ISO-8601; `history.jsonl` uses epoch ms.
- **`project`** — absolute workspace path (the pre-sanitization form).
- **`sessionId`** — session the prompt was submitted in. Absent in
  older entries; treat as optional.

Unlike session JSONLs, `history.jsonl` is not subject to the 30-day
cleanup — prompts are kept indefinitely. Users can opt out with
`CLAUDE_CODE_SKIP_PROMPT_HISTORY=1`.

## `~/.claude/settings.json`

User-level config. JSON, not JSONL. Holds permissions, hooks,
attribution settings, `statusLine` config, env vars. Project-local
overrides live at `<project>/.claude/settings.json` and
`.claude/settings.local.json`.

Not covered in detail here — see Claude Code's own settings docs when
writing a tool that consumes this file.

## `~/.claude.json`

(Note: file in `$HOME`, not under `~/.claude/`.) A single JSON file
holding:

- OAuth tokens and account state.
- Per-project state: `mcpServers`, `allowedTools`,
  `hasTrustDialogAccepted`, `lastSessionId`, `projectOnboardingSeen`.
- Cross-session preferences.

Backed up automatically to `~/.claude/backups/.claude.json.backup.<unix-millis>`
on changes.

## `~/.claude/todos/`

Per-session TodoWrite state. **Marked legacy** by Anthropic — current
Claude Code versions no longer write here; TodoWrite state is inlined
into the session's tool-result entries instead. Safe to delete.

Filename convention: `<sessionId>-agent-<agentId>.json`.

Content: a JSON array of todo objects.

```json
[
  {"content": "Design a combined stream…", "status": "completed", "priority": "high", "id": "1"},
  {"content": "Wire it to handle_connection", "status": "pending", "priority": "medium", "id": "2"}
]
```

## `~/.claude/shell-snapshots/`

Captured shell environment at session start. One file per snapshot.

Filename: `snapshot-<shell>-<unix-millis>-<random>.sh`, e.g.
`snapshot-zsh-1759194893070-ab12cd.sh`.

Content: a shell script that recreates the user's environment —
function definitions, aliases, exported variables. Claude Code sources
these when running `Bash` tool commands so they operate in the same
environment the user would see.

## `~/.claude/file-history/`

Content-addressed file backups supporting undo/rollback. When Claude
Code edits a file, it writes the pre-edit content here before applying
the change.

Structure: `<session-uuid>/<contentHash>@v<versionNumber>`.

The `trackedFileBackups` object on `file-history-snapshot` entries
references entries here by (contentHash, versionNumber).

## `~/.claude/session-env/`

Per-session environment-variable snapshots. Typically one empty
directory per session (the dir itself is a presence marker).

## `~/.claude/plans/`

Plan-mode artifacts. When a user enters plan mode, the plan drafts
Claude produces land here.

## `~/.claude/sessions/`

A session cache / index. In some versions, contains a
`sessions-index.json` with a global view of all sessions across
projects. Fields per entry:

```
{version: 1, sessions: [
  {sessionId, fullPath, fileMtime, firstPrompt, messageCount,
   created, modified, gitBranch, projectPath, isSidechain}
]}
```

Can be stale or missing. Treat it as a hint, not ground truth — rescan
`projects/` if precision matters.

## `~/.claude/statsig/`

Anthropic's feature-flag cache. Fixed filenames:

- `statsig.cached.evaluations.<id>`
- `statsig.session_id.<id>`
- `statsig.stable_id.<id>`

Not useful to external tools.

## `~/.claude/plugins/`

Installed plugins directory. `plugins/blocklist.json` lists plugins
Anthropic has flagged:

```json
{
  "fetchedAt": "...",
  "plugins": [{"plugin": "...", "added_at": "...", "reason": "...", "text": "..."}]
}
```

## `~/.claude/debug/`

Per-session debug logs. Text files, one per session. Verbose logging
of what Claude Code did internally. Useful for investigating bugs.

## `~/.claude/paste-cache/`

Clipboard / paste history. Attachments pasted into Claude Code land
here. Referenced by paste-slot IDs inside `history.jsonl`'s
`pastedContents`.

## `~/.claude/image-cache/`

Cached images from pasted screenshots, URLs, etc.

## `~/.claude/ide/`

Per-IDE lock files. When Claude Code runs in an IDE extension (VS Code,
JetBrains), each active IDE process creates a `.lock` file here to
coordinate.

## `~/.claude/backups/`, `~/.claude/cache/`, `~/.claude/chrome/`

Internal state directories, generally not interesting to external
tools.

## `stats-cache.json`

File at `~/.claude/stats-cache.json`. Cached usage statistics for the
CLI's stats commands. Full schema varies; not commonly consumed
externally.

## Cleanup / retention

Session files and most peripheral state are cleaned up after
`cleanupPeriodDays` days (default 30). `history.jsonl` is not on that
schedule. Plugins, settings, and OAuth state are never cleaned up.
