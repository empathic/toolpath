# examples/

Every file here validates against `schema/toolpath.schema.json` (`path p validate`, run by the quality gates).

## Generated: a real producer's output

These are written by `scripts/gen-examples.sh`, and `crates/path-cli/tests/examples_generated.rs` fails when a
committed copy differs from what the producer makes now. Do not edit them by hand: change the producer or the input.

| File                                           | Producer                                                 | Input                                                                                     |
| ---------------------------------------------- | -------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `step-01-minimal.json`                         | `path p track` (init, one step)                          | `src/main.rs`, "Hello world" → "Hello, world!"                                            |
| `path-01-pr.path.json` (+ `.jsonl`)            | `path p track`, five steps, one forked off and abandoned | `src/auth/validator.rs` as a human, an agent and rustfmt edit it                          |
| `path-02-local-session.path.json` (+ `.jsonl`) | `path p derive claude` (the goldens pipeline)            | the real recorded session `test-fixtures/claude/convo.jsonl`                              |
| `graph-01-release.json`                        | `path p merge`                                           | the documents derived from the real Codex, Gemini CLI and Pi sessions in `test-fixtures/` |

The `.path.jsonl` twins are the streaming writer's output for the same documents.

## Illustrative: no producer makes these yet

Hand-written to show a part of the format that nothing emits today. Each carries a qualifier concern saying why.

| File                                                                           | What no producer emits                                                                                                                                            |
| ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `step-02-agent.json`, `step-04-human-refinement.json`, `step-05-dead-end.json` | structural AST operations (`structural.type` like `rust.add_items`); step fragments whose parents are outside the file                                            |
| `step-03-formatter.json`                                                       | a step fragment whose parent is outside the file (a formatter step on its own)                                                                                    |
| `step-06-signed.json`, `path-03-signed-pr.path.json` (+ `.jsonl`)              | signatures: toolpath can now verify an ssh signature (`toolpath` `verify`) but no producer signs                                                                  |
| `step-07-merge.json`, `path-04-exploration.path.json` (+ `.jsonl`)             | a merge step (two parents) inside one path, and structural operations; `path p track` forks but never merges, and the git import puts each branch in its own path |
