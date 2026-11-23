# fix(cursor): the IDE writer names no workspace the document never named

Status: draft · Base: pr/82 · Code (toolpath-cursor project.rs) + docs/agents/formats/cursor.md + 3 snapshots

## Why (plain words)
`CursorProjector` took its workspace from the caller, then the view's turns, then the view's base, and otherwise fell back to
`/`. gemini names no working directory, so gemini -> cursor wrote a `file:///` workspace and read back with
`paths[].path.base.uri` and `environment.working_dir` it never had (pr/82's "Not fixed here"). That directory was invented.

## What
With no directory from the caller or the view, the writer leaves `workspaceIdentifier` out of the composer head. A caller-given
workspace id with no directory keeps the id and writes no `uri` (as for an untitled or remote window). Cursor's own heads carry
both shapes, and the reader already reads them as "no workspace". Sources with a cwd, and every caller that passes a project
directory (`path export cursor --project`, `p resume`), write as before. docs/agents/formats/cursor.md: the writer rule.

## Snapshots
- `round_trip@gemini@cursor`: `gained` loses `paths[].path.base.uri` and `...structural.environment.working_dir`; after
  key_paths 54 -> 52. Steps, text and tool uses unchanged.
- `projection@gemini@cursor`: the head has no `workspaceIdentifier` (was id `8a5e…` with `file:///`).
- `shed@gemini`: the cursor projection's size shrinks by those lines (1372 -> 1362 lines, 65272 -> 65045 bytes).
- Nothing else moved: every other cursor source has a cwd; `twice_around@gemini@cursor` stays empty.

## Verification
Unit test `no_working_dir_writes_no_workspace`. `INSTA_UPDATE=always` workspace test run, then the gates as for pr/81.
