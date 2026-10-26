# feat(goldens): the coverage map, report mode (pr/78 step 1)

Status: draft · Base: fdc3da4 · Code: `classify.rs` (+2 unit tests), `tests/classification.rs`, generated `docs/baseline/classification.md`

## What
For each of the 63 recording x target round trips, count the lost and gained key paths that the TARGET writer's contract
(`fixtures/contract/<target>.json`) covers (declared) and those nothing explains (unexplained). Read from the committed `round_trip@*` snapshots;
the table is committed and a test fails when it is stale (`UPDATE_CLASSIFICATION=1` regenerates). Report mode: nothing fails on unexplained.

## Result (c. fdc3da4)
63 pairs (step 1, before source bookkeeping): lost 295 declared / 2,531 unexplained; gained 292 declared / 159 unexplained. This agrees with the measurement (2,540 of 2,854 lost; 175 of
472 gained, taken at c1e8cd9, before pr/76-77 and 81-83). Source-bookkeeping, minted and renamed have no declarations yet, so they are inside "unexplained".

## Next (pr/78 steps 2-4)
Declare source-bookkeeping once per source agent and reuse fixtures/twice-around.json for minted values; pair renames by leaf value; then fail on unexplained.
Defaults taken with controller-core: source-bookkeeping is an acceptable declared class; the table lives in docs/baseline/.

## Not run
Pinned 1.94, gitleaks, nix-develop recapture.

## Step 2a (source-bookkeeping), a second commit
`fixtures/source-bookkeeping.json`: per source agent, the prefixes of its own envelope with one reason each (claude `entry_extra`, `raw`; codex
`info`, `rate_limits`, `permission_profile`, `file_system_sandbox_policy`, `sandbox_policy`, `collaboration_mode`, `truncation_policy`, `source`,
`timezone`). The table gains a "lost source" column. Lost (2,826): 295 declared / 1,946 source bookkeeping / 585 unexplained (was 2,531 unexplained
before the declarations). CORRECTION: an earlier version of this paragraph swapped the last two figures (585 source / 1,946 unexplained) and drew
the wrong conclusion from it; the table, the code and the test always said 1,946 source and 585 unexplained. The 585 unexplained are the real
defect candidates: tool_uses, token_usage, stop_reason, delegations, file-change `before`/`after`/`operation` and similar content, codex -> * (32-36
per pair) first, then copilot -> * (8-20 per pair), each a reader/writer bug when the target format can hold the value. Not done in this step: minted and
renamed classes (leaf-value pairing); unexplained still does not fail.

## Step 2b: renamed and synthesized classes (a third commit)
`classify.rs` now separates: lost *dropped* (contract drop op), *renamed* (contract rename/move `from`), *source* (recording agent's envelope), *unexplained*; gained
*synthesized* (contract synthesize op, or `fixtures/target-synthesized.json`: the producer name and version every target's reader stamps), *renamed* (`to` side),
*unexplained*. `docs/baseline/unexplained.md` lists the unexplained paths per pair (the defect candidates). 63 pairs: lost 280 dropped / 15 renamed / 1,946 source /
585 unexplained; gained 299 synthesized / 14 renamed / 138 unexplained. Renaming here is what the contracts already declare; pairing undeclared renames by leaf value
needs values the snapshots do not hold and is not done. Unexplained does not fail.
