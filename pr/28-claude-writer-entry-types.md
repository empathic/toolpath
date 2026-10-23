# fix(claude): write only Claude Code entry types; write message.stop_reason

Status: planned (asked 2026-10-19: fix the two known defects in the Claude writer) · Base: the goldens stack once merged · Independent of pr/19–27 (the goldens files it recaptures follow its code)

## Why
Two pinned known defects, both in `crates/toolpath-claude`:

- **codex-to-claude-illegal-types.** `project.rs::project_event` wrote every non-preamble IR event as a Claude entry
  with the event's raw `event_type` as `type`. Codex's events (`session_meta`, `turn_context`, `task_started`,
  `user_message`, `agent_message`, `token_count`, `task_complete`) are codex's wire vocabulary, so codex -> claude
  wrote 34 of 44 lines with types Claude Code never emits (pinned: agent_message 13, session_meta 2, task_complete 1,
  task_started 2, token_count 13, turn_context 1, user_message 2), all text-less shells chained after the last turn,
  so the file's leaf was a `task_complete` line. Copilot -> claude did the same with `session.model_change` and
  `system.message` (2 lines; not pinned). Their text was never written: it already rides on the view's turns.
- **claude-roundtrip-renames** (qualifier 6a02e92f). `types.rs::Message` is `rename_all = "camelCase"`, so
  `stop_reason` was written as `message.stopReason` (and `stop_sequence` as `stopSequence`). `message` is the
  Messages API object, snake_case; the envelope's `stopReason` is a different, hook-provided field
  (docs/agents/formats/claude-code/jsonl-envelope.md).

## What changes
- `crates/toolpath-claude/src/project.rs`: `CLAUDE_EVENT_ENTRY_TYPES` (the entry types of
  docs/agents/formats/claude-code/entry-types.md) and `is_claude_event`; a non-preamble event of any other type is
  not written, and any entry whose `parentUuid` named a dropped event is re-linked to its nearest written ancestor.
  Preamble lines (`data.raw`) and `tool_result_user` events are untouched. Tests: the `codex` golden read through
  `toolpath_codex` and projected writes only Claude types, keeps every assistant text and every tool use, and leaves
  no dangling `parentUuid`; a child of two chained foreign events re-links to the user turn.
- `crates/toolpath-claude/src/types.rs`: `Message.stop_reason` / `stop_sequence` serialise as `stop_reason` /
  `stop_sequence`; `stopReason` / `stopSequence` stay read aliases (files older toolpath wrote). Test: every
  `message.stop_reason` line of the `claude` golden is written back under that name, never `stopReason`.
- `crates/toolpath-claude/Cargo.toml`: dev-dependencies `toolpath-golden`, `toolpath-codex` (tests from goldens,
  rule 10; codex does not depend on claude).
- Known defects retired: both `Defect`s removed from `crates/path-cli/src/goldens.rs` (with `illegal_types`,
  `CLAUDE_TYPES`, `renamed_fields`, `judge_counts`, `judge_renames` and the rename-judge test, now unused;
  `DEFECTS` is empty after pr/22, `Verdict` carries an `expect(dead_code)` with the reason) and from
  `crates/toolpath-goldens/src/defects.rs`: with pr/27's accounting three gone these were the last two, so `DEFECTS`
  is empty; `Target` carries an `expect(dead_code)` with the reason, the measures (`illegal_claude_types`,
  `round_trip_renames`) keep their unit tests, and the set-filter test now asserts a set with no open defect
  measures nothing. `known_defects@<set>.snap` is `[]` for every set. `goldens/known-defect/` is gone.
- Qualifier: `resolve` (reason fixed) of a7578bee (illegal types) and 6a02e92f (renames) on
  `toolpath-claude/src/project.rs`; the defects.rs concern ("two defects pinned", d1465c82) resolved: none is pinned; the capture-live
  `--allow-all-tools` concern re-anchored (lines moved, content hash unchanged).
- `docs/GOLDENS.md`, `goldens/DEMO.md`: no file-pinned defect is open.

### Goldens recaptured (`path goldens capture --all`, `shed --json`, `INSTA_UPDATE=always cargo test -p toolpath-goldens`)
- `codex -> claude`: 44 -> 10 lines (user 7, assistant 2, permission-mode 1). Round trip: steps 43 -> 9 (the 34
  event steps), text 13445 chars and 1 tool use unchanged; the lost key path `structural.cwd` is the dropped
  events' cwd (every written user/assistant line still carries `cwd`).
- `copilot -> claude`: the two copilot event lines gone (26 -> 24 lines; round trip steps 15 -> 13).
- `pi -> claude`, `opencode -> claude`: `stopReason` -> `stop_reason` on every assistant line.
- Shed: `renamed: []` and `message.stop_reason` no longer dropped in claude, claude-session, claude-compacted.
  `shed --json` also refreshes `string_chars`/`bytes` for `-> pi` and the copilot/pi IR rows that were already
  `info` at the base (grown before this change, unrelated); `projection@*` for codex/pi/copilot targets and every
  claude -> X golden are byte-identical.
- insta headers: `input_file` now names `test-fixtures/<agent>/convo.jsonl` (the glob that runs the set; same
  `input_sha256`).

## Verification
- `cargo test -p toolpath-claude` (new tests included), `path goldens check`: all ok, qualifier review 17 fresh.
- `docs/model/model.json` is NOT regenerated here: generating at this tip (pathbase 1d5bc39) reports
  `piece:toolpath/27 names commit "<a pre-squash sha>" which is not on the revision` (pr/27's record cites a pre-squash
  commit) and `toolpath-model`'s committed-model test then fails. The model stays at 79003bc; regenerate once pr/27's
  record is fixed (known-defect findings drop to 0 with this piece).
- Gates as `scripts/quality_gates.sh`: fmt, clippy `--workspace --all-targets --features path-cli/goldens -D warnings`,
  `RUSTFLAGS=-D warnings cargo test --workspace --features path-cli/goldens`, doc `-D warnings`, shellcheck.
- Resume: codex -> claude is covered by `cross_harness_matrix` and the resume tests in that run.
- Capture ran a copy of this tree's `path` binary: the shared `target/` is rebuilt by other worktrees, and cargo's
  mtime freshness can count another worktree's build as fresh for this one (a first capture recorded someone else's
  copilot change); sources were touched before each build and the output checked for the fix.

## Paths
```
crates/toolpath-claude/src/project.rs
crates/toolpath-claude/src/types.rs
crates/toolpath-claude/src/.qual
crates/toolpath-claude/Cargo.toml
crates/toolpath-goldens/src/defects.rs
crates/toolpath-goldens/snapshots/known_defects@claude.snap
crates/toolpath-goldens/snapshots/known_defects@codex.snap
crates/toolpath-goldens/snapshots/projection@codex@claude.snap
crates/toolpath-goldens/snapshots/projection@copilot@claude.snap
crates/toolpath-goldens/snapshots/projection@opencode@claude.snap
crates/toolpath-goldens/snapshots/projection@pi@claude.snap
crates/toolpath-goldens/snapshots/round_trip@codex@claude.snap
crates/toolpath-goldens/snapshots/round_trip@copilot@claude.snap
crates/toolpath-goldens/snapshots/shed@claude-compacted.snap
crates/toolpath-goldens/snapshots/shed@claude-session.snap
crates/toolpath-goldens/snapshots/shed@claude.snap
crates/toolpath-goldens/snapshots/shed@codex.snap
crates/toolpath-goldens/snapshots/shed@copilot.snap
crates/toolpath-goldens/snapshots/shed@opencode.snap
crates/toolpath-goldens/snapshots/shed@pi.snap
goldens/codex/to-claude.jsonl
goldens/copilot/to-claude.jsonl
goldens/pi/to-claude.jsonl
goldens/roundtrip/codex-claude.json
goldens/shed/
pr/28-claude-writer-entry-types.md
```
