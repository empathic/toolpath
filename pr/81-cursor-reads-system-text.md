# fix(cursor): the IDE reader reads system-role text back from the model history

Status: draft · Base: pr/77 · Code (toolpath-cursor types.rs, provider.rs, reader.rs, agent_store.rs) + 2 snapshots

## Why (plain words)
pr/77 made the Cursor IDE writer keep system-role text in the model history (`conversationState` lists the hashes of
`agentKv:blob:<sha256>` messages), but the reader read bubbles only and no bubble draws a system turn. So codex -> cursor and
copilot -> cursor still came back short: 3077 of 13445 and 4594 of 30034 text chars. pr/77's "Known gap".

## What
- `ComposerData::model_history_hashes` decodes `conversationState` (`"~"` + base64 of a protobuf, field 1 = 32-byte hashes);
  `CursorSession::model_messages` parses each listed blob.
- `session_to_view` turns each system message into a System turn (`system-<i>`, as the cursor-agent reader names them), placed
  before the turn the next user or assistant message names (the k-th user turn with text, or the assistant turn with that id), or
  at the end; parents are re-chained in order. A composer with no history blobs (every real fixture today) reads as before.
- `DbReader::load_session` now loads the `agentKv:blob:` rows its `conversationState` lists, so a store on disk reads the same.
- One `system_text` helper shared with the cursor-agent reader; the test-only base64 decoder in project.rs now uses the crate's.
- docs/agents/formats/cursor.md: the reader rule.

## Snapshots
- `round_trip@codex@cursor`: steps 6 -> 8, text_chars 3077 -> 13445 (= source), key_paths 25 -> 26;
  `paths[].meta.actors.<actor>.provider` no longer lost.
- `round_trip@copilot@cursor`: steps 12 -> 13, text_chars 4594 -> 30034 (= source), key_paths 57 -> 58; the same path no longer lost.
- Nothing else moved: projections, writer@cursor and shed are unchanged (the writer is unchanged); `capture --all` moved no output sha.

## Verification
Unit tests: a projected view with two system turns reads back in place with a straight parent chain; a DB with a
`conversationState` and its blobs loads them and reads the system turn first. Gates: see the commit's run (fmt, clippy -D warnings,
workspace tests, doc, secrets + shellcheck, qualifier review 0 drifted, just baseline). One qualifier record re-anchored
(toolpath-cursor provider.rs, the producer.version comment moved one line).

## Note
On a real Cursor 3.24 store the history's first blob is Cursor's own system prompt; it now reads back as a System turn too, as the
cursor-agent reader already does for the CLI's.
