# fix(gemini): read gemini-cli 0.47's `session-*.jsonl` record stream

Status: planned (asked 2026-10-19 on the goldens demo deck: "do we have a fix for these two? … separate prs in jj") · Base: the goldens stack once merged · Sibling: pr/20 (cursor), independent

## Why
gemini-cli 0.47 writes `chats/session-<ts>-<id8>.jsonl`, an append-only record stream, and no longer the one JSON
document per file the adapter reads. `paths.rs` listed and resolved `.json` only and `read_chat_file` parsed one
document, so `path p derive gemini` and `resolve_main_file` saw nothing a 0.47 CLI wrote (qualifier b0a413e4; drawn
red on the transformation graph). The goldens pipeline, `cross_harness_matrix.rs` and `real_fixture_roundtrip.rs`
each assembled the stream themselves, three copies, and all three appended every message record — but 0.47 appends a
message again when its tool calls complete, so those copies doubled every assistant turn (the gemini goldens pinned
22 IR steps for 12 messages, and 945 reasoning tokens for 489) and ignored `$set`, so `last_activity` stayed at the
header's start time.

## What changes
- `toolpath_gemini::ConversationReader::read_chat_file` / new `parse_chat`: one JSON document as before, else the
  0.47 stream replayed as gemini-cli's `loadConversationRecord` does: metadata records merge into the header, message
  records upsert by id in first-seen order, `$set` merges (a `messages` array replaces), `$rewindTo` truncates; a
  non-JSON line is skipped as the CLI skips it; a message the type refuses, or a stream without `sessionId` and
  `projectHash`, is an error.
- `PathResolver`: lists, resolves (by stem and by inner `sessionId`, now in name order) and names `.json` and
  `.jsonl` chat files; a `.jsonl` twin wins over a same-stem `.json` (0.47 leaves a projected `.json` untouched and
  keeps writing the stream). Sub-agent chat files the same.
- The goldens pipeline, `cross_harness_matrix.rs` and `real_fixture_roundtrip.rs` call the adapter (one
  implementation). Pipeline qualifier records re-anchored (`--supersedes`); 0b188981 (the third copy) resolved fixed.
- `tests/jsonl_sessions.rs`: the real capture `test-fixtures/gemini/convo.jsonl` placed where 0.47 writes it —
  listed, resolved by stem and by `sessionId`, read (one message per id, last `$set` as `lastUpdated`), derived; a
  `.jsonl` twin beats a stale `.json`.
- Gemini goldens recaptured: 12 IR steps, `last_activity` 18:30:14.544Z, projections and round trips without the
  doubled turns. The accounting known-defect `gemini-total-usage-drops-breakdowns` stays (now 489, not 945).
- `docs/agents/formats/gemini.md`: the 0.47 record stream.

## Verification
`cargo test -p toolpath-gemini` (unit tests for upsert, `$set`, `$rewindTo`, `$set.messages`, missing identity;
`jsonl_sessions`); `cargo test --workspace --features path-cli/goldens`; fmt, clippy (`--all-targets -D warnings`,
with the feature), doc, shellcheck as `scripts/quality_gates.sh` runs them.

## Paths
```
crates/toolpath-gemini/src/reader.rs
crates/toolpath-gemini/src/paths.rs
crates/toolpath-gemini/src/.qual
crates/toolpath-gemini/tests/jsonl_sessions.rs
crates/toolpath-gemini/tests/real_fixture_roundtrip.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/snapshots/*@gemini*.snap
crates/path-cli/tests/cross_harness_matrix.rs
docs/agents/formats/gemini.md
```
