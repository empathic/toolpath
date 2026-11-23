# feat(sync): CLAUDE_CONFIG_DIR may list several roots (cache sync)

Status: draft · Base: pr/73 · Code: Bobby's 2026-10-18 commit "feat(sync): support ':'-separated CLAUDE_CONFIG_DIR list" (local branch claude-multi-config-dir) cherry-picked unchanged as its own piece; this file is its record

## Why
`path query --source claude --project-under ~/.local/state/lobby-seats` returned nothing when `$CLAUDE_CONFIG_DIR` pointed at one account
and the sessions lived under another. Cache sync now reads every root in a `:`-separated list and stamps each session with its owner
(`path.meta.claude_config_dir`, `path.meta.account`).

## What
- `Config::claude_config_dirs()` splits on the platform path separator; empty entries dropped; unset yields none (the resolver defaults to `~/.claude`).
- `ClaudeSource` enumerates all managers and derives with the owning one.
- Single-session commands (derive, resume) still read only the first root; account discipline is the caller's (documented in providers.rs).
- Default unchanged: a value without `:` is exactly one root. Consultant pick 12: merge-tree against f308ab5 had no conflicts.

## Gates (sandbox, cargo 1.97.1) and not run
See the report that carries this commit; not run: pinned 1.94, gitleaks.
