# Piece 01 — derive-crate

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. Pure code + fixtures — no live `amp` runs needed.

## Context to load first

1. `CLAUDE.md`; `roger-amp-plan/PLAN.md` (Global Constraints, Piece 01); `roger-amp-plan/BUILD_LOG.md` (what piece 00 decided — especially the Q1 architecture fork and Q2 token classification; they parameterize this piece).
2. `docs/agents/formats/amp/` — the format truth. Cite it; trust nothing undocumented.
3. Reference implementations: `crates/toolpath-copilot/src/` (module-by-module template), `crates/toolpath-convo/src/{lib.rs,derive.rs}` (IR + shared deriver), `crates/toolpath-claude/src/provider.rs:531` / `crates/toolpath-codex/src/provider.rs:642-932` / `crates/toolpath-pi/src/provider.rs:178` (the three token patterns).

**Assumed done:** piece 00 (dossier + `test-fixtures/amp/convo.jsonl`). Tag `amp-m0` exists.

## Scope

Create `crates/toolpath-amp` (version 0.1.0, preview labeling) with `error/paths/reader/types/io/provider/derive` per PLAN.md Piece 01's Interfaces block — exact type/field names there are binding (later pieces consume them). `to_view`: linear parent chaining via a `push_linked` helper; tool results merged into originating turns; verbatim tool names + `tool_category` classification; token layer implements ONLY the pattern piece 00 classified, with `is_usage_zero → None` and the group-final placement rule. `derive.rs` wraps `toolpath_convo::derive_path` (title `"Amp session: <8ch>"`). Tests: unit per module (mirror copilot's counts as a floor), `tests/roundtrip.rs` on a synthetic fixture, `tests/real_fixture_roundtrip.rs` on the real capture (forward counts, valid single-Path graph, wire serde value-identity per line), and a kind-conformance test validating a maximal derived path against `crates/path-cli/kinds/agent-coding-session/v1.1.0/schema.json`. Then the minimal CLI import seam exactly as PLAN.md Piece 01 lists (ArtifactType/Harness/bundle/derive_amp_session/ImportSource + picker), bumping the two fixed-size arrays.

TDD: for each module write the failing test from the fixture first, run it (`cargo test -p toolpath-amp <name>` — expect FAIL), implement minimally, re-run to PASS, commit small (Conventional Commits).

### Out of scope

list/show/share wiring (02), projector/export/resume (03), docs-site/release bookkeeping (06). No new deps beyond the copilot set (+`rusqlite` only if the dossier found SQLite).

## Definition of done

`cargo test -p toolpath-amp` green; `cargo run -p path-cli -- p import amp --session <captured-id>` → cache file `amp-path-amp-…`; `p validate --input <it>` passes; `p import amp --session <id> --no-cache --force | p render md --input - --detail full` shows the feature-elicit beats; `cargo build/test/clippy --workspace -- -D warnings` + `just ci` green. Append the Piece 01 BUILD_LOG entry (ADR: token-pattern implementation choice + any dossier corrections — update the format docs in the same commit if the fixture taught something new). Tag `amp-m1`. **Stop.**
