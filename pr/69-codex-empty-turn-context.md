# fix(codex): the writer opens a turn_context only for a turn it writes lines for

Status: draft · Base: pr/68 · Code + snapshots (projection claude/claude-session/cursor to codex, their twice_around, cursor round_trip and shed) + `fixtures/twice-around.json`

## Why
twice-around found it: the codex writer stamped a turn_context (the opening one, and one at each group change) for assistant turns
that `emit_assistant` writes no line for. The reader moves that turn_context, and its step id, to the next turn, so the second
pass differs from the first (7 pairs into codex), and cursor -> codex merged a step (56 -> 55).

## What
`assistant_writes_lines` (same condition as `emit_assistant`) gates both the opening turn_context's group (first assistant that writes) and
each group change. An assistant turn that writes nothing opens no group.

## Result
All the codex step/actor/timestamp/event_source_id/cwd/model/type drift is gone from twice-around; what remains for codex is the
declared turn_id renumbering and pi's model_provider (untraced, declared). Declarations for the fixed drift removed from the fixture.

## Open
Same dev-shell items as pr/64-68 (capture, qualifier re-anchor). No gitleaks / pinned toolchain run.

## Why it matters, in plain words (for the deck)
- What the writer did: when it wrote a Codex session it put a "new turn starts here" marker (a turn_context line) in front of every
  assistant turn, including turns it then wrote nothing for (an assistant turn with no text, no tool call, no thinking).
- Why only the second pass shows it: reading a Codex file back, the reader cannot attach a marker to a turn that has no lines, so it
  slides the marker (and the step id and timestamp that come with it) onto the NEXT turn. The first pass starts from the original
  recording, where the marker sits on the empty turn; the second pass starts from what the reader made, where it already moved. So pass 2
  differs from pass 1 once (7 pairs into codex; cursor -> codex lost a whole step, 56 -> 55).
- Why it does not accumulate: after the marker has moved to a turn that has lines, nothing is left to move; pass 3 equals pass 2.
- The fix writes the marker only for a turn that has lines. Measured after: no step, actor or timestamp drift into codex.
