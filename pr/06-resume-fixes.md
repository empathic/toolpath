# fix(pi,copilot,claude): projected sessions resume with their history in every agent

Status: opened #361 (draft, 2026-10-16) — was: ready · Base: pr/02-goldens-all (the recaptured projections live there) · Three independent fixes; split into three PRs by giving each its own recapture if preferred.

Commits: `fix(pi): keep the parent chain through tool results so a projected Pi session resumes with its history` · `fix(copilot): a failed tool call keeps its error message, and workspace.yaml gets a datetime Copilot accepts` · `fix(claude): never project an errored tool_result with empty content` · one repin (`goldens capture --all`).

## Why
A resume matrix over the goldens (each captured session projected into each installed agent, then resumed headlessly in a temp HOME and asked for the first user message) found three cells where the agent saw only the new prompt or refused the file. `path resume` reported success in every case. Method and per-cell artifacts: `review/resume-matrix.md`.

## What changes
- **toolpath-pi**: Pi chains every entry to the previous one, including `toolResult` and model-change entries that the reader folds away; a turn whose Pi parent was folded ended up with no parent in the IR, so Claude's `parentUuid` walk and Pi's own thread walk stopped at the last message (10 of 11 assistant lines had `parentUuid: null`). The provider now resolves a parent through folded entries to the nearest turn; the projector hangs the next turn off the last synthesized `<id>-tr-N` result entry instead of the assistant. Tests: `test_parent_through_folded_entries_resolves_to_the_nearest_turn`, `test_turn_after_tool_results_hangs_off_the_last_result_entry`.
- **toolpath-copilot**: a failed `tool.execution_complete` carries `error.message` and no `result`; the reader took `result` only, producing `is_error: true, content: ""`, which the Messages API rejects (`400 … content cannot be empty if is_error is true`) on the first resumed turn. The reader keeps the message as the result. Also `workspace.yaml` timestamps are written as `…59.997Z`, the form copilot 1.0.61 accepts (`Invalid datetime` before). Tests: `failed_tool_call_keeps_its_error_message_as_the_result`, `workspace_timestamps_are_millisecond_utc_z`.
- **toolpath-claude**: the projector never writes an errored `tool_result` with empty content, whatever the source: it substitutes `(tool call failed; the source session recorded no output)`. Hard rule 4 added to `docs/agents/formats/claude-code/writing-compatible-jsonl.md`. Test: `test_errored_tool_result_without_output_is_never_empty`.
- Goldens: the `-> pi`, `pi -> claude` and `copilot -> *` projections recaptured; known defects unchanged.

## Proves
- Pi: a turn whose parent was folded away resolves to the nearest turn — crates/toolpath-pi/src/provider.rs::test_parent_through_folded_entries_resolves_to_the_nearest_turn
- Pi: the next turn hangs off the last synthesized tool-result entry — crates/toolpath-pi/src/project.rs::test_turn_after_tool_results_hangs_off_the_last_result_entry
- Copilot: a failed tool call keeps its error message as the result (never empty) — crates/toolpath-copilot/src/provider.rs::failed_tool_call_keeps_its_error_message_as_the_result
- Copilot: workspace.yaml timestamps are millisecond UTC `Z` — crates/path-cli/src/projection/copilot.rs::workspace_timestamps_are_millisecond_utc_z
- Claude: an errored tool_result is never written with empty content — crates/toolpath-claude/src/project.rs::test_errored_tool_result_without_output_is_never_empty

## Verification
Resume matrix after the fixes: pi→claude and pi→pi load with 0 null parents; copilot→claude loads and the errored result carries `Path does not exist`. Copilot as a target could not be judged (GitHub returned 402, monthly quota; sessions opened and the datetime error is gone). `cargo test` for the three crates and `path-cli --test goldens` green; `check`: all ok.

## Note
PR #306's `message.usage` mechanism did not reproduce here: codex→claude loaded even with `input_tokens` bumped to 788052.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
crates/path-cli/src/projection/copilot.rs
crates/toolpath-copilot/src/provider.rs
crates/toolpath-copilot/src/types.rs
crates/toolpath-pi/src/project.rs
crates/toolpath-pi/src/provider.rs
docs/agents/formats/claude-code/writing-compatible-jsonl.md
```
