# feat: hash-verified test agents (path d agents, cargo xtask pins), cargo xtask act, replay CI, pathbase cassettes on main d874b09, docs/agents/support.toml

Status: draft · Base: pr/116 · Source: toolpath-author batch 2026-10-23 (9 commits + 1 landing fix; patch: pr/patches/117-agents-pins-act-pathbase-main-support-list.patch)

## Why
- Test agents must come from a lock, hash-verified, never from PATH or Homebrew (a Homebrew codex raised a Gatekeeper prompt on Bobby's screen during replay). Rust runs the pins (Bobby).
- Replay should run the same way locally and in CI: act with pinned images and a squid allowlist, plus a workflow_dispatch replay job.
- The pathbase cassettes follow pathbase main now ("we keep bumping"); they are no longer provisional.
- Bobby asked for a formal running list of supported agents.

## What changes
- `path d agents list|fetch|path`: blobs named by agents.lock, sha256-verified, cached by hash, never on PATH (no lock committed yet; nothing downloaded).
- `cargo xtask pins list|verify` (offline); fetch/bump refuse until sources are approved.
- `cargo xtask act` (dry run), `ci/act.lock` (every pin PENDING), squid allowlist, `just act` / `just replay`; `ci: replay workflow` (workflow_dispatch).
- pathbase cassettes re-recorded from pathbase main d874b09 (includes #468, #486); README says so; the provenance records the OpenAPI sha per version.
- pathbase-fetch tests use their own fake credentials in a temp config dir.
- `docs/agents/support.toml` + `cargo xtask agents` -> `docs/agents/SUPPORT.md`; a test fails when an entry disagrees with the repo.
- `mod agents` declared after `goldens` so the qualifier anchor doesn't move.

## Landing fix (scratch-main-82)
The batch as handed over failed two gates: `cargo fmt` (crates/xtask) and the pinned `path d --help` tree (help@d.snap lacked the new `agents` subcommand). Also gitleaks read the new `openapi_sha256_v1/_v2` provenance keys as API keys; the existing allowlist entry now covers the versioned names.

## Verification (scratch-main-82, nix develop)
fmt clean · clippy -D warnings clean · tests 2,290 passed, 0 failed · cargo doc -D warnings clean · secrets + shellcheck 2/2 · no_secrets 2 passed · qualifier 238 checked, 0 drifted · `just baseline` ok.
NOT run: an actual agent fetch (no lock entries yet), act end to end (dry run only), a live replay.
