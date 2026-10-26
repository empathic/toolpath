# fix(opencode): read, list, glob and grep no longer become file changes

Status: draft · Base: pr/56-writer-contracts · Code + recorded snapshots (opencode rows and columns) + one contract

## Why
Asked from the deck's "gains" slide: which code path turns a read into a file change, is it a bug, is it documented.
- Path: `toolpath-opencode/src/provider.rs`, the tool-input fallback in `compute_turn_mutations`. It ran for every tool
  whose input has `filePath`, `file_path` or `path`, and `tool_to_operation` maps every name it does not know to `touch`.
  So `read`, `list`, `glob` and `grep` (all path-shaped, one on a directory) each became a `file.write` change with
  operation `touch`, and landed in `files_changed`.
- Bug: not documented anywhere as intended. pr/56 and the opencode contract called it "a failed read", which was wrong:
  a successful read of an existing file or directory did it too (the recording's `read` of the working directory, and of
  `notes.md` before it was written).

## What changes
- `toolpath-opencode`: the fallback only runs for tools whose category is `FileWrite` (`write`, `edit`, `multiedit`,
  `patch`, `delete`). New unit test: a session with a read, a grep and a write yields one mutation (the write, `add`).
- `crates/toolpath-goldens/fixtures/contract/opencode.json`: the four `synthesize` entries for `does-not-exist.txt`
  are gone; there is nothing left to declare.
- `pr/56-writer-contracts.md`: the defect-table row for it is removed.
- Snapshots re-recorded (INSTA_UPDATE=always, then reviewed): 19 opencode-touching files. Every diff is the removal of a
  `touch` mutation for a read/glob/grep (3 on the working directory, 1 on `notes.md` read before it is written, 1 on
  `does-not-exist.txt`), the matching `files_changed` entries, and the counts derived from them (`key_paths` 59 -> 51,
  `filesChangedCount` 4 -> 2, shed byte and char totals). No snapshot of another harness's own row changed.

## Not changed
- The `FileWrite` operation default (`touch` for an unknown name) is now unreachable for opencode; left in place.
- Nothing run unsandboxed: gitleaks, qualifier gate, `just baseline`, the pinned toolchain.
