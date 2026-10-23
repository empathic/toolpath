# test(harness-mock): what Claude Code would send when it resumes a projected session, without the binary

Status: planned (asked 2026-10-19 on the goldens demo deck: "we have a webmock could we create a mock harness too?") · Base: the goldens and webmock stacks once merged

## Why
Resume is proven only by running the real agent binaries (the resume matrix), which CI cannot do. Every resume bug so
far lived in the harness-local half of a session file: pi → claude's broken `parentUuid` chain (the model saw only the
newest message) and copilot → claude's empty errored `tool_result` (a 400). `toolpath-webmock` stands in for the model;
this stands in for the agent: given a projected session, compute the history the harness's own loader would send.

## What changes
- `crates/toolpath-harness-mock` (`publish = false`): `Message`/`Block`, `check_api` (the Messages API's rules: every
  tool call answered in the next message, a result follows its call, an errored result is not empty), and
  `claude::resume`, Claude Code's loader as five rules (leaf, `parentUuid` walk, roles, merges by `message.id`,
  parallel tool results) returning the messages and the conversation entries the walk never reaches.
- `tests/claude.rs`: every projection into Claude Code in `toolpath-goldens/snapshots` resumes whole, except two pinned
  findings; the two historical bugs, re-introduced in a copy of the fixture, are caught; contract: on the codex golden's
  projection the mock's history is a prefix of what Claude Code 2.1.285 really sent (webmock cassette `9d1356…`).
- `docs/mock-harness.md`: buy/borrow/build, the design, what it catches, the order for the other six harnesses.

## Findings (pinned in `KNOWN`, unverified live)
- opencode → claude: every assistant is parented to the first user turn (opencode's own parent field), so Claude Code's
  walk reaches 2 of 22 conversation entries: the pi → claude bug class, in a source the matrix never ran.
- cursor → claude: the golden's Glob call has no result; the projection writes an unanswered `tool_use`.

## Verification
`cargo test -p toolpath-harness-mock`: 5 unit + 4 integration tests. Workspace gates with `--features path-cli/goldens`:
fmt, clippy `-D warnings` (all targets), `cargo test --workspace` (83 suites, private target dir: a shared one raced
with other builds overwriting `target/debug/path`), doc, shellcheck.

## Paths
```
Cargo.toml
Cargo.lock
crates/toolpath-harness-mock/
docs/mock-harness.md
```
