# fix(copilot): the session id is derived from the document, not minted at random

Status: draft · Base: pr/86 · Approved by Bobby 2026-10-21 · Code: toolpath-copilot `session_id_for`, path-cli projection/copilot.rs, goldens pipeline; 8 twice_around snapshots

## Why (plain words)
Every Copilot projection minted `Uuid::new_v4()` as its session id. So projecting the same document twice gave two sessions, and going round twice
(round 2's input is round 1's output) changed `graph.id`, `path.id` and `meta.title`, which carry the id: all 8 copilot pairs differed by id alone, and
the id never settled.

## What
- `toolpath_copilot::project::session_id_for(source_id)`: a UUIDv5 (a valid UUID, which Copilot needs) of the document's own id in a toolpath namespace.
  An id that is already a v5 comes back unchanged: that is an id this function made, handed back by reading a projection and projecting it again, so the id is
  a fixed point. A random v4 (Copilot's own) or any other string is hashed. Limit: a real source whose own id happens to be a UUIDv5 keeps that id.
- path-cli `project_copilot` never clobbers: a session with that id already on disk with byte-identical events is reused (nothing written, said so); one that has
  diverged is left untouched and the document is written under a fresh random id (said so); a `session-store.db` row with no state directory counts as diverged.
- `EventLine.extra` is a `BTreeMap`: a `HashMap` made the same event serialize its keys (`id`, `parentId`) in a different order per run, so projections of the
  same document were never byte-identical (the baseline hid this by sorting keys, `canonicalize`).
- goldens pipeline uses the same function.

## Tests (written first, failing first)
tests/copilot_id.rs (same document -> same bytes incl. id; twice-around documents identical incl. ids; the id is a v5 UUID), toolpath-copilot id_tests (same source -> same
id, round trip keeps it, v4 is hashed), path-cli `project_copilot_names_one_session_for_one_document_and_reuses_it` and `..._never_overwrites_a_diverged_session`.

## Result (on purpose)
The 8 copilot twice_around snapshots are now empty; the three copilot declarations in fixtures/twice-around.json are removed. Twice-around: 56 of 63 recordings x target
pairs are stable (was 48); the 7 left are all codex as target (turn_id renumbering, pi's model_provider), both declared.

## Pending (needs Bobby's go per run)
Live check: `copilot --resume <derived id>` on a derived session. Not run.

## Notes
- Cargo.lock gains `sha1_smol` (uuid's v5 feature). A nix build that vendors crates needs its hash refreshed; I cannot run nix here.
- `canonicalize` for Copilot in the baseline is now redundant for the id but still sorts keys; left as is.
- Not run: pinned 1.94, gitleaks, nix-develop recapture.
- `path d baseline capture --all` now pins Copilot projections as "raw bytes" (it was "canonical (sorted keys, session id masked)"): the baseline picks raw when two
  projections agree, and now they do. goldens/*/to-copilot.jsonl change accordingly (6 files, the id and key order).
