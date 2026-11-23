# test(goldens): the twice-around round trip, drift declared

Status: draft · Base: pr/67 · Code + 63 `twice_around@<set>@<target>.snap` + `fixtures/twice-around.json`

## Why
Bobby: "the double round trip, the accumulating encoding error. Hopefully they don't exist but we should know."

## What
- Stage `twice_around`: for every recording x target, the document the target's adapter derived from pass 1 is projected to the
  SAME target again, read back and derived; pass 2 is compared with pass 1 (`losses::compare_passes`): key paths lost, gained,
  and `changed` (present in both, values differ as sorted lists). Paths only, never values, so a minted id cannot make a snapshot flap.
- `tests/twice_around.rs` reads the committed snapshots and `fixtures/twice-around.json`: every drifting key path must be declared
  with a reason (and optionally the defect that causes it), and a declaration no snapshot needs any more fails (both directions).
- Schema `docs/baseline/schemas/twice_around.schema.json` generated from `TwiceAround`.

## Result
Drift is one-time, nothing accumulates (scratch-main-21 measured IR2 a fixed point for 63/63 on f308ab5). 15 of 63 snapshots drift,
all into codex (11 key paths) or copilot (14): copilot's minted ids by design; codex's empty-turn turn_context (BUG, pr/69);
copilot's dropped system.message (BUG, pr/70); codex turn_id renumbering (declared; stabilise by writing codex-turn-NNNN ids).
pi's invoke_agent -> task rename is not seen here (it shows in an IR the pass does not compare); it folds into the delegation
helper fix as Bobby said and is not done in this piece.

## Open
- `qualifier_anchors_on_this_crate_are_fresh` (toolpath-goldens) fails: annotations in this crate drifted because of added lines;
  re-anchor with the qualifier, not by hand. Same remedy as pr/67's two.
- `path d baseline capture --all` / baseline check not run (dev shell).

## Why drift appears on pass 2 only, and why it does not pile up (for the deck)
Pass 1 starts from the original recording; pass 2 starts from what the reader derived from pass 1's output. Anything a writer or
reader normalises (renumbers, moves, mints, drops) is already normalised in pass 2's input, so the first trip changes it and later trips
find nothing left to change. Measured by scratch-main-21 (P(IR1) == P(IR2) byte-identical for 63/63): IR2 is a fixed point, text and tool
uses identical for 63/63. Drift that is a bug (pr/69, pr/70) was fixed; the rest is declared: copilot mints a new session id per export
(by design), codex renumbers turn ids on read (declared), pi -> codex model_provider (untraced, declared).
