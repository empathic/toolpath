# test(path-cli): tests for #305's pack_batches over the baseline's documents

Status: draft · Base: #305 (ben/jsonl-streaming-upload) at a84386a2 · Author of the tests: toolpath-author; the code under test is Ben Barber's and is NOT changed

## Why
Ben's streamed upload (#305) is going to merge. `pack_batches` is pure (no server, no network), so it can be tested against every real document the baseline holds, not only the
synthetic paths its own tests build. Bobby asked for this before the Ben-first rebase.

## What changes
- `crates/path-cli/src/cmd_pathbase_batching_tests.rs` (new), mounted by ONE `#[cfg(test)] #[path = ...] mod batching_tests;` line in `cmd_pathbase.rs` (the only edit to Ben's file, so the tests can see the private `pack_batches`).
- `proptest = "1"` as a dev-dependency of path-cli (Cargo.toml, Cargo.lock).
- A qualifier concern on `pack_batches`' closing condition (`crates/path-cli/src/.qual`), see "Finding".

Recordings: `examples/*.path.json` always; when the baseline is in the tree, every `crates/toolpath-goldens/snapshots/document@*.snap` (13 recordings incl. the four forced compactions).
On Ben's head alone only the examples (4 paths) are found, so the "every baseline recording" run happens once this rides on a tree that has the baseline.

## Verification (sandbox, cargo 1.97.1, offline)
Run on Ben's head a84386a2 plus this piece:
- reassembly: the batches concatenate to exactly the writer's output (after `parents_first_order`, as `stream_paths` does), at budgets 1, 700, 20000 and 4 MiB: PASS
- each batch valid alone (PathOpen only in batch 0, actors before the first step, parents before children across batches, a step's signatures stay behind its step; every batch holds a step except an allowed closing tail): PASS
- a small document is one request; a path with no steps packs to at most one batch and loses nothing: PASS
- a 5 MiB step goes alone and everything still reassembles: PASS
- a real document repeated past 4 MiB runs the batching path (>= 2 batches, over budget only for a lone step): PASS
- proptest (fixed seed, 200 cases: random step sizes, parent links, order, budget): PASS
- Ben's own tests, unchanged: 458 pass; the 31 that fail all fail with `PermissionDenied` at the mock server's `bind` (this sandbox forbids opening a port): his 30 `MockServer` tests and my one wire test.
  NOT verified here: `wire::a_streamed_upload_puts_the_writers_jsonl_on_the_wire` (stub Pathbase, bodies on the wire concatenate to the writer's JSONL): compiles, could not run; to be run by scratch-main-82.
- NOT done: a Pathbase read-back comparison with a real server ("streamed upload reads back equal to single-request"); that needs Pathbase and is the pathbase-side run.

## Finding for Ben (not fixed here)
`pack_batches` closes a full batch when the NEXT line would exceed the budget and the batch already holds a step. When that next line is a closing line (`Head`, `PathClose`, a path-level `Signature`),
the closing lines become a batch of their own with NO step: on `path-04-exploration` at budget 1 the last batch is `[Head, PathClose]`. The doc comment says a batch is only closed once it holds a step; that
is true of the batch being closed, not the one that follows. With the real 4 MiB budget it needs the last step to land within ~60 bytes of the limit. Whether the server accepts a step-less append is unknown.
The strict test `no_batch_is_ever_without_a_step` is `#[ignore]`d and fails today; the other tests allow exactly this tail and nothing else. Idea: only close before a `Step` line. Recorded as a qualifier concern at `cmd_pathbase.rs:664`.
Also: Bobby's brief mentions `Head` lines added per batch and non-final batches ending with one; #305's `pack_batches` adds nothing ("the bodies concatenate back to the input") and relies on the writer's single closing `Head`. Tested as it is.

## For the next PR
Run the wire test and the baseline-wide run where a port can be bound and the baseline is present; decide with Ben whether the step-less tail is a bug.

## Not run
Pinned 1.94, gitleaks, clippy/fmt of the whole workspace (Ben's tree: `cargo fmt -p path-cli` only), nix.
