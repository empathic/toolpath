# fix(opencode): projected ids use opencode's own shape, so Zen's free tier accepts resumed sessions

Status: ready · Base: pr/08-resume-opencode · Independent of the other resume fixes

Commits: `fix(opencode): …` (5ff0d26).

## Why
OpenCode Zen's free tier (opencode/big-pickle) answered native sessions and returned 403 FreeTierError on every toolpath-projected one. Bisected by pointing `provider.opencode.options.baseURL` at a local server (zero Zen calls) and replaying with curl: the gate validates only the shape of the `x-opencode-session` header, opencode's id format (`ses_` + 12 hex + 14 base62). toolpath minted `ses_` + 24 hex, so every projected session was refused. Body content (foreign context turns, `version 0.0.0-projected`, the model stamp) is not checked.

## What changes
`crates/toolpath-opencode/src/project.rs`: one `mint_id` writes opencode's shape for session, message and part ids (time·4096+counter in the low 48 bits, a SHA-1-derived base62 tail; deterministic; part ids now monotonic). Three unit tests; "Projected IDs" subsection in the format doc.

## Verification
61 tests pass, fmt and clippy clean, `goldens check` 46 ok. End to end: codex → opencode headless on the free model, rc 0, the probe answered with the source's first message and END (`review/opencode-free-tier.md`).

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
crates/toolpath-opencode/src/project.rs
```
