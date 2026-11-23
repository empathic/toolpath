# feat(goldens): capture-live, one real headless session per agent, leak-checked

Status: not cut (the first-cut plan; see HOLD.md and README) — was: draft · Base: pr/03 · Stack 3/4

Commits: `feat(goldens): capture-claude-session.sh and an optional claude-session golden case` · `fix(goldens): capture script decodes a hex-encoded Keychain value…` · `fix(goldens): capture-claude leak check allows only the attribution address…` · `goldens: captured Claude session` · `feat(goldens): capture-live <claude|codex|pi|copilot> with per-agent drivers…` · `fix(goldens): capture-live redacts account identifiers instead of refusing them…`.

## Why
Fixtures of unknown origin pin what someone once wrote; a live capture pins what the agent writes today, with the agent's version recorded. `claude-session` is the Claude input to trust.

## What changes
- `path goldens capture-live <agent>`: runs the agent once in a temp HOME with a bare `ls`, credentials only ever copied into the temp dir, output leak-checked (refuses a real home path, a token or the local username; redacts account identifiers and lists them in the manifest) and captured as a set.
- Agent drivers for claude, codex, pi, copilot; cursor/opencode refused with the reason.

## Verification
`claude-session` captured with claude 2.1.285; `check`: 46 ok.

## Reviewer note
copilot runs with `--allow-all-tools` and pi unconstrained while a real token is in the environment; the claude driver scopes tools to `Bash(ls)`. Either scope the others the same way before merge or state why not.
