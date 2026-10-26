# fix(codex): a codex -> codex round trip keeps current_date, timezone, workspace_roots and thread_source

Status: draft · renumbered on landing (author label pr/103) · Base: pr/104 · Source: pr/99 recommendation 1, group 3; controller-core's order

## Why
After pr/104 codex-forced still lost 42 paths on codex -> codex. Group 3 is the context Codex states on its `turn_context` (`current_date`, `timezone`, `workspace_roots[]`) and `thread_source` on `session_meta`.
The writer had typed slots for the first two and left them `None`; the others had no slot. Codex holds all four: writer bugs.

## What changes (toolpath-codex/src/project.rs, Codex-recorded views only, read from the view's first `turn_context` / `session_meta` event)
- `TurnSettings` carries `current_date`, `timezone`, `workspace_roots`; written on every `turn_context`.
- `workspace_roots` that were exactly the source's `cwd` follow the cwd the session is written for (a resume in another directory must not point at the old one); other roots go back verbatim. Tested both ways.
- `thread_source` goes back on `session_meta`.
- Other harnesses' views get none (the existing negative test now lists the three keys).

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `codex_session_keeps_its_turn_settings` (extended) and `codex_compaction_and_base_instructions_are_written_back` (extended with thread_source) failed on the new keys (Null), then passed; `workspace_roots_that_were_just_the_cwd_follow_the_new_cwd` is new.
- shed@codex-forced: lost 42 -> 38 key paths; shed@codex and goldens/shed/codex.json the same paths gone; no cross-agent snapshot or table changed.
- Slices (toolpath, toolpath-convo, toolpath-codex, toolpath-goldens, path-cli --lib, path-cli --features develop --test goldens), fmt, clippy -D warnings (workspace, nix clippy 1.97.1), qualifier: see the report.
- NOT run: path-cli integration tests, pinned 1.94, gitleaks, nix, live codex resume.

## For the next PR
Group 4: rate_limits, token `info`, timings (`started_at`, `completed_at`, `duration_ms`, `time_to_first_token_ms`), `model_context_window`, `collaboration_mode_kind`, exec/patch result fields.
