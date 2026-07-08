# Format changelog

A version-keyed record of OpenClaw session-format fields and behaviors, so
downstream readers can tell whether a cited rule is current. Entries are
newest-first.

The format self-identifies via the header `version` field
([jsonl-envelope.md](jsonl-envelope.md#the-header-line)); the reader
hard-rejects anything other than `3`. The OpenClaw package version (CalVer,
e.g. `2026.6.10`) is separate from this on-disk format version.

## 2026-07-08: first-hand observation pass (image v2026.6.11)

Real sessions were captured from the official `openclaw/openclaw` Docker
image via `scripts/openclaw-docker.sh` and are committed as fixtures at
`test-fixtures/openclaw/`. Confirmed against real bytes: the v3 header,
entry types `session`/`message`/`model_change`/`thinking_level_change`/
`custom`/`custom_message`, roles `user`/`assistant`/`toolResult`, content
blocks, `stopReason`, and the full `Usage` shape (including prompt-cache
`cacheWrite`). Newly observed facts:

- **Native tool names:** `exec`, `read`, `write`, `edit`,
  `sessions_spawn` (sub-agent dispatch), `sessions_yield`.
- **Sub-agent routing keys:** `agent:<id>:subagent:<uuid>` appears in
  `sessions.json` after a `sessions_spawn`, with the child transcript as a
  sibling file.
- **DM scope default:** `agent --to <E.164>` routes into
  `agent:<id>:main` (the default DM scope collapses DMs into the main
  session); a channel-scoped key requires an explicit `--session-key`.
- **`sessions.json` entries carry ~19 fields** (`updatedAt`,
  `sessionStartedAt`, token counters, `systemPromptReport`, …) and
  `sessionFile` is stored **absolute**.
- **Transcript file mode is `0644`** (docs previously said `0600`; see
  known-issues).
- **Inception adoption:** a running gateway adopts a transcript +
  `sessions.json` entry written by an external tool without restart, and
  appends follow-up turns to the same file.
- `leaf` rows are **not** written on every append — a linear real session
  contained none (the visible head falls back to the last entry).

## Format version 3

**Status as of this reference:** the only format version this doc set
covers. Established by reading upstream source at
`openclaw/openclaw @ 68c533cfb339cbb8650832cb2a4bf38dba7022fa` (package
`2026.6.10`); **not** yet confirmed against a first-hand on-disk sample.
Implemented by the `toolpath-openclaw` crate (forward + projector), whose
tests exercise the shapes below against synthesized fixtures.

Salient v3 facts (all detailed elsewhere in this folder):

- Header line `{ type:"session", version:3, id, timestamp, cwd,
  parentSession? }`.
- Ten entry types: `message`, `model_change`, `thinking_level_change`,
  `compaction`, `branch_summary`, `custom`, `custom_message`, `label`,
  `session_info`, `leaf`.
- Tree/DAG via `id`/`parentId` with a separate visible-leaf pointer
  (`leaf` rows, `appendMode:"side"`).
- Message roles `user`/`assistant`/`toolResult`/`bashExecution`; content
  blocks `text`/`thinking`/`image`/`toolCall`; tool results are separate
  entries linked by `toolCallId`.
- Per-message `Usage` (input/output/cacheRead/cacheWrite/totalTokens/cost),
  per-step delta, no reasoning token field; zeroed across compaction.
- Dual timestamp encodings (entry ISO string vs inner message epoch-ms).

### Pre-v3 / legacy

Logs predating entry ids exist and are migrated with synthetic `parentId`
chains by `migrateLegacySessionEntries` (`src/trajectory/export.ts:946-989`).
The legacy state directory name was `~/.clawdbot` (still tolerated; config
`clawdbot.json`). We have not characterized the pre-v3 on-disk shape — treat
migrated files as v3-shaped with back-filled lineage.

## Maintenance

When the `toolpath-openclaw` crate lands, or when a new OpenClaw release
changes a field or bumps the header `version`:

1. Add a new section at the top here with the version and what changed.
2. Update the affected reference doc(s) in the same change.
3. Upgrade claims from "from source" to "observed" once a real sample
   confirms them, and bump the revision date in
   [README.md](README.md).
