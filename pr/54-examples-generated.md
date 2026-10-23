# test(examples): the examples a producer can make are made by one

Status: planned (asked 2026-10-19: "we should generate these from real code") · Base: the goldens stack once merged ·
Siblings: pr/34 (head fix) changes the derived Claude document this piece commits as path-02; pr/37 (claims as
records) holds the format-page claim this piece re-anchors

## Why
Every file in `examples/` was hand-written. They are what the RFC, the format page, the site's visualizer and the
schema tests show as "a Toolpath document", yet no producer had ever written one of them, so nothing said whether the
tools actually produce that shape. Now the ones a producer can make are its output, and a test fails when a committed
copy drifts from what the producer makes.

## What changes
- `crates/path-cli/tests/examples_generated.rs` (+ `scripts/gen-examples.sh`, `UPDATE_EXAMPLES=1`):
  - `step-01-minimal.json`: `path p track` init + one step over `src/main.rs`, byte for byte its output.
  - `path-01-pr.path.json`: `path p track`, five steps over `src/auth/validator.rs` (human scaffold, an agent's regex
    attempt forked off and abandoned, the agent's typed version, rustfmt, a human refinement); byte for byte.
  - `path-02-local-session.path.json`: the document `path p derive claude` makes from the real recorded session
    `test-fixtures/claude/convo.jsonl` (the goldens pipeline in process), keys sorted.
  - `graph-01-release.json`: `path p merge` over the documents of the real Codex, Gemini CLI and Pi sessions in
    `test-fixtures/`, keys sorted.
  - the `.path.jsonl` twins of path-01 and path-02 from the streaming writer (now identical to their `.json` twins,
    graph id included).
  - Keys are sorted only for the derived sessions: their types carry hash maps whose order changes per process (the
    goldens sort the same way); the `p track` output needs no normalisation.
- `path p track init` gains `--id` (path and graph id; reproducible output) and `--base-branch`; the CLI page and the
  pinned help say so.
- `examples/README.md`: which examples are generated and by what, and which stay illustrative and why (structural AST
  operations, signatures, merge steps, step fragments: no producer emits them). A qualifier concern on each
  illustrative file.
- Shape changes, each followed where it is quoted:
  - step-01: graph id `path-step-001` (track uses the path id), diff with three lines of context (`@@ -1,3 +1,3 @@`).
    `site/pages/format.md`'s quote updated to match (it parses to the same value as the fixture).
  - path-01: step ids `step-001`…`step-005` (the abandoned attempt is `step-002`), one file, the issue ref on the
    first step (track has no path-level refs).
  - path-02 is a real Claude Code session; graph-01 is three real sessions ("Graph: three harnesses" in the
    visualizer's menu, was "parallel agent attempts").
- Tests that leaned on the old hand-written content: `export_claude_refuses_a_document_with_no_session_id` uses
  path-01 (path-02 is a real Claude session now); `query_dead_ends` expects exactly `["step-002"]`; render_md, p
  merge, formats@examples and help@p-track snapshots reviewed and recaptured.
- Qualifier: the format page's minimal-example claim and the CLI page's command-tree concern re-anchored with
  `--supersedes`.

## Verification
`cargo test -p path-cli --test examples_generated` passes three runs in a row (stable); full gates with
`--features path-cli/goldens`: fmt, clippy -D warnings, test --workspace, doc, shellcheck, prettier over site/,
`path p validate` on every example.

## Paths
```
crates/path-cli/tests/examples_generated.rs
crates/path-cli/src/cmd_track.rs
crates/path-cli/src/cmd_export.rs
crates/path-cli/tests/integration.rs
crates/path-cli/tests/snapshots/
crates/path-cli/Cargo.toml
crates/toolpath-goldens/snapshots/formats@examples.snap
Cargo.lock
examples/
scripts/gen-examples.sh
site/pages/format.md
site/pages/cli.md
site/eleventy.config.js
site/pages/.qual
```
