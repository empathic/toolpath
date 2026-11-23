# design: ONE cut file on wip, everything else derived (DESIGN ONLY, nothing built)

Status: design for review, docs only · Base: pr/111 · Source: Bobby 2026-10-21 "one file that gets derived; it's more like cutting the cake we have and holding a part back. It should always include all of the changes."

## Today (measured)
`pr/` holds 110 records (one `NN-slug.md` each, with a `Paths` fileset block), `pr/patches/` 7 MB of per-record patches, `cut.sh` (jj split by fileset, the remainder = HOLD), `HOLD.md` (the held paths), `open.sh`, `gate-piece.sh`, `stack.json`. The patches duplicate what the wip commit already contains; the filesets say which whole FILES a piece takes, so a file touched by two pieces (Cargo.lock, CHANGELOG, a shared source file) goes to the earlier one and the later piece cannot carry its own hunk.

## The model
wip is the cake: one commit whose tree = main + EVERYTHING. A **cut** divides its diff against main into ordered slices plus a final `hold`. Every hunk is in exactly one slice or in hold. Patches, branches, bookmarks and trees are DERIVED from (wip rev, cut file) and never committed.

### The file: `pr/cut.toml` (TOML)
Chosen over JSON (comments, no trailing-comma pain, multi-line bodies) and over YAML (no implicit typing). One file, slices in stack order:
```toml
[meta]
base = "main"                      # the rev the cake is cut against
hold_title = "held back"

[[slice]]
id    = "pr/95-codex-permission-settings"   # also the bookmark name
title = "fix(codex): a codex -> codex round trip keeps permission_profile ..."
base  = "pr/94-jsonl-batching-tests"        # previous slice unless "main"
body  = "pr/95-codex-permission-settings.md" # Alex's shape stays in a short md beside it (or body = """...""" inline)
take  = [
  "crates/toolpath-codex/src/project.rs",          # whole file
  "crates/toolpath-goldens/snapshots/shed@codex*.snap",   # glob
  "Cargo.lock#a1b2c3d4e5f6",                       # ONE hunk of a shared file, by hunk id
]

[[slice]]
id = "pr/96-..."
# ...

[hold]                              # last: what is held back, with the reason
take = ["pr/**", "crates/toolpath-webmock/cassettes/claude/**"]
why  = "producer pins: complexity for a drift that has not happened (HOLD.md today)"
```
- **Selector**: a path (exact or glob) takes every hunk of that file not claimed by an EARLIER more specific selector; `path#<hunk-id>` takes one hunk. A **hunk id** is the first 12 hex of sha256 over the hunk's changed lines (context and line numbers excluded), so it survives line shifts and is stable across rebases. `cut list --hunks <path>` prints a file's hunks with ids and a preview, so a shared file (Cargo.toml, Cargo.lock, CHANGELOG) is split by pasting ids.
- Precedence is exact: `path#hunk` beats a glob beats a directory glob; two selectors of the same strength claiming one hunk is an error, not a tie-break.

### The cake invariant (checked, a failing check, not a warning)
`cut check` computes the full diff main..wip, splits it into (path, hunk-id) units, resolves every selector, and asserts:
1. every unit is claimed by exactly one slice or by hold (nothing lost, nothing duplicated);
2. a unit no slice claims lands in `unassigned` and FAILS the check, naming the unit (the wip commit always contains ALL changes, so a new wip change must be cut or held before the cut is valid);
3. the slices applied in order onto main, then hold, reproduce wip's tree byte for byte (tree hash compare), and each slice's tree is exactly its parent slice's tree plus its units;
4. a slice's `base` exists earlier in the file (or is main).

## Derived, never committed
A nix flake output `.#pr-slices` (inputs: the wip rev, `pr/cut.toml`; the tool below):
- one directory per slice: `patch` (a format-patch mbox, the title as subject), `tree` (the slice's tree), `gate.json` (cargo metadata, fmt, clippy -D warnings, test, doc: the `gate-piece.sh` set, run in the derivation or right after it), plus `body.md`;
- a thin wrapper `cut bookmarks` creates the jj bookmarks (`pr/NN-slug`) from those outputs in the colocated clone (what `cut.sh` does by `jj split` today, minus the splitting);
- `pr/open.sh` stays Bobby's one gated step; it reads titles, bodies and bases from `cut.toml` instead of globbing records.
Because the outputs are functions of (wip rev, cut.toml), the same inputs give the same patches: a reviewer can recompute them.

## The tool: `cake` (one crate, no toolpath code)
A small Rust binary (own crate in the repo under `tools/cake/`, publishable later), using `gix`/`git2` for diffs and trees: `cake check|list|slice|bookmarks`. Reads `--repo`, `--wip`, `--cut`; knows nothing about cargo (the gate command is a field in the file: `gate = "pr/gate-piece.sh"`). The same binary serves pathbase's wip and any repo with a wip commit. TDD: the invariant tests are property tests (random hunks, random selectors: the partition always holds or the check names the offender).

## Migration (no big bang)
1. `cake import`: reads today's `pr/NN-*.md` (title line, `Base:`, `Paths`) and INVENTORY, writes `pr/cut.toml` with whole-file `take`s; hold from `HOLD.md`.
2. Prove it: derive the slices for the newest 5 records and compare with their committed `pr/patches/NN-*.patch` (same changed files and hunks; commit metadata differs). Any difference is a bug in the import, fixed before anything is deleted.
3. Only then delete `pr/patches/` (about 7 MB today, 20 MB in Bobby's count with history) and the per-record `Paths` blocks (the md keep title, Base, body).
4. Files shared between records get hunk-level `take`s the first time a record needs one; until then the earliest-record rule of today applies.

## Risks and open questions
- Hunk ids change when a hunk's changed lines change (an amend): the cut must be updated; `cake check` says exactly which id vanished (a loud failure, not a silent re-cut).
- A pure rename or a binary file is one unit (whole-file selector only).
- jj/git interplay: the derived bookmarks live in the jj clone; nothing derived is pushed by the tool.
- Needs Bobby's decision: TOML (proposed), the crate location (`tools/cake` vs its own repo), and whether `gate` runs inside the derivation (hermetic, slow) or right after (proposed: after).
