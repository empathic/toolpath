# fix(toolpath): a document serializes its maps in key order, so the upload body is the same bytes every time

Status: draft · Base: pr/89 · Found by the toolpath-side share test (controller-core's decision: body == derived document byte for byte)

## Why (plain words)
`path share` uploads `Graph::to_json()` (cmd_export.rs:911-924), and says in a comment that the cached copy "would reproduce it byte-for-byte anyway". It would not. The core
types hold their maps as `HashMap` (`change`, `actors`, and the flattened `extra` of the structural change, step meta, path meta, graph meta), and `HashMap` writes its keys in
a different order every run. The new test (tests/share_body.rs) read each committed document, wrote it, read that back and wrote it again: `claude-compacted` came out
with `role`/`text` in the other order. So the same session produced different upload bodies on different runs, and a cache file did not match a fresh derive. Nothing was lost
(same value either way), but every byte comparison, cache check and diff of an upload was noise. This is the same HashMap-order cause as the Copilot `id`/`parentId` flip fixed in pr/87.

## What
`toolpath::v1` types serialize those maps through `sorted_map` / `sorted_opt_map` (keys in order). Types and the API are unchanged; only the written order is fixed.

## Test (written first, failing first)
tests/share_body.rs: for every committed `document@*` snapshot the body `to_json` writes is byte-identical after being read and written again, and equals the snapshot's value. It failed on
claude-compacted before the change; five repeat runs pass after (a HashMap order that is right by luck would not).

## Where this leaves the share cells (pr/89)
Toolpath's side of the upload is now provably the document, verbatim and stable. The four lossy cells are in Pathbase's reader (routed to work-pathbase-author by controller-core).

## Not run
Pinned 1.94, gitleaks, nix-develop recapture. The byte layout of `path p derive`/share output is now key-sorted (it was random per run); no committed file depended on the old order (workspace tests unchanged).

## Who reads or compares these bytes (what I checked, and what I did not)
Checked in this tree (workspace tests unchanged after the change): the goldens snapshots (insta writes JSON with sorted maps, so they never depended on the order), `examples/` and
`path-cli`'s `query@site-example` (compared as values), signatures (they canonicalize with JCS, which sorts keys itself, tests/jcs.rs and signatures.rs pass), and `Graph::from_json`
(order-insensitive). NOT checked, outside this repo: Pathbase's stored documents and `pathbase-doc-format` snapshots (JSON readers are order-insensitive by construction, but I did not run
them); existing cache files and earlier shares keep their old byte order (equal as values, different as bytes) until re-derived; any external tool that diffs or hashes raw output
(only the byte order moves, and it was random before, so no stable hash of it could have existed). The committed `document@*` snapshots for the Pathbase-side run are in this tree
(`crates/toolpath-goldens/snapshots/`); I could NOT make copies under `~/.lobby/ops/decisions/` (my write scope refused the copy and a tar of it), so they exist only in the
worktree until someone copies them.
