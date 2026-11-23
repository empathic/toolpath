# docs(baseline): one pinning model, flake-shaped agents.lock, and isolation in CI (design, no code)

Status: draft · Base: pr/91 · Design record only; Bobby's brief 2026-10-21 (fd32432a), refined the same day (pinned downloads without requiring nix; isolation in GitHub Actions)

## Why (plain words)
Recordings, live checks and schemas all depend on which agent binary produced them, and today nothing pins that: capture uses whatever `claude`/`codex`/… is installed, cassettes record `harness: null`, and the manifest only repeats the version a recording declares. One model ties agent binaries, vendor schemas, our kind and contracts, and provenance together so a newer vendor version is a drift report, never a silent change.

## What changes
- `docs/baseline/pinning.md` (new): pinned inputs (an `agents.lock` per agent and platform: version, source URL, sha256, signing identity), derivations keyed by input hash (per-version session-file schemas, Pathbase OpenAPI), our specs (the kind, writer contracts), per-recording provenance, what fails the build vs what is a drift report, and how a pin is bumped.
- Isolation in GitHub Actions: one agent per job in its own `live-<agent>` environment, GitHub-hosted ephemeral runners only for third-party binaries, `permissions: {}`, SHA-pinned actions, an egress allowlist, and the API proxy holding the real key as a separate user so the agent process never sees it. Where the live keys live in CI, and the run click as the spend cap.
- No code; nothing enforced yet.

## Verification
Docs only: qualifier review (0 drifted) and the secrets gate. Statements about today's state were read from this tree (pinning.md "What is pinned today").

## For the next PR
The `agents.lock` file and fetcher (download → verify hash/signature → content-addressed cache), then provenance in cassettes and the manifest for all seven agents.
