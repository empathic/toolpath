# fix(copilot): a copilot -> copilot round trip keeps the sub-agent's assistant.message events

Status: draft · Base: pr/100 · Source: deck v52 item 2

## Why
copilot -> copilot wrote back 11 of the source's 13 `assistant.message` events. The two missing are the sub-agent's own messages (`parentToolCallId` = the `task` call, turnIds "0" and "1"):
the reader merged their text, reasoning and tokens into the dispatching turn (so the parent turn's text carried "16 words ..." that the sub-agent said), and the writer never wrote a delegation's turns.
The IR has a place for them (`delegations[].turns`) and the Copilot format holds them; a value the format can hold is a writer bug. Copilot's own contract had declared the nine `delegations[].turns[]` fields lost ("no record of a sub-agent's own turns"); that was wrong.

## What changes
- `MessageEvent.parent_tool_call_id` (types.rs); the reader routes an `assistant.message` whose `parentToolCallId` names a delegation into that delegation's `turns` (id `<toolCallId>-<n>`, text, reasoning, model, usage).
- Tokens: the dispatching turn keeps the sub-agent's usage too, so steps still sum to the session total (the goldens accounting contract); the delegation turn carries its own figure and the writer subtracts it from the parent message, so each message gets back its own `outputTokens` (tested).
- The writer emits each delegation turn as an `assistant.message` with `parentToolCallId` between `subagent.started` and `subagent.completed`.
- Contract: the nine `delegations[].turns[]` drops removed from fixtures/contract/copilot.json. Snapshots, goldens/ (capture --all, shed) and docs/baseline/classification.md regenerated.
- One qualifier comment re-anchored (provider.rs, line moved).

## Verification (sandbox, cargo 1.97.1, offline)
- Test first, both red before the fix: `a_sub_agents_messages_are_the_delegations_turns_not_the_parents_text` (0 vs 2 delegation turns), `copilot_to_copilot_keeps_every_assistant_message` (11 vs 13). Green now; the second also compares every message's outputTokens.
- toolpath-copilot and toolpath-goldens tests: pass. `path-cli --features develop --test goldens` (the baseline check): pass. qualifier review: 238 fresh, 0 drifted. fmt: clean.
- Workspace: the only failures are the known sandbox port-bind ones (no new failure).
- Effect on other targets: copilot -> X no longer carries the sub-agent's reply inside the parent's text; the nine fields are now counted as dropped by every target's contract (classification.md: copilot rows, lost dropped +10 each, unexplained unchanged). The sub-agent's own tool calls (`tool.execution_*` with `parentToolCallId`) still attach to the dispatching turn; not changed here.
- clippy -D warnings (workspace, all targets, nix-store clippy 1.97.1, run on pr/96's tree after the tip was reported): clean.
- NOT run: pinned 1.94 toolchain, gitleaks, nix, live copilot.

## For the next PR
Sub-agent tool calls could belong to the delegation's turns as well; not needed for the counts and left alone.
