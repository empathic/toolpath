# triage: why unexplained grew to 1,479 with the compaction recordings, codex-forced first (no code)

Status: analysis only · Base: pr/98 · Source: deck v52 item 13 · Data: docs/baseline/unexplained.md at pr/98, docs/baseline/classification.md, shed@codex-forced.snap

## What grew
Lost-unexplained went from 585 (63 pairs) to 1,479 (91 pairs). The 28 added pairs are the four forced recordings x 7 targets. One of them dominates: **codex-forced -> {claude, copilot, cursor, cursor-agent, gemini, opencode, pi}
= 789 of the 1,479 lost paths (110-120 per pair)**; the rest are the other three forced recordings and the earlier sets.

## What the codex-forced paths are (claude target; the other six differ by under 10)
Of the 114 lost paths for codex-forced -> claude:
- **48 `structural.tools[]...`**: the tool DEFINITIONS (name, description, JSON-schema parameters) a codex `turn_context` carries. 48 paths x 7 targets = 336, the cluster the deck names. Same 48 paths in every pair: one finding, not 336.
- **5 `structural.replacement_history[]...`**: the compacted history codex stores on a `compacted` event (role, content, encrypted_content).
- **6 `structural.changes.<absolute path>...` + 12 `change.<file>.raw/structural...`**: an `apply_patch` call's per-file map (keyed by the recording machine's absolute temp path) and the file artifacts derived from it.
- **1 `meta.files_changed[]`**, **6 `tool_uses[].input` / `arguments`**: `cmd`, `workdir`, `max_output_tokens`, `yield_time_ms`, `limit`, `query`. `cmd` is the target's `command` (the 1 gained path): a RENAME, not a loss.
- **~36 event-envelope fields** on codex's own event steps (`cli_version`, `originator`, `rate_limits.*`, `info.*_token_usage.*`, `model_context_window`, `timezone`, `current_date`, `realtime_active`, `thread_source`, `started_at`/`completed_at`/`duration_ms`, ...): pr/85's step-level finding again.

## Is it holdable? The same-format round trip answers it
shed@codex-forced (codex -> codex) loses 99 of 152 source key paths. It loses the same 48 tool-definition paths, the 5 replacement_history paths, `base_instructions.text`, `current_date`, `timezone`, `workspace_roots[]`,
`rate_limits.*`, the token `info.*`, `started_at`/`completed_at`/`duration_ms`/`time_to_first_token_ms`, `thread_source`, `multi_agent_version`, `realtime_active` and the exec/patch result fields (`exit_code`, `duration`, `aggregated_output`, `parsed_cmd[]`, `changes.*`).
Codex's own format holds every one of them, so for codex -> codex each is a **writer bug** (a value the format can hold), not a declaration. This is the "codex -> codex further losses" list item 1 left as candidates; items 2 and 3 are done, so it can start.

## Recommendation (in order; each its own piece)
1. **codex -> codex first** (fixes the bulk at the source): restore `tools`, `base_instructions.text`, `collaboration_mode_kind`, `current_date`, `timezone`, `workspace_roots[]`, `thread_source`, `multi_agent_version` on the `turn_context`/session_meta the writer already emits (the pr/100 path), keeping them as raw JSON like pr/100;
   then the event steps (`rate_limits`, token `info`, timings); then `replacement_history` with the compaction item (needs the shared compaction shape: #108). Test first on codex-forced.
2. **Declare the rest once**, for the other six targets, only after 1: the tool definitions have no home in claude/copilot/cursor/gemini/opencode/pi transcripts (none records the tool list); list `tools[]` under codex in `fixtures/source-bookkeeping.json` as source bookkeeping (48 paths, one reason). The envelope fields join the pr/85 "no home" group.
3. **Declare `cmd` -> `command` as a rename** in the targets' contracts (it is already written, under the other name), and check `workdir`/`yield_time_ms`/`max_output_tokens` (codex-only: bookkeeping).
4. **Needs a look**: `message`, `summary`, `last_agent_message`, `stdout`/`stderr` on event steps (text echoes; the text total is unchanged, so likely duplicates of the assistant/tool text).

## Finding: a machine path in a key
`structural.changes.<absolute temp path>.content` puts the recording machine's `/var/folders/.../T/toolpath-elicit.XXXXXX/codex/...` into a KEY path, so it appears in docs/baseline/unexplained.md and shed@codex-forced.snap. No username or credential, but it is machine-specific and the key differs per recording.
`keypaths` should generalise a variable key (`changes.<path>`) the way `<artifact>` already generalises file names; otherwise a re-recording churns every table. Not changed here.

## Not run
Read-only analysis of the generated tables and shed snapshots (counts by grep from the committed docs); no code, no gates, no snapshots changed.
