# fix(codex): a Codex session projected back to Codex keeps its turn settings

Status: planned (ruling 2026-10-19, design consultant: "Codex keeps its own settings same-harness") · Base: the goldens stack once merged

## Why
The Codex writer wrote every `turn_context` line with `approval_policy`, `sandbox_policy`, `personality` and
`collaboration_mode` empty, even when the session came from Codex and the document still had them (they survive derive
as the `turn_context` event's data). `shed@codex` pinned all of them as dropped on the same-format round trip.

## What changes
- `crates/toolpath-codex/src/project.rs`: `TurnSettings::from_view` reads the four settings from the view's first
  `turn_context` event, only when `view.provider_id == "codex"`; `make_turn_context_line` writes them on every
  `turn_context` line. Another harness's view gets none: those settings mean nothing to Codex, and what a cross-harness
  projection loses stays declared in the writer contract (`fixtures/contract/codex.json`, pr/54-writer-contracts).
- Tests: `codex_session_keeps_its_turn_settings`, `other_harness_settings_are_not_copied`.

## Snapshots
- `shed@codex`: same-format `lost` 69 → 58, `after` 37 → 48; the 11 key paths no longer dropped are
  `payload.approval_policy`, `payload.collaboration_mode.{mode, settings.developer_instructions, settings.model,
  settings.reasoning_effort}`, `payload.personality`, `payload.sandbox_policy.{exclude_slash_tmp,
  exclude_tmpdir_env_var, network_access, type, writable_roots[]}`; same-format bytes 20136 → 20488. Nothing else in it
  changed. Its `projections.cursor` stats are left as pinned: the cursor snapshots are being re-recorded separately.
- `goldens/shed/codex.json` not refreshed: fewer drops are info in `path d baseline check`, and refreshing it would also
  take the new cursor stats.
- No other snapshot changes (`writer@codex` derives from `fixtures/full-view.json`, which is not a Codex view).

## Still dropped same-format (not in the ruling)
`payload.file_system_sandbox_policy.*`, `payload.collaboration_mode_kind`, `payload.current_date`, `payload.timezone`
and the other `turn_context` keys Codex added outside the typed `TurnContext` fields.
