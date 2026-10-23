# fix(codex): a codex -> codex round trip keeps the task lifecycle (task_started / task_complete)

Status: draft · renumbered on landing (author label pr/104) · Base: pr/105 · Source: pr/99 recommendation 1, group 4 (first part); controller-core's order

## Why
After pr/105 codex-forced still lost 38 paths on codex -> codex. Part of group 4 is the task lifecycle Codex writes as `event_msg` `task_started` and `task_complete`:
`started_at`, `completed_at`, `duration_ms`, `time_to_first_token_ms`, `collaboration_mode_kind`, `model_context_window`, `last_agent_message`. The reader keeps both as events; the writer wrote neither. Codex holds them: a writer bug.

## What changes
`codex_passthrough` (toolpath-codex/src/project.rs) also returns the `task_started` / `task_complete` events as `event_msg` lines, payload verbatim, same placement rule as pr/102, Codex-recorded views only.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `codex_task_lifecycle_events_are_written_back` failed (no task_started line), passes. The existing negative tests (other harnesses get nothing) still pass; goldens twice-around and contract checks pass with the new lines in the output.
- shed@codex-forced: lost 38 -> 31 key paths; shed@codex and goldens/shed/codex.json the same; no cross-agent snapshot or table changed.
- toolpath-codex, toolpath-goldens, toolpath, toolpath-convo, path-cli --lib (463 pass, 14 known port-bind failures), path-cli --features develop --test goldens, fmt, clippy -D warnings (nix clippy 1.97.1), qualifier 238/0.
- NOT run: path-cli integration tests, pinned 1.94, gitleaks, nix, live codex resume.

## What is left of group 4 (31 paths) and why it is a separate piece
- `token_count` lines: the writer synthesizes them from the steps' usage, so the original's `rate_limits.*`, `info.last_token_usage.*`, `info.model_context_window` and `total_tokens`/`reasoning_output_tokens` need enriching the synthesized line (aligning the source's duplicated counts with the written ones), not a passthrough: a passthrough would double the usage the reader differences.
- `exec_command_end` / `patch_apply_end`: synthesized too (partial); `aggregated_output`, `formatted_output`, `parsed_cmd[]`, `duration`, `exit_code`, `command[]`, `changes.<path>...` need enriching the same way.
- `encrypted_content` (reasoning), `namespace`, `multi_agent_version`, `realtime_active`, `summary`: each needs a look at where it is read.
