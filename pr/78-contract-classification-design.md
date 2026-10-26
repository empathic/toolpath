# design: classify every lost and gained key path on the real pairs (coverage map first)

Status: DESIGN ONLY, no code · Base: pr/77 · Source: addendum 22:45Z item 3 and "ONE PROCEDURAL SUITE"; measurement scripts in ~/.lobby/ops/goldens-session-2026-10-19/new-deck/gen/ (measure.py)

## Problem
`tests/contract.rs` checks each writer's declared ops (drop / rename / synthesize in fixtures/contract/<agent>.json) against ONE input,
fixtures/full-view.json. The 63 real recording x target round trips are pinned as lost/gained key-path lists (`round_trip@*.snap`) but
nothing says WHY each path is lost or gained. Measured at c1e8cd9: 2,540 of 2,854 lost and 175 of 472 gained paths are undeclared. Most is
the source agent's own bookkeeping, but it is unlabelled, so a real defect (a lost system turn, a dropped tool call) hides in the list.

## Classes (each lost or gained path on each pair gets exactly one)
1. declared: the TARGET writer's contract has an op covering the path (prefix match, as measure.py `cov`).
2. source-bookkeeping: the path belongs to the SOURCE agent's own envelope (e.g. codex `turn_context.*`, claude `entry_extra.*`,
   copilot `data.*`); target formats cannot hold another agent's bookkeeping. Declared ONCE per source agent in fixtures/contract/<agent>.source.json
   as path prefixes with a reason, not per pair.
3. minted: ids, timestamps and per-export values the target mints (copilot session id, codex turn ids); already in fixtures/twice-around.json for pass 2; the same list is reused.
4. renamed: the value arrives under another key (writer rename op, or reader normalisation). Counted when lost and gained paths pair up by leaf value.
5. unexplained: everything else. THE TEST FAILS ONLY ON THESE, listing path, pair and the nearest candidate class.

## Mechanism (no new snapshot format)
- New stage output `classified` per recording x target, computed from the existing `lost`/`gained` lists plus contracts; snapshot stores
  counts per class and the unexplained paths only (paths only, never values, so minted ids cannot flap it).
- Pairing for renamed: join lost and gained on sorted leaf values (`keypaths::leaf_values`, from twice-around), same path-shape tail.
- `baseline map` renders one row per agent pair from the same data: "N lost: a declared, b source-bookkeeping, c renamed, d unexplained".
  Seeded from measure.py (which already computes the per-pair counts); Rust port keeps its output as the test for parity.

## Order of work (each its own pr/NN)
1. Coverage map: run the classifier in report mode over all 63 pairs, commit the table (this record's table), no failing test.
2. Source-bookkeeping and minted declarations: bring unexplained from ~2,300 toward the real defects. Every real defect found is fixed in a writer (as pr/69, 70, 77), declared only when the target format has no place for it.
3. Turn the check on: the contract test fails on unexplained > 0; the allow-list is the declaration files.
4. Retire per-crate round-trip assertions one at a time, each citing its map row (the one-suite migration); credit Ben in the PR.

## Open (Bobby)
- Is "source-bookkeeping" an acceptable declared class, or should those paths be required to ride in `extra` and be re-written (a stronger round trip, much bigger work)?
- Where the table lives: docs/baseline/ next to the schemas (recommended) or the deck repo.

## Not run
Record only. No code, no gates.
