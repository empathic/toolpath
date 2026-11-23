# test(goldens): toolpath-goldens, the goldens as insta snapshots beside `path goldens`

Status: opened #362 (draft, 2026-10-16) — was: ready (Bobby chose the insta shape as the long-term goldens tool, 2026-10-15) · Base: pr/02-goldens-core's inputs · Independent of the resume fixes

Commits: `test(goldens): toolpath-goldens, the goldens as insta snapshots beside path goldens`.

## Why
`path goldens` is 3.8k lines inside the shipped binary doing five jobs over untyped JSON (hermetic child, manifest pins, key-path diff, report formatting, qualifier gate). The half that is "compare bytes to a committed file, show a diff, accept on purpose" is what `insta` already does (it is already a `path-cli` dev-dependency); the rest is a few pure functions. Same regressions caught, under half the code, standard tooling, small files that make by-feature PRs cuttable, and a dev crate instead of `path-cli` (the ownership finding).

## What changes
- `crates/toolpath-goldens` (publish = false): `model.rs` (`Agent`, `Set`, `Stages`, `RoundTrip`, `Shed`, `KnownDefect`, `StageError`), `pipeline.rs` (`run(set, scratch)`: read → ir → derive → project to the other three agents through the same projectors path-cli uses → round trips → same-format round trip → shed → known defects; no child process, no manifest, no git), `keypaths.rs`, `losses.rs`, `shed.rs`, `defects.rs` (the three TSVs become `known_defects@<set>.snap`), `canonical.rs` (copilot's sorted keys + masked session id), `leaks.rs` (planted-needle control).
- `tests/goldens.rs`: `insta::glob!` over `goldens/*/input.jsonl`, one TempDir HOME, leak scan, `set_sort_maps`, header carries agent/adapter/input sha256. 60 snapshots: `ir`, `document`, `projection@set@agent`, `round_trip@set@agent`, `shed`, `known_defects`.
- `tests/qualifier.rs`: runs `qualifier review --format json` at the repo root and filters to the crate; fails on drifted/missing or zero records; skips only when the binary is absent and `TOOLPATH_GOLDENS_QUALIFIER` is unset. Five concerns recorded.
- `path goldens` and `goldens/manifest.json` are left in place for now; `capture-live` stays in path-cli (it needs the CLI). Retiring the old tool is the follow-up once this is agreed.

## Verification
24 unit + glob + qualifier tests green after one `INSTA_UPDATE=always` and three clean runs (~0.5 s against the old tool's ~40 s); fmt and clippy `-D warnings` clean. All three known defects and every shed number reproduce exactly; claude, codex and pi projections are byte-stable. Finding: copilot → claude round-trips here; the old tool pinned it as "cannot round-trip", which came from its hermetic placement step, not the adapter. Old 4051 lines (goldens.rs, tests, script) → 1775.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
glob:"crates/toolpath-goldens/tests/**"
crates/toolpath-goldens/Cargo.toml
"crates/toolpath-goldens/snapshots/document@claude-compacted.snap"
"crates/toolpath-goldens/snapshots/document@claude-session.snap"
"crates/toolpath-goldens/snapshots/document@claude.snap"
"crates/toolpath-goldens/snapshots/document@codex.snap"
"crates/toolpath-goldens/snapshots/document@copilot.snap"
"crates/toolpath-goldens/snapshots/document@pi.snap"
"crates/toolpath-goldens/snapshots/ir@claude-compacted.snap"
"crates/toolpath-goldens/snapshots/ir@claude-session.snap"
"crates/toolpath-goldens/snapshots/ir@claude.snap"
"crates/toolpath-goldens/snapshots/ir@codex.snap"
"crates/toolpath-goldens/snapshots/ir@copilot.snap"
"crates/toolpath-goldens/snapshots/ir@pi.snap"
"crates/toolpath-goldens/snapshots/known_defects@claude-compacted.snap"
"crates/toolpath-goldens/snapshots/known_defects@claude-session.snap"
"crates/toolpath-goldens/snapshots/known_defects@claude.snap"
"crates/toolpath-goldens/snapshots/known_defects@codex.snap"
"crates/toolpath-goldens/snapshots/known_defects@copilot.snap"
"crates/toolpath-goldens/snapshots/known_defects@pi.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@codex.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@pi.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@codex.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@pi.snap"
"crates/toolpath-goldens/snapshots/projection@claude@codex.snap"
"crates/toolpath-goldens/snapshots/projection@claude@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@claude@pi.snap"
"crates/toolpath-goldens/snapshots/projection@codex@claude.snap"
"crates/toolpath-goldens/snapshots/projection@codex@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@codex@pi.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@claude.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@codex.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@pi.snap"
"crates/toolpath-goldens/snapshots/projection@pi@claude.snap"
"crates/toolpath-goldens/snapshots/projection@pi@codex.snap"
"crates/toolpath-goldens/snapshots/projection@pi@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@copilot.snap"
"crates/toolpath-goldens/snapshots/shed@claude-compacted.snap"
"crates/toolpath-goldens/snapshots/shed@claude-session.snap"
"crates/toolpath-goldens/snapshots/shed@claude.snap"
"crates/toolpath-goldens/snapshots/shed@codex.snap"
"crates/toolpath-goldens/snapshots/shed@copilot.snap"
"crates/toolpath-goldens/snapshots/shed@pi.snap"
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/src/canonical.rs
crates/toolpath-goldens/src/defects.rs
crates/toolpath-goldens/src/keypaths.rs
crates/toolpath-goldens/src/leaks.rs
crates/toolpath-goldens/src/lib.rs
crates/toolpath-goldens/src/losses.rs
crates/toolpath-goldens/src/model.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/src/shed.rs
```
