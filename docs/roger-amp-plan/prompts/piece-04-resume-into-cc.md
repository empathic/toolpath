# Piece 04 — resume-into-cc (goal level 3)

You are a fresh Claude Code session building one piece of the Amp harness for toolpath. Self-contained; execute end to end. One ⚠ step (live `claude -r`).

## Context to load first

`CLAUDE.md`; `roger-amp-plan/PLAN.md` (Piece 04); `roger-amp-plan/BUILD_LOG.md`; `crates/path-cli/tests/resume.rs` + `tests/support/mod.rs` (`make_convo_path:134`, `args_explicit:182`); `crates/toolpath-amp/src/provider.rs` (`tool_category`); `docs/agents/formats/claude-code/` if rendering questions arise.

**Assumed done:** 00–03 (tags through `amp-m3`).

## Scope

Amp-shared doc → Claude Code. Existing machinery does the work; this piece proves and hardens it: add `tests/resume.rs::file_input_amp_source_into_claude_projects_and_records_exec` (amp-actor convo path + `Harness::Claude`, RecordingExec asserts `claude -r <fresh-id>` + on-disk JSONL under scoped `$HOME`); run the real feature-elicit-derived doc through `extract_conversation` → claude projection and fix any `tool_category`/name-mapping fallout (extend arms + unit tests — changes live in `toolpath-amp`, not claude code). ⚠ Live: `path resume <amp-shared input> --harness claude -C <dir>` → probing question inside real `claude -r`.

### Out of scope

CC→amp direction, matrix row (both 05); bookkeeping (06).

## Definition of done

Probing-question pass in real `claude -r`; new test green; gates green; BUILD_LOG entry. Tag `amp-m4`. Stop.
