# fix(goldens): a cursor-agent read-back gets no working directory the document never named

Status: draft · Base: pr/76 · Code (toolpath-goldens pipeline.rs `value_cwd`, `read_in`) + 1 snapshot

## Why (plain words)
pr/76 tells the cursor-agent reader the directory the session was resumed in. For a document that names none, `value_cwd` fell
back to `CALLER_CWD` (`/`, where the hermetic tool ran), so gemini -> cursor-agent came back with a `file:///` base it never had:
`paths[].path.base.uri` gained. That directory was invented, not the source's.

## What
`value_cwd` returns the document's own `file://` directory or None (no fallback); `read_in` sets the cursor-agent view's
working_dir only when there is one. `resume_dir` (what a projection is resumed in) keeps its `CALLER_CWD` fallback; the writers
are unchanged. Sources with a cwd read back as in pr/76.

## Snapshots
- `round_trip@gemini@cursor-agent`: `gained` `[paths[].path.base.uri]` -> `[]`; after key_paths 34 -> 33. Steps, text and
  tool uses unchanged.
- Nothing else moved: every other cursor-agent round trip and twice-around comes from a source with a cwd; `capture --all` moved
  no output sha.

## Verification
`INSTA_UPDATE=always cargo test -p toolpath-goldens` then a clean run; gates as for pr/81. One qualifier record re-anchored
(toolpath-goldens pipeline.rs, the `round_trip` concern moved 12 lines).

## Not fixed here
gemini -> cursor (the IDE writer) still gains `paths[].path.base.uri` and `environment.working_dir`: `CursorProjector` falls
back to workspace `/` when the view has none. A writer change, separate from this read-back; fixed in pr/83.
