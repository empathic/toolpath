# test(goldens): cover all seven harnesses, pin schema validation, assert the accounting contract

Status: opened #367 (draft, 2026-10-16) — was: ready · Base: pr/07-insta-goldens · Independent of the resume fixes

Commits: `test(goldens): cover all seven harnesses, pin validate, assert the accounting contract` (78b4e80).

## Why
toolpath.net lists seven harnesses as Read and Write; the insta goldens pinned four. The kind spec (agent-coding-session v1.1.0) states an accounting contract (usage on exactly one step per group, Σ steps = session totals) that no test asserted, which is how `breakdowns` was lost silently; and `path p validate` was never run over the goldens' documents.

## What changes
- `Agent` has seven variants. Fixture-backed sets read `test-fixtures/<agent>/convo.*` directly (one fixture location; `goldens/` keeps only the live capture and the derived compacted set). 9 sets × 6 targets: 54 projections and 54 round trips, every pair compared. Cursor's projection is pinned with sorted keys (its flattened extra maps reorder per run).
- `validate@<set>`: base schema plus the bundled kind schemas over each document and every round trip. All valid.
- The accounting invariant, asserted per set with the differing field named: per-step usage (four counters and breakdowns, class by class) equals the IR total from the provider and the total extracted back. Holds for six sets; three real gaps are pinned as known defects: copilot (input and cache totals exist only on `session.shutdown`, which has no step), gemini and opencode (their provider-side `sum_usage`/`accumulate_total` drop per-turn breakdowns, the twin of `c65a6b6`).
- Five drifted qualifier anchors re-recorded; four new concerns for the gaps above.

## Verification
37 unit tests, the glob and the qualifier test green; fmt and clippy clean; the old `path goldens check` still 46 ok. Not covered: live captures (no gemini driver; cursor and opencode refuse) and `--project` placement writes. Report: `review/goldens-coverage.md`.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
"crates/toolpath-goldens/snapshots/document@cursor.snap"
"crates/toolpath-goldens/snapshots/document@gemini.snap"
"crates/toolpath-goldens/snapshots/document@opencode.snap"
"crates/toolpath-goldens/snapshots/ir@cursor.snap"
"crates/toolpath-goldens/snapshots/ir@gemini.snap"
"crates/toolpath-goldens/snapshots/ir@opencode.snap"
"crates/toolpath-goldens/snapshots/known_defects@cursor.snap"
"crates/toolpath-goldens/snapshots/known_defects@gemini.snap"
"crates/toolpath-goldens/snapshots/known_defects@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@claude-compacted@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@claude-session@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@claude@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@claude@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@claude@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@codex@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@codex@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@codex@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@copilot@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@claude.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@codex.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@cursor@pi.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@claude.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@codex.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@opencode.snap"
"crates/toolpath-goldens/snapshots/projection@gemini@pi.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@claude.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@codex.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@copilot.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@opencode@pi.snap"
"crates/toolpath-goldens/snapshots/projection@pi@cursor.snap"
"crates/toolpath-goldens/snapshots/projection@pi@gemini.snap"
"crates/toolpath-goldens/snapshots/projection@pi@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-compacted@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude-session@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@claude@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@codex@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@copilot@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@cursor@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@opencode.snap"
"crates/toolpath-goldens/snapshots/round_trip@gemini@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@claude.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@codex.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@copilot.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@opencode@pi.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@cursor.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@gemini.snap"
"crates/toolpath-goldens/snapshots/round_trip@pi@opencode.snap"
"crates/toolpath-goldens/snapshots/shed@cursor.snap"
"crates/toolpath-goldens/snapshots/shed@gemini.snap"
"crates/toolpath-goldens/snapshots/shed@opencode.snap"
"crates/toolpath-goldens/snapshots/validate@claude-compacted.snap"
"crates/toolpath-goldens/snapshots/validate@claude-session.snap"
"crates/toolpath-goldens/snapshots/validate@claude.snap"
"crates/toolpath-goldens/snapshots/validate@codex.snap"
"crates/toolpath-goldens/snapshots/validate@copilot.snap"
"crates/toolpath-goldens/snapshots/validate@cursor.snap"
"crates/toolpath-goldens/snapshots/validate@gemini.snap"
"crates/toolpath-goldens/snapshots/validate@opencode.snap"
"crates/toolpath-goldens/snapshots/validate@pi.snap"
crates/toolpath-goldens/src/accounting.rs
crates/toolpath-goldens/src/validate.rs
```
