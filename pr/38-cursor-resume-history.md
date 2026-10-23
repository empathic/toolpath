# fix(cursor): write the model history Cursor 3.24 reads, so a resumed chat carries its transcript

Status: draft · Base: main (independent; touches toolpath-cursor and path-cli's cursor projection only)

## Why
Seen 2026-10-19 in Cursor 3.24.12. Bobby ran `path resume --harness cursor` on a Copilot session (composer
7a80f0ee-0c9f-4e1a-acbd-d655a5721cfe, 12 bubbles). Cursor's Agents window listed the chat, but opening it showed only the
empty new-chat view. He typed "this works?", and Cursor's agent answered: "No. This chat does not have the Copilot
session as prior turns … What I actually got is a new conversation whose first user message is `this works?`".

Cause: I compared a native 3.24 chat with the projected one, field by field, on a read-only copy of `state.vscdb`.
- What Cursor 3.24 sends the model is not the bubbles. It is the list in `composerData.conversationState`: `"~"` plus
  base64 of a protobuf whose field 1 repeats, once per message in order, each holding a 32-byte SHA-256.
- Each hash points at an `agentKv:blob:<sha256 hex>` row, which holds one AI SDK `ModelMessage` as JSON.
- The projector wrote neither. It also stamped `_v:16` and `agentBackend:"cursor-agent"` and left out `hasLoaded`,
  where a native chat has `_v:18` and `hasLoaded:true`, and has no `agentBackend`.

## What changes
- toolpath-cursor:
  - `CursorSession.agent_blobs` (hash → JSON) and `reader::AGENT_BLOB_PREFIX`.
  - The projector builds one user / assistant / tool message per turn. Assistant messages carry reasoning, text and
    tool-call parts; tool results become `tool` messages.
  - It keys each message by the SHA-256 of its bytes, writes `conversationState`, and stamps `_v:18` and
    `hasLoaded:true`.
  - `agentBackend` is set only when the config asks for one.
- path-cli `projection/cursor.rs`: writes the `agentKv:blob:` rows next to the bubbles. The export message counts them.
- `docs/agents/formats/cursor.md`: new section "What the model sees (Cursor 3.24)"; corrected the `agentKv` row.
- `README.md:41` and `.qual`: the Cursor row now says what was seen. A concern records that the live check is pending.

## Verification
- New unit tests `conversation_state_lists_the_history_blobs_in_order` and `base64_matches_known_vectors`. The first
  checks that the state decodes to one 34-byte entry per message, that each hash is the sha256 of its blob, and the
  roles user / assistant / tool, with the tool-call part present.
- `composer_metadata_populated` now asserts `_v:18`, `hasLoaded` and no `agentBackend`.
- ~~`cargo test -p path-cli --features path-cli/goldens`: no snapshot changed.~~ Correction: snapshots re-recorded in
  3e8f7dfceb67; the earlier "no snapshot changed" was wrong. That command does not run the baseline;
  `cargo test -p toolpath-goldens --test goldens` does, and it failed on every recording (see *Snapshot review*).
- Two qualifier records whose lines shifted (cursor.rs, reader.rs) were re-anchored with superseding copies.
- Not yet verified: that Cursor 3.24.12 renders and sends the projected history. That needs a live check against
  Bobby's Cursor. Still open: the other `conversationState` fields, `providerOptions`, and the
  `~/.cursor/projects/<slug>/agent-transcripts/<id>.jsonl` a native chat also writes.

## Snapshot review (re-recorded in 3e8f7dfceb67)
26 snapshots, every difference one of this piece's four changes; no bubble, message text or history order moved.
- `projection@*@cursor` (8): composerData `_v` 16 → 18, `hasLoaded: true` added, `agentBackend: "cursor-agent"`
  removed, `conversationState` written; a new top-level `agent_blobs` map. Checked by script on every one: each key is
  the sha256 of its value, the state decodes to one 34-byte entry per message whose hashes all resolve (codex: 7
  entries over 6 blobs, two identical user messages share one), roles alternate as the bubbles do, and every text and
  reasoning part of a blob appears verbatim in a bubble.
- `round_trip@*@cursor` (8) and `writer@cursor`: `key_paths` −1, `paths[].meta.producer.version` lost (or, where the
  source had none, no longer gained). The cursor reader maps `agentBackend` to `producer.version`
  (`provider.rs:360`), and the projector no longer writes it. Accepted; qualifier comment d1487b05 on that line.
- `shed@<recording>` (8, every harness): only the `cursor` projection row moves (lines, bytes, string_chars up). Lines
  grow by blob count + 3 (`agent_blobs` open/close, `conversationState`, `hasLoaded`, minus `agentBackend`), e.g.
  claude 5424 → 5480 = 53 blobs + 3. Not pi-provider (pr/40) fallout: no IR, document or other projection row moves,
  and the `meta.actors.<actor>.provider` losses are unchanged context.
- `shed@cursor` same-format trip: key paths after 70 → 101, lost 104 → 103, gained 0 → 30. `data.conversationState`
  and `data.hasLoaded` are no longer dropped, `data.agentBackend` now is; the 30 gained are all
  `agent_blobs.<sha256>` (checked against the same-format output).
- No `known_defects`, `ir`, `document` or `validate` snapshot changed.
- pr/53 (e8b4ea7) had committed 20 `.snap.new` files: insta's pending copies of 20 of these snapshots, each body equal
  to the re-recorded `.snap`, read by no test. Deleted here; `.gitignore` now excludes `*.snap.new` and
  `*.pending-snap` (its hunk is this piece's; the rest of `.gitignore` stays in HOLD).

## Paths
```
crates/toolpath-cursor/src/types.rs
crates/toolpath-cursor/src/reader.rs
crates/toolpath-cursor/src/project.rs
crates/path-cli/src/projection/cursor.rs
docs/agents/formats/cursor.md
README.md
.qual
crates/path-cli/src/projection/.qual
crates/toolpath-cursor/src/.qual
crates/toolpath-goldens/snapshots/projection@*@cursor.snap (per hunk: the hunks above)
crates/toolpath-goldens/snapshots/round_trip@*@cursor.snap (per hunk)
crates/toolpath-goldens/snapshots/writer@cursor.snap (per hunk)
crates/toolpath-goldens/snapshots/shed@*.snap (per hunk: the cursor rows)
.gitignore (per hunk: the *.snap.new lines)
```
