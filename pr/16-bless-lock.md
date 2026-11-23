# chore(goldens): bless the goldens — LOCK.json records the committed shas

Status: planned (decided 2026-10-16 at the demo: goldens stay in-repo; blessing is a follow-up PR that only records shas) · Base: the goldens stack once merged
Decided with it: the goldens gate stays on every PR (not nightly).

## Why
Every manifest/snapshot header says "dev build": the outputs are pinned, but nothing says a human looked at them and
accepted them as the baseline. Blessing is that act, recorded as data: a LOCK.json with the commit and the sha256 of
each golden input and each snapshot, so a later change to a golden is a change to LOCK.json too, reviewable on its own.

## What changes (to write when cut)
- `LOCK.json` beside the goldens: {set, input sha256, snapshot shas, blessed_at commit}; a test fails when a snapshot's
  sha is not the blessed one and the lock was not updated in the same change.
- A `bless` verb (toolpath: `path goldens bless`; pathbase: `cargo run -p pathbase-goldens -- bless`) that rewrites LOCK.json.
- No golden content changes in this PR: lock only.

## Verification
- Blessing on the merged stack produces a LOCK.json whose shas equal `sha256sum` of the files; changing one snapshot
  byte makes the lock test red; re-blessing makes it green.

## Paths
```
LOCK.json
```
