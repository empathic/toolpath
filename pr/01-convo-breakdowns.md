# fix(convo): per-turn token breakdowns survive extract_conversation

Status: ready · Base: main · Independent (no stack)

Commits: `fix(convo): extract_conversation sums per-turn token breakdowns into total_usage` · `docs(convo): a session total's breakdowns are the per-turn breakdowns summed class by class`

## Why
`derive_path` writes each turn's `token_usage.breakdowns` (e.g. `output.reasoning`) into the step, and `extract_conversation` reads them back per turn, but it rebuilt `total_usage` from the four counters only. A Codex session's reasoning tokens were on every turn and on no total. Found by pathbase's goldens (`codex`: `$.total_usage.breakdowns` lost on IR → stored document → IR).

## What changes
- `toolpath-convo` `extract.rs`: the total's `breakdowns` are the per-turn breakdowns added class by class, alongside the counters. `output_tokens` is unchanged; breakdowns are still never summed into a counter (the doc comment on `TokenUsage::breakdowns` now says so in one sentence).
- Test `test_total_usage_breakdowns_round_trip`: two turns, 900/450 and 100/25, derive → extract → total 1000 with `output.reasoning` 475.

## Verification
`cargo test -p toolpath-convo`: 124 + 4 pass. `goldens check`: 46 ok (outputs byte-identical; this changes a value the goldens had pinned as lost in pathbase, not in toolpath).

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
crates/toolpath-convo/src/derive.rs
crates/toolpath-convo/src/extract.rs
crates/toolpath-convo/src/lib.rs
```
