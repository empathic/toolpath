# Listing, resuming, and managing sessions

The surface a future projector / `path resume` integration must match, and the
identifier-resolution rules a library reader should mirror (see pitfalls #2/#7
in [`../adding-a-projector.md`](../adding-a-projector.md)). All `[official]`
([command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference))
unless noted.

## CLI flags

| Flag | Behavior |
|---|---|
| `--continue` | Resume the most recent session in the current working directory, falling back to the globally most recent session. |
| `-r, --resume[=VALUE]` | Resume via an interactive picker, or directly by **session ID**, **ID prefix**, or **session name**. |
| `--session-id ID` | Resume an exact session/task ID — **no** prefix or name matching. |
| `--connect[=SESSION-ID]` | Connect to a **remote** session (not a local `session-state/` dir). |
| `-n, --name=NAME` | Name a new session (auto-generated if omitted). |
| `--log-dir=DIRECTORY` | Where per-session logs go (default `~/.copilot/logs/`). |
| `--log-level=LEVEL` | Log verbosity. |

The resume picker offers relevance/created/name/last-used sorts, separate
**local vs. remote** tabs, and a delete action.

## Slash commands (interactive)

| Command | Behavior |
|---|---|
| `/resume [SESSION-ID]` | Open the picker, or resume a specific session. |
| `/continue [SESSION-ID]` | Continue most-recent / specified session. |
| `/session info` | Show current session metadata. |
| `/session checkpoints` | List checkpoints (rewind points). |
| `/session files` | List files touched (the `session_files` view). |
| `/session plan` | Show the working plan. |
| `/session rename` | Rename the session. |
| `/session cleanup` \| `prune` \| `delete` \| `delete-all` | Housekeeping on stored sessions. |
| `/rename [NAME]` | Rename the current session. |
| `/settings [show \| KEY VALUE \| reset KEY]` | Read/modify `settings.json`. |

## Identifier resolution (what `--resume` matches)

`--resume` accepts **three** forms — full session ID, ID **prefix**, and
**name** — while `--session-id` accepts only the exact ID. This is directly
analogous to `toolpath-codex`'s `PathResolver::find_rollout_file`, which already
resolves full stem / bare UUID / short prefix and detects ambiguity. A
`toolpath-copilot` resolver should:

1. Match an exact session-ID directory name under `session-state/` (and the
   legacy `history-session-state/` — see
   [directory-layout.md](directory-layout.md#legacy-layout-migration)).
2. Match a **unique prefix** of a session ID; error on ambiguity.
3. Match a **session name** — which requires the name→ID mapping. That mapping
   lives in [`session-store.db`](session-store-db.md)'s `sessions` table, **not**
   in the directory layout `[inferred]`, so name resolution implies reading the
   DB (or scanning each session's metadata).

**Library/CLI parity** (pitfall #7): whatever forms the future `path` CLI
advertises for `--session`, the `toolpath-copilot` library reader must resolve
the same way, so `path p export copilot … && copilot --resume <same-id>` round-
trips. Document any asymmetry if full parity isn't implemented.

## Remote sessions

`--connect` targets sessions that don't live under local `session-state/` at all
(they're the cloud/remote counterpart). A local-filesystem derivation can't see
these `[official]`; they'd be reachable only via GitHub's API, the same way the
cloud "Copilot coding agent" is (see
[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md)). Scope a first
`toolpath-copilot` to **local** sessions only.
