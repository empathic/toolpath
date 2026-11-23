# fix(copilot): the reader keeps system.message

Status: draft · Base: pr/69 · Code + 19 snapshot files (copilot ir/document/shed, copilot -> every projection and round trip, codex -> copilot) + `fixtures/twice-around.json`

## Why
The writer emits `system.message` for a `Role::System` turn (project.rs:126) but the reader had no case for it (types.rs
`CopilotEvent::event`), so it came back as an unknown event and the next pass dropped it. Developer text was lost (codex -> copilot:
-4 key paths, 8 -> 6 steps). Found by twice-around. The recording itself (test-fixtures/copilot) contains a system.message, so the
Copilot IR gains a leading system turn.

## What
`EV_SYSTEM_MESSAGE`, `CopilotEvent::SystemMessage`, and a provider case that makes a `Role::System` turn with the text. Unit test for the
classification; the roundtrip test now expects system, user, assistant turns.

## Result
Every copilot drift in twice-around except the three by-design minted-id paths (graph.id, path.id, title) is gone; declarations removed.

## Open
Dev-shell items as pr/64-69. The extra system turn changes copilot's IR, document and every projection FROM copilot: review the
snapshot diff (leading system turn) before signing.

## Why it matters, in plain words (for the deck)
- What the writer and reader did: the writer wrote developer/system text as a `system.message` line, but the Copilot reader had no rule
  for that line type, so it kept it only as a generic "unknown event" and not as a turn with text.
- Why only the second pass shows it: the first read keeps the line as an unknown event, so the first derived document still carries
  the text inside it. The second write starts from the IR, which has no system turn, so it writes no system.message and the text is gone
  (codex -> copilot lost 4 key paths and 2 steps; developer text was lost).
- Why it does not accumulate: once the text is gone there is nothing more to lose; pass 3 equals pass 2. But the loss is real: it is
  permanent after one round. That is why this was a bug to fix, not a drift to declare.
- The reader now makes a system turn, so the text survives every pass.
