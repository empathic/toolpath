# Compaction in the baseline

Added 2026-10-21 at Bobby's direction. The four recordings are Ben Barber's (empathic/toolpath#108, `test-fixtures/<agent>/convo-compacted.*`): a
compaction FORCED in the real agent by his capture script, from the same 10-step session. They are copied unchanged from the PR's head
(`git fetch origin pull/108/head`); nothing was re-recorded and none of #108's code is here.

| set | recording | agent forced the compaction | targets |
|---|---|---|---|
| claude-forced | test-fixtures/claude/convo-compacted.jsonl | Claude Code | 7 |
| codex-forced | test-fixtures/codex/convo-compacted.jsonl | Codex | 7 |
| pi-forced | test-fixtures/pi/convo-compacted.jsonl | pi | 7 |
| opencode-forced | test-fixtures/opencode/convo-compacted.json | opencode | 7 |

Every stage is snapshotted for each (ir, document, projection and round trip into all 7 other agents, twice-around, validate, shed, known defects, formats):
28 round trips and 28 twice-around results, so the baseline is 13 recordings and 91 round trips.

## Row for the map's resume and project slides
**compaction: 4 recordings (claude, codex, pi, opencode forced), 28 round trips. Outcome: all text survives (12,194 / 17,497 / 4,900 / 5,740 characters,
identical after every round trip but one: pi -> opencode gains 2); the compaction itself does not: no reader has a compaction item yet, so the summary reads as
ordinary text and the markers (claude `compactMetadata`, codex's compacted item, ...) are source bookkeeping or unexplained paths. The opencode recording also meets
the old opencode parent bug (assistants hang off the first user turn: opencode -> pi cuts the chain, opencode -> claude leaves 22 entries unreached), pinned in
`UNTIL_108` (tests/goldens.rs) and `KNOWN` (toolpath-harness-mock); not caused by compaction.**

## What it shows today (not fixed ahead of #108)
- Classification (`classification.md`, `unexplained.md`): claude-forced loses 44 source-bookkeeping and 5-12 unexplained paths per pair; codex-forced 46 and 110-120
  (the compacted item's event fields); opencode-forced 0 and 0-9; pi-forced 0-5 declared and none unexplained.
- When #108 lands, these snapshots change: that diff is the proof of Ben's work, and `UNTIL_108` must be emptied.

## Kept both Claude compactions
`goldens/claude-compacted` (hand-made from `crates/toolpath-claude/tests/fixtures`) stays: it carries the old tool's pinned outputs (`goldens/claude-compacted/to-*.jsonl`
and the round-trip pins) and a small hand-built boundary; `claude-forced` is a real forced compaction and has no `to-*` pins. Replacing the first would drop those pins.

## Pending (not done, by instruction)
- The REAL compacted sessions (codex, copilot, pi `compacted-real`): need a privacy pass before they enter the repo.
- Copilot: no forced compaction (Copilot does not persist them).
- Live resume of compacted sessions: only on Bobby's go per run.
- `observed.rs` (the per-harness observed-behaviour docs) still lists only `convo.*` and `input.*` recordings, so the forced sets are not in those pages.

## Test changes this needed
- Adding the four recordings changes `path-cli`'s `query@site-example` snapshot (output tokens per actor across every fixture session); updated on purpose.
