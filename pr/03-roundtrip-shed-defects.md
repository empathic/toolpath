# feat(goldens): round trips, shed reports and known defects

Status: not cut (the first-cut plan; see HOLD.md and README) — was: draft · Base: pr/02 · Stack 2/4

Commits: `feat(goldens): path goldens roundtrip (IR -> adapter -> IR per set)…` · `feat(goldens): path goldens shed [set]…` · `feat(goldens): check recomputes shed per set and fails on newly dropped key paths or lost text` · `feat(goldens): round-trip check fails on newly gained key paths` · `fix(goldens): known-defect check tells FIXED from changed-and-worse` (+ their repins, folded).

## Why
Byte pins say *that* an output moved. These say *what a conversation loses* through each adapter: which key paths and how many text characters survive IR → X → IR, per set and per adapter, so a loss is a number someone declared, not a surprise.

## What changes
- `path goldens roundtrip`: one row per (set, adapter): key paths lost and gained; `check` fails on a new loss or gain.
- `path goldens shed [set]`: per-stage bytes/chars/types, same-format round trip, dropped fields with examples, renames; recomputed by `check`.
- `goldens/known-defect/*.tsv`: three projector defects pinned as regression tests (codex→claude foreign entry types; `export codex -o` writes the caller's cwd; `message.stop_reason` → `stopReason`). A fix fails `check` with `looks FIXED` so the pin is recaptured on purpose; any other change fails as `CHANGED and is NOT fixed`.

## Verification
`check`: 46 ok. Projecting `stop_reason: None` in `toolpath-claude/src/project.rs` fails with `known defect … CHANGED and is NOT fixed`.
