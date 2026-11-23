# fix(codex): a codex -> codex round trip keeps permission_profile and file_system_sandbox_policy

Status: draft · renumbered on landing (author label pr/95; clashes with pr/95-claude-thinking-signature) · Base: pr/94 · Source: deck v52 item 1

## Why
The writer restored `approval_policy`, `sandbox_policy`, `personality` and `collaboration_mode` on each `turn_context` it wrote, but not the two other permission settings Codex states there:
`permission_profile` (8 paths) and `file_system_sandbox_policy` (6 paths): 14 paths lost in codex -> codex that the format can hold. A value the format can hold is a writer bug.

## What changes
`TurnSettings` (toolpath-codex/src/project.rs) also reads the two settings from the view's first `turn_context` event and writes them back on every `turn_context`, as the JSON Codex wrote
(no new typed struct: their inner shape is Codex's and changes between Codex versions). Still only for a view Codex recorded; other harnesses' views get none (tested).

## Verification
- Test first: `codex_session_keeps_its_turn_settings` now asserts both settings (the fixture event carries them); it failed (`Null` vs "managed"), passes now. `other_harness_settings_are_not_copied` also checks the two keys.
- shed@codex and shed@codex-forced no longer list the 14 paths; no other snapshot changed (cross-agent projections never carried them).
- Sandbox, cargo 1.97.1: fmt, clippy -D warnings, `baseline check`, qualifier, toolpath-goldens tests, workspace tests (only the 31 sandbox port failures): see the report.

## For the next PR
The other codex fields that shed@codex still lists for codex -> codex (effort, current_date, base_instructions, rate_limits, token info, ...) are the next candidates; each needs the same look (held by the format, or a declared loss).
