# test(goldens): each writer on its own — one generated document, one snapshot per writer

Status: planned (asked 2026-10-19 on the goldens demo deck: "can we prove it?", gap "each writer on its own") · Base: the goldens stack once merged

## Why
The goldens measure losses end to end on real sessions: a loss in `round_trip@<set>@<agent>` can come from the source
agent's reader or from the target agent's writer, and every source carries different fields. Nothing said which
writer drops what on the same input.

## What changes
- `crates/toolpath-goldens/fixtures/full-view.json`: a generated conversation view that fills every field the view
  carries: thinking, three tool calls (one failed), token usage with cache and a reasoning breakdown, attributed usage,
  a file mutation with before/after/diff, a delegation with its own turn, environment, base, producer, group ids.
  It lives outside `goldens/` so the real-session glob does not pick it up.
- `pipeline::writer`: project a document for one agent, read it back with that agent's adapter, derive again, compare.
- `tests/writers.rs`: derives the document from the fixture (`toolpath_convo::derive_path`, thinking and tool uses
  included) and pins `writer@<agent>` for all seven agents. Existing snapshots are untouched.

## What it shows (2026-10-19, document of 63 key paths)
| writer | lost | gained | text | tool calls |
|---|---|---|---|---|
| claude | 21 | 5 | kept | 3/3 |
| codex | 23 | 32 | kept | 3/3 |
| pi | 25 | 0 | kept | 3/3 |
| copilot | 20 | 2 | kept | 3/3 |
| gemini | 24 | 0 | kept | 3/3 |
| opencode | 24 | 5 | kept | 3/3 |
| cursor | 24 | 2 | kept | **2/3** |

Every writer keeps the conversation text. Cursor drops one of three tool calls. No writer keeps a delegation's own
turns; only copilot keeps the delegation itself (agent id, prompt, result). Codex invents the most (32 gained key
paths: its session_meta/turn_context defaults read back as fields).

## Verification
- `cargo test -p toolpath-goldens --test writers` passes against the committed snapshots; the full gates pass.

## Paths
```
crates/toolpath-goldens/fixtures/full-view.json
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/tests/writers.rs
crates/toolpath-goldens/snapshots/writer@*.snap
```
