# fix(opencode, cursor, cursor-agent): system-role text survives; fix(cursor): every tool call survives

Status: draft · Base: pr/76 · Source: the map measurement (addendum 22:45Z, defects 1 and 2)

## Why (plain words)
1. codex and copilot recordings written into opencode, cursor and cursor-agent lost exactly their system-role text (6 pairs): the
   writers skipped system turns outright. Content, so the writers are fixed, not declared.
2. copilot -> cursor lost one tool call (11 -> 10): a Cursor bubble holds one tool call and the writer wrote only a turn's first.

## What
- opencode: system text rides on `UserMessage.system` of the user message it precedes (several join with a blank line; text after the
  last user message goes on the last one); the reader turns `system` back into a System turn before that user turn.
- cursor and cursor-agent: the shared model history now writes `{"role":"system","content":text}`; the cursor-agent reader reads it
  (string or text parts) back as a System turn.
- cursor IDE: a turn's 2nd..nth tool calls each get a bubble of their own (`<turn id>-tool-<k>`), as Cursor writes them.
- contract/cursor.json: `tool_uses[].input.targetFile` declared (the writer copies the read path there, keeping the original key).

## Result
Text chars now equal the source in the opencode and cursor-agent round trips (e.g. codex 3077 -> 13445); copilot -> cursor tool_uses 11 -> 11.

## Known gap, not fixed here
The cursor IDE READER reads bubbles only, so the system text the IDE writer now puts in the model history (conversationState) is still not
read back: codex/copilot -> cursor still drops it on the way back. Needs a reader change or a declared loss; Bobby's call.
Fixed by pr/81 (the reader reads the history's system messages).

## Not run
Pinned 1.94, gitleaks, nix-develop recapture.

## Addendum
The cursor reader folds a bubble whose id is `<previous turn id>-tool-<k>` back into that turn (tool uses and file mutations), so a re-read
has the shape of the first and the cross-harness matrix's idempotence check (copilot -> cursor, thinking by assistant index) still holds.
One qualifier record re-anchored (toolpath-cursor provider.rs).
