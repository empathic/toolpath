# TUI drift check

A manual, per-harness-release verification loop. It is the only check that
exercises the real harness loader and renderer — the surfaces our unit,
fixture, and matrix tests cannot see. Run it when a harness ships a new
version, or before claiming resume fidelity for a provider.

Every level of automated testing here has missed defects the level above it
caught: fixtures passed while real sessions failed, the real-session matrix
passed while wire-level diffs failed, and wire-level identity passed while
the actual TUI rendered a resumed session blank (Claude 2.1.216 aborts its
transcript renderer on an assistant entry whose content is
`[{"type":"text","text":""}]` — invisible to every oracle below the real
renderer). This loop is the top level.

## The loop, per harness

Drive it from tmux (`tmux new-session -d -s probe -x 200 -y 50`,
`send-keys` / `capture-pane`) so every keystroke and every rendered frame
is scripted and inspectable. Rules learned the hard way: verify the pane
is a shell prompt before sending shell commands (a missed exit types your
command into the chat and contaminates the session); never reuse the
session-under-test's id; copy session files before importing them
(mid-write reads catch flush boundaries).

1. **Create a real session interactively.** Small but non-trivial: a
   prompt that triggers a tool call that writes a file, then `/compact`
   (or the harness's equivalent), then a post-compaction question that can
   only be answered from preserved context.
2. **Exit the harness cleanly** and copy the on-disk session out.
3. **Import** (`path p import <harness> …`) and check the stderr line —
   `events preserved:` names every event type that rode through as a
   generic event; a compaction-ish name in that list means an unmapped
   encoding (this is exactly how Copilot's `session.compaction_start` /
   `complete` shipped unnoticed as `compactions=0`).
4. **Oracle**: derive → extract → derive stability plus
   `toolpath_convo::testing::{check_view_invariants, assert_fixpoint}`,
   looped ~24×. Single passes can succeed by hash-order luck — the pi
   kept-run loss reproduced in only ~1 run in 4.
5. **Wire diff**: project back and diff the file (line count, entry-type
   sequence, per-entry field diff against the native original). Every
   difference is either a bug or a documented deliberate loss — no third
   category.
6. **Resume the projection in the real harness** (`path resume <doc>
   --harness <h> -C <fresh-dir>`) and compare the rendered transcript
   against a resume of the native session: prompts, tool rows, the
   compaction marker, model/thinking state, session title in the picker.
7. **Clean up**: remove projected sessions and any state rows the harness
   indexed for them.

## Version log

| Harness | Last verified | Notes |
|---|---|---|
| claude | 2.1.216 (2026-07) | Transcript renderer aborts on empty text blocks; compact summary must be `isVisibleInTranscriptOnly`. |
| codex | 0.144.4 (2026-07) | "Context compacted" row renders from `event_msg`/`context_compacted`; thread title from first user message after `turn_context`; summary now encrypted in `replacement_history`. |
| copilot | 1.0.68 (2026-07) | Compaction pair observed and mapped; loader contract in `docs/agents/formats/copilot-cli/writing-compatible.md`. |
| opencode | 1.18.4 (2026-07) | Clean: manual compact round-trips; divider renders on projected resume. |
| pi | 0.72 (2026-07) | Restores model from last assistant's provider+model pair and replays `model_change`/`thinking_level_change` entries. |
| gemini | — | Persists no compaction; base visual loop not yet run. |
| cursor | — | IDE store, no CLI TUI; verified via state.vscdb round-trip only. |
