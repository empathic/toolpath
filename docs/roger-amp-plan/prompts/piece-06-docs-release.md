# Piece 06 — docs-release

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. Docs/bookkeeping only — no live amp, no behavior changes.

## Context to load first

`CLAUDE.md` (§Versioning and release checklist — items 1–11 are the law); `roger-amp-plan/PLAN.md` (Piece 06 + Global Constraints); `roger-amp-plan/BUILD_LOG.md` (what actually shipped/verified — it decides whether hedges flip); `roger-amp-plan/adding-a-projector-delta.md` (the 30-claim refresh table); the copilot entries in `CHANGELOG.md`, `site/_data/crates.json`, `scripts/release.sh`; `docs/agents/adding-a-projector.md`.

**Assumed done:** 00–04 (05 optional — reflect reality).

## Scope

`CLAUDE.md` ×7 spots (crate tree, dep graph `(preview)`, preview prose block, cross-deps sentence, test-count line, provider bullet, "seven agent harnesses"→eight — **and fix the stale `Turn.extra` bullet**); `README.md` workspace list + TSV provider doc; `site/_data/crates.json` 6-key entry (role text with preview caveat); `site/pages/crates.md` diagram + cross-deps sentence; `scripts/release.sh` comment block + `_all_crates` + **both** tier-2b loops (publish and wait_for_index); `CHANGELOG.md` `## New provider: Amp (preview) — <date>` (coverage, sourcing posture, wiring, loader contract w/ verbatim errors, verification evidence); `scripts/capture-elicit-fixtures.sh` (`ALL_HARNESSES` + `drive_amp()` + dispatch case); refresh `docs/agents/adding-a-projector.md` from the delta table (fix stale paths/signatures, drop `Turn.extra` teaching, add share/resume/`Harness`/`ExecStrategy`/writer-contract/matrix sections, re-point the reference block at copilot+amp); flip preview hedges to `✅ Verified in amp <version>` in all 5 lockstep places **only for what BUILD_LOG proves** (including the stderr banner — don't repeat copilot's stale hedge). Versions: `toolpath-amp 0.1.0` in its Cargo.toml + workspace deps + crates.json; `path-cli` 0.16.0→0.17.0 + CHANGELOG.

## Definition of done

Every CLAUDE.md checklist item pointable in the diff; `shellcheck scripts/*.sh` + `bash -n scripts/release.sh` clean; `cd site && pnpm run build` green; `just ci` + workspace gates green; BUILD_LOG final entry with the release-readiness statement. Tag `amp-m6`. PR/push only on Roger's explicit word. Stop.
