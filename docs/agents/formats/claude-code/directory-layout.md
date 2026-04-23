# `~/.claude/` directory layout

Everything Claude Code persists to disk lives under `~/.claude/` (on
Windows, `%USERPROFILE%\.claude\`). The directory is created on first
run and populated lazily as features are used.

## The tree

```
~/.claude/
├── projects/                  # per-project session storage — the main artifact
│   └── <sanitized-cwd>/
│       ├── <session-uuid>.jsonl
│       ├── <session-uuid>.jsonl
│       └── …
├── history.jsonl              # global user-prompt history (separate format)
├── settings.json              # user-level config (permissions, hooks, etc.)
├── todos/                     # per-session TodoWrite state (legacy; see peripheral-files.md)
├── shell-snapshots/           # zsh/bash env dumps captured at session start
├── file-history/              # content-addressed file backups for undo/rollback
├── session-env/               # per-session env-var snapshots
├── plans/                     # plan-mode artifacts
├── sessions/                  # session cache / index
├── statsig/                   # Anthropic feature-flag cache
├── plugins/                   # installed plugins
├── debug/                     # per-session debug logs
├── paste-cache/               # clipboard history
├── backups/                   # backups of ~/.claude.json
└── ide/                       # per-IDE lock files (e.g. VS Code attach points)
```

Only `projects/` is necessary for reconstructing a conversation. The
rest is supporting state; see [peripheral-files.md](peripheral-files.md)
for the bits that matter.

## `projects/` directory naming

Each project directory is named after the **resolved absolute path** of
the working directory in which Claude Code was invoked, with `/` and
`_` characters replaced by `-`.

| Original cwd                                      | Directory name                         |
|---------------------------------------------------|----------------------------------------|
| `/Users/alex/Devel/empathic/toolpath`             | `-Users-alex-Devel-empathic-toolpath`  |
| `/Users/alex/my_project`                          | `-Users-alex-my-project`               |
| `/home/bob/code`                                  | `-home-bob-code`                       |

### Sanitization is lossy

Both `/` and `_` map to `-`, so you cannot round-trip perfectly. Any
tool unsanitizing a directory name can only guess where path separators
were. For the typical case (no underscores in directory names) it works.

### Path canonicalization before sanitization

Claude Code resolves the cwd to a canonical path before sanitizing. On
macOS in particular, `/tmp` is a symlink to `/private/tmp`:

| Launched in         | Resolves to            | Directory name       |
|---------------------|------------------------|----------------------|
| `/tmp/foo`          | `/private/tmp/foo`     | `-private-tmp-foo`   |

If you're building test fixtures or synthesizing directory paths, you
must canonicalize first. A tool that writes to `-tmp-foo/` will not
show up when Claude Code is launched in `/tmp/foo`.

### Session files

Each conversation is a single JSONL file named `<session-uuid>.jsonl`
where `session-uuid` is a UUIDv4. A single *logical* conversation can
span multiple files if Claude Code rotated mid-session — see
[session-chains.md](session-chains.md).

## `~/.claude.json`

Distinct from `~/.claude/` (note the missing dot-dir). This is a single
JSON file at `$HOME/.claude.json` holding OAuth tokens, per-project
state (MCP servers, allowed tools, `lastSessionId`, trust-dialog state),
and other cross-session settings. Backups of it land in
`~/.claude/backups/`.
