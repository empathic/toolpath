# Pulling PRs out of `wip/bdelanghe`

`wip/bdelanghe` is the only branch pushed to this repo. It is one squashed commit on
top of `main` plus the signed commits made since the squash (re-squashed with
`--force-with-lease` when it must be; no Claude trailers; the fleet rule: push wip only).
PRs are *derived* from it. This directory is the source for those PRs: one file
per planned PR with the title, the body in the shape Alex reads, and the fileset
(`Paths`) it carries. The files are committed on wip so they travel with the work;
the PR branches themselves never include this directory.

## The loop

1. **Review before he does.** Run the machine round yourself (Qualifier-style
   `Blocker/Concern/Suggestion` + `Failure:` findings, verified by running the
   code), then the hand review in his voice (scope, stacking, ownership, naming,
   "already solved?"). Fix the blockers on wip as small signed commits. Hold back
   anything you cannot justify against a regression that actually happened.
2. **Describe each PR here** (`NN-<slug>.md`): title, base, its fileset (`Paths`),
   and a body with `Why`, `What changes`
   (per crate), `Verification` (what ran, counts), and `For the next PR` when it
   is a stack. Independent fixes come first and alone.
3. **Cut the branches with `pr/cut.sh`** in a colocated jj clone, never in the
   pushed checkout: one new commit on `main` whose tree is wip's tree (`jj new` +
   `jj restore --from wip`), then `jj split` by each PR's fileset, in order (see
   *Virtual PRs* below). The remainder is HOLD and stays in the clone.
4. **Verify each branch on its own** from a depth-1 clone (CI's checkout
   shape): fmt, clippy `-D warnings`, the crate's tests, and the goldens check.
5. **Open the PR** with the file's body (Bobby opens; `gh pr create` is
   refused for agents in empathic repos). CI green is the merge gate.
6. **After merge**, `jj`'s view of wip picks up the squash; wip itself is left
   alone and `main` is merged into it when needed.

## Files

- `README.md`: this process.
- `NN-<slug>.md`: one planned PR. `Status:` is one of `draft`, `ready`,
  `opened #N`, `merged`.
- `HOLD.md`: commits deliberately not in any PR, and why; its Paths block is the remainder fileset.
- `cut.sh`: cuts the virtual PRs with `jj split` by fileset (below).
- `stack.json`: `cut.sh`'s output, the record of the last cut (the committed copy is the cut
  made at squash time). The *shape* (order, titles, files per piece) is a function of the
  filesets and stays true; the ids (change, commit, tree, operation) belong to the clone and
  operation that produced them and are rewritten on every run.
- `patches/`: convenience export of the pre-squash commits per PR; the filesets are the source.

## What the first run taught (2026-10-15)

- **Duplicate the whole series in wip order, not a by-feature subset.** Cherry-picking
  commits out of an incremental series conflicts in every file the skipped commits
  touched (`manifest.json`, `DEMO.md`, the tool's main source). The ordered series
  duplicated onto `main` conflicted in one file (root `Cargo.toml`, a version line).
  Splitting by feature needs the code split into files first; that is the insta
  rewrite's job, not jj's.
- **Exclude unrelated wip commits by inspection, then re-include the ones the series
  depends on.** A grep on subjects pulled in two old `convo` commits that were not
  goldens work and conflicted; a real fix the goldens pinned (`fix(claude): drop empty
  streaming-seed assistant lines`) had to come first in the chain.
- **Generated files are regenerated at the PR tip, never merged by hand.**
  `goldens/manifest.json`, `goldens/*/` outputs and `DEMO.md`: run `capture --all`
  (or the crate's capture) on the branch tip and commit the result.
- **`main` differs from wip in ways a branch inherits**: toolpath's `main` has no
  `flake.nix` (#332), so anything that shells out to `nix develop` must fall back to
  plain cargo; pathbase's `main` moved `tests/it/*.rs`, so qualifier span records
  anchored on wip drift on the branch and a fail-closed qualifier gate goes red there.
- **Verify the branch, not wip**: depth-1 clone of the bookmark, build, tests, check.
- **Bookmarks**: `pr/NN-<slug>`; the jj clone lives outside the pushed checkout
  (`scratchpad/jj/<repo>`, `origin` = the worktree, `github` = the real remote for
  `main`). `jj git fetch --remote github` before duplicating anything new from wip.
- **Fix branches carry their own recaptured goldens.** A fix found by the goldens recaptures
  the projections it changes, so two fix branches conflict in `goldens/`. Landing them on
  wip: cherry-pick the code commits with `-X theirs` (only `goldens/` conflicts; the code is
  in different crates), then one `capture --all` at the tip and one repin commit. Never
  commit a recapture that did not run: the first attempt committed conflict markers in
  `manifest.json` because the capture failed and the commit step did not check it. Replaced
  with `--force-with-lease` within minutes; the fix is to gate the repin commit on the
  capture's exit status.

## Virtual PRs (Bobby, 2026-10-15; squash model since the history scrub)

`wip/bdelanghe` is squashed: one signed commit on top of `main` whose tree is the
whole series (the per-commit history is gone from the remote; the pre-squash tip is
kept as a local tag `wip-pre-squash-2026-10-15`, never pushed). A PR is not opened
until Bobby says so. Until then it is *virtual*: its file here (title, body) plus a
`Paths` block, a jj **fileset** (one path or `glob:"dir/**"` per line). Filesets are
disjoint across PRs and their union plus `HOLD.md`'s Paths is the whole squashed diff;
a file several PRs touched belongs to the earliest. Qualifier stays first-class: open
review findings are `qualifier record`s at their spans, and a fix `qualifier resolve`s
them; the insta crates run `qualifier review` as a test.

Cutting (`pr/cut.sh`, run with `JJ_CLONE` set to a colocated jj clone with a `github`
remote; never in this checkout):

1. `jj new --no-edit main@github` once, described `HOLD: not in any PR`, then
   `jj restore --from wip/bdelanghe@github --into` it: one commit whose tree is wip's
   whole tree, however many commits wip carries (the squash and what was signed on
   top of it since). It shrinks as PRs are split off it.
2. For each `NN-<slug>.md` with a Paths block, in order: `jj split -r <remainder>
   <fileset…>` (non-interactive; the fileset becomes the PR's commit, described with the
   md's title, bookmarked `pr/NN-<slug>`; the rest stays on top). A PR whose bookmark
   already exists in the clone is skipped, so a rerun only cuts what is new.
3. The final remainder must match `HOLD.md`'s Paths; otherwise the script fails and
   names the stray file (fix the md filesets or HOLD.md, never the clone by hand).
4. `pr/stack.json` is written next to the script: base, wip and the jj operation id
   (`jj op restore <id>` puts the clone back at exactly this stack), and per piece the
   change id, commit id, tree id (the cache key for any later per-commit derivation),
   description, parents, bookmarks and changed files. A later check can assert the
   stack's shape (linear, every commit described, filesets disjoint).
5. The script prints the stack and the push line; it never pushes. In the clone only,
   `git.private-commits = 'description(glob:"wip:*") | description(glob:"HOLD*")'` is set
   by the script so the HOLD remainder can never be pushed by accident.

`patches/NN-<slug>.patch` are a convenience export of the pre-squash commits (mbox,
`git am --3way`-able, machine paths scrubbed); the filesets are the source.
