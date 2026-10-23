# fix(codex): a codex -> codex round trip keeps the compaction (replacement_history) and the base instructions

Status: draft · renumbered on landing (author label pr/102) · Base: pr/103 · Source: pr/99 recommendation 1, group 2; controller-core's order

## Why
After pr/102 codex-forced still lost 48 paths on codex -> codex. Group 2 is the compaction Codex wrote (`compacted` line: `message`, `replacement_history[]` with role, content, encrypted_content) and
the session's own system prompt (`session_meta.base_instructions.text`). The reader keeps both as events (`compacted`, `session_meta`); the writer ignored them. Codex holds both: a writer bug.

## What changes (toolpath-codex/src/project.rs, Codex-recorded views only)
- `codex_passthrough` also returns the `compacted` events as `compacted` lines, payload verbatim, in event order (same placement rule as pr/102).
- `make_session_meta_line` takes `base_instructions` from the view's `session_meta` event.
- Other harnesses' views get neither (tested). The shared compaction shape of #108 is not touched: this writes back what Codex itself wrote.

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `codex_compaction_and_base_instructions_are_written_back` failed (no `compacted` line), passes; `other_harness_compaction_and_base_instructions_are_not_copied` guards the other direction.
- shed@codex-forced: lost 48 -> 42 key paths; shed@codex and goldens/shed/codex.json: the same paths gone; no cross-agent snapshot or table changed.
- Per-crate slices, fmt, clippy -D warnings (workspace, all targets, nix clippy 1.97.1), qualifier, `path-cli --features develop --test goldens`: see the report.
- NOT run: full workspace tests in one go (killed at the 30 min limit last time; run in slices instead), pinned 1.94, gitleaks, nix, live codex resume.

## For the next PR
Groups 3-4: current_date, timezone, workspace_roots (turn_context); rate_limits, token info, timings (event steps).
