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

## act approved and run locally (2026-10-26)
Source: Bobby 2026-10-26, "lets do it" on the consolidated deck's act thread (#150).
- `ci/act.lock` pins filled: act **0.2.89** (sha256 of `act_Darwin_arm64.tar.gz` from the release's
  `checksums.txt`; locally the same version from nixpkgs, built from source), runner image
  `ghcr.io/catthehacker/ubuntu` `rust-24.04` by index digest `sha256:1d8114fb…ae966` (rustup and
  cargo preinstalled, like GitHub's ubuntu-24.04). The egress allowlist stays a GUESS: no proxy yet.
- `cargo xtask act`: maps `ubuntu-24.04` (the label pr/118 pins), adds
  `--container-architecture linux/arm64` and `--container-daemon-socket -` (a job never gets the
  VM's Docker socket; colima's socket also cannot be bind-mounted, so act fails to start without it).
- `replay.yml`: installs Rust with `rustup show`, spells out `just replay`'s command (neither image
  ships `just`), passes the agent through the environment (no expression in the script), and sets
  `WEBMOCK_REQUIRE` plus `replay_zz_coverage` so a skipped replay fails instead of passing green.
- Verified on phobos's colima VM (aarch64, Ubuntu 24.04): `act workflow_dispatch -W replay.yml
  --input agent=pi` runs checkout, Rust and the replay step in the container. It now fails as it
  should: `required but not replayed: ["pi"]`, because no agent binary is in the image. That turns
  green with the hermetic harness install (pr/119). Before `WEBMOCK_REQUIRE` the same run passed by
  skipping. `cargo test -p xtask`: 6 passed. actionlint: clean.

## Landing fix (scratch-main-82)
The batch as handed over failed two gates: `cargo fmt` (crates/xtask) and the pinned `path d --help` tree (help@d.snap lacked the new `agents` subcommand). Also gitleaks read the new `openapi_sha256_v1/_v2` provenance keys as API keys; the existing allowlist entry now covers the versioned names.

## Verification (scratch-main-82, nix develop)
fmt clean · clippy -D warnings clean · tests 2,290 passed, 0 failed · cargo doc -D warnings clean · secrets + shellcheck 2/2 · no_secrets 2 passed · qualifier 238 checked, 0 drifted · `just baseline` ok.
NOT run: an actual agent fetch (no lock entries yet), act end to end (dry run only), a live replay.
