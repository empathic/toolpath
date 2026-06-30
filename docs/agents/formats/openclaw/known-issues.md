# Known issues and gotchas

Format-level quirks, ambiguities, and things to defend against. Because we
have **no first-hand on-disk sample** yet (see
[README.md §Sourcing](README.md#sourcing-and-confidence)), some of these are
"from the producer type, not confirmed against a real file" — flagged where
so.

## Two timestamp encodings per line

A `message` entry carries an **ISO-8601 string** `timestamp` at the entry
level and an **epoch-milliseconds number** `timestamp` inside the message
object. Mixing them up yields 1970-era or unparseable dates. See
[messages.md](messages.md#the-two-timestamp-encodings).

## The visible head is not the last line

You cannot take "the last entry" as the conversation tip. The visible leaf
is a separate pointer moved by `leaf` rows, and `appendMode:"side"` rows
advance the raw cursor without changing the visible branch. Reconstruct the
conversation by tracking the leaf and walking `parentId` to the root, not by
file order. See
[jsonl-envelope.md](jsonl-envelope.md#the-tree-and-the-visible-leaf).

## Entry ids are file-scoped and only 8 chars

Entry `id`s are `uuidv7().slice(0,8)` — 8 hex chars, unique **within one
file** via collision retry, not globally. A consumer merging multiple
sessions must namespace ids by file; do not assume cross-file uniqueness.
The session id (the filename stem) is a full UUID — a different format from
entry ids.

## Usage undercounts across a compaction

Assistant `usage` at or under the latest compaction boundary is **zeroed**
on disk (`stripStaleAssistantUsageBeforeLatestCompaction`). Summing
transcript usage without accounting for compaction boundaries undercounts
the session total. See [usage.md](usage.md#compaction-zeroes-stale-usage).

## No reasoning token count on disk

The persisted `Usage` has input/output/cache/total only. Reasoning/thinking
**token counts** live solely in the runtime accumulator / stream events /
trajectory, never in the transcript — even though a `thinking` **content**
block is persisted. Don't try to populate a reasoning breakdown from the
transcript. See [usage.md](usage.md#reasoning-tokens-are-runtime-only).

## No stored diffs

File edits are tool calls with arguments; there is no unified diff or
before/after content in the transcript. A derived `Path` gets structural,
tool-input-derived changes only — no `raw` perspective. See
[tools.md](tools.md#file-operations-tool-input-only-no-raw-diff).

## Group-sender identity is text-only

In a group/channel session the key identifies the room, not the speaker.
The individual sender is injected as a `[from: Name (+E164)]` marker in the
prompt text; structured `senderId`/`senderName` are not persisted as
transcript fields. Per-message human identity in groups is best-effort
string parsing. See
[channels-and-actors.md](channels-and-actors.md#who-is-the-human).

## `version` is hard-rejected if not 3

The reader throws on `version !== 3` (`jsonl-storage.ts:80-82`). A
forward-compatible consumer should treat a different version as "re-read
this reference," not silently parse it as v3.

## The inner message body is unvalidated

`parseEntryLine` validates only the envelope; the `message` object and its
content blocks are cast through unchecked (`jsonl-storage.ts:108-147`). The
shapes in [messages.md](messages.md) are the **producer** contract. A robust
reader should tolerate missing/extra fields inside `message` rather than
assume the documented shape holds for every line.

## Blank lines between records

The reader filters empty lines before parsing (`jsonl-storage.ts:184`).
Don't assume strictly one record per physical line with no gaps.

## Legacy v1 logs get back-filled ids

Old logs predating entry ids are migrated with synthetic `parentId` chains
(`migrateLegacySessionEntries`, `src/trajectory/export.ts:946-989`). Ids in
such files may be derived, not original.

## Two code layers for one format

The transcript is produced/consumed by both the agent-core harness storage
(`packages/agent-core/src/harness/session/jsonl-storage.ts`, types in
`harness/types.ts`: `SessionTreeEntry`, `MessageEntry`, `LeafEntry`, …) and
the gateway session manager (`src/agents/sessions/session-manager.ts`,
writer `src/config/sessions/transcript-jsonl.ts`: `SessionEntry`,
`SessionMessageEntry`, `CompactionEntry`, `FileEntry`). These appear to be
two views of the **same** on-disk JSONL, but they use different type names.
This reference uses the agent-core names. **Unconfirmed:** that the two
serialize byte-identically in every case — verify against a real file (and
against whichever layer actually writes the user's transcript) when the
`toolpath-openclaw` crate is built.

## Daemon profile can relocate the whole store

A non-default `OPENCLAW_PROFILE` moves everything to `~/.openclaw-<profile>`
via the daemon's own path resolver, which ignores `OPENCLAW_HOME`. If
sessions seem to be "missing," check for a profile. See
[directory-layout.md](directory-layout.md#daemon-path-divergence).
