# Design: OpenClaw on-disk format reference docs

> **Status:** approved outline, pre-authoring
> **Date:** 2026-06-30
> **Author:** Alex (with Claude)
> **Scope:** Write `docs/agents/formats/openclaw/` — a claude-code-style
> folder of focused reference docs for OpenClaw's on-disk session format.
> This is step one of the larger "add OpenClaw as a provider" effort; the
> `toolpath-openclaw` crate itself is **out of scope** for this spec.

## Why

We want to add OpenClaw (`github.com/openclaw/openclaw`) as a provider we
derive `toolpath` documents from, the way we already do for Claude Code,
Codex, Gemini, opencode, Cursor, and Pi. Every provider starts from an
empirical on-disk-format reference under `docs/agents/formats/`. This spec
defines that reference for OpenClaw so the eventual crate can be built
without re-reverse-engineering the format.

## What OpenClaw is

A local-first personal AI assistant — "Your own personal AI assistant. Any
OS. Any Platform. The lobster way. 🦞" It runs agent sessions locally and
bridges them to messaging channels (WhatsApp, Telegram, Slack, Discord,
Matrix, Signal, …). TypeScript/Node pnpm monorepo. This makes it different
from the other providers, which are all single-user coding-agent CLIs: an
OpenClaw "session" is scoped to an *agent persona × channel × peer/thread*,
and the human on the other end is a messaging contact, not a local shell
user.

## Sourcing and provenance (read this — it differs from the other docs)

- **Pinned to** `openclaw/openclaw @ 68c533cfb339cbb8650832cb2a4bf38dba7022fa`
  (branch `main`), package version `2026.6.10` (CalVer), license
  NOASSERTION (custom).
- **No first-hand on-disk sample.** OpenClaw is not installed on the
  authoring machine, so unlike claude-code/codex/cursor we have **no real
  session file** to inspect. Every claim is derived from **upstream source
  code**, not observed bytes.
- **No `toolpath-openclaw` parser yet.** Unlike the other refs, we can't
  cite our own tests/parser as corroboration.
- Therefore these docs are in the same posture `opencode.md` was written in:
  "intended input for a future provider," compiled from upstream types.
  Every doc's header must say so, and any field whose **serialized** form we
  could not confirm from a real file must be flagged ("from the producer
  type; not confirmed against a sample").
- When the crate lands, revisit and upgrade claims from "from source" to
  "observed" with a real sample, and add a `format-changelog.md` entry.

### Reconciliation item (must be resolved while authoring)

The session-transcript format is produced/consumed across **two code
layers** with slightly different type names. The docs must present one
coherent format and note the divergence:

- **agent-core harness storage** — `packages/agent-core/src/harness/session/jsonl-storage.ts`
  (+ `storage-base.ts`, `session.ts`), types in `packages/agent-core/src/harness/types.ts`:
  `SessionHeader`, `SessionTreeEntry`, `MessageEntry`, `LeafEntry`, etc.
- **gateway session manager** — `src/agents/sessions/session-manager.ts`
  (+ writer `src/config/sessions/transcript-jsonl.ts`): `SessionHeader`,
  `SessionEntry`, `SessionMessageEntry`, `CompactionEntry`, `FileEntry`.

Working hypothesis: these are two views of the **same on-disk JSONL** (the
gateway wraps/mirrors agent-core's storage). Authoring task: confirm they
serialize identically (same `type` strings, same field names) and, if they
diverge anywhere, document which one writes the file users actually have.
Cite agent-core types as the canonical shape unless proven otherwise.

## Format synthesis (the durable research)

All file:line citations below are at the pinned commit.

### 1. Storage roots and layout

- Root data dir precedence: `OPENCLAW_STATE_DIR` → existing `~/.openclaw`
  → existing legacy `~/.clawdbot` → default `~/.openclaw`. The `~` itself is
  resolved via `OPENCLAW_HOME` → `HOME`/`USERPROFILE`/Termux → `os.homedir()`.
  No XDG, no per-OS special dir; identical layout on macOS/Linux/Windows.
  (`src/config/paths.ts:209-273`, `src/infra/home-dir.ts:35-54`.)
- Config file: `OPENCLAW_CONFIG_PATH` → `<stateDir>/openclaw.json`
  (legacy `clawdbot.json` tolerated). OAuth: `OPENCLAW_OAUTH_DIR` →
  `<stateDir>/credentials/oauth.json`.
- Canonical transcript path:
  `~/.openclaw/agents/<agentId>/sessions/<sessionId>.jsonl`, default
  `agentId = "main"` (`src/config/sessions/paths.ts:653-661, 898-902`;
  `src/routing/session-key.ts` `DEFAULT_AGENT_ID`).
- Filename variants in the same dir: `<sessionId>.jsonl`,
  `<sessionId>-topic-<topicId>.jsonl`, forked/rotated
  `<ISO-ts>_<sessionId>.jsonl`. `<sessionId>` is a UUID; entry ids are
  8-char UUIDv7 prefixes.
- Index sidecar: `sessions.json` = `Record<sessionKey, {sessionId,
  sessionFile, updatedAt, sessionStartedAt, …}>`
  (`src/config/sessions/paths.ts:678-680`). Maps routing keys → files.
- **Not canonical:** `<sessionId>.trajectory.jsonl` +
  `<sessionId>.trajectory-path.json` (runtime telemetry sidecars);
  `~/.openclaw/state/openclaw.sqlite` and
  `~/.openclaw/agents/<id>/agent/openclaw-agent.sqlite` (routing/auth/cron/
  queue/cache/embeddings — **no transcript table**; the one message-shaped
  table, `acp_replay_events`, is a transient ACP replay buffer).
- Permissions: transcript/JSON files `0600`; SQLite dirs `0700`, files
  `0600`. Daemon path divergence: `src/daemon/paths.ts:116-127` resolves its
  own `~/.openclaw[-<profile>]` from `HOME`/`USERPROFILE` only (ignores
  `OPENCLAW_HOME`); non-default `OPENCLAW_PROFILE` ⇒ `~/.openclaw-<profile>`.

### 2. JSONL envelope and entry types

- Line 1 = header `SessionHeader` (`jsonl-storage.ts:22-29`):
  `{type:"session", version:3, id, timestamp (ISO), cwd, parentSession?}`.
  Reader hard-rejects `version !== 3` (`jsonl-storage.ts:80-82`).
  `parentSession` = path to a parent session file (cross-file chaining).
- Entry base `SessionTreeEntryBase` (`harness/types.ts:353-364`):
  `{type:string, id, parentId:string|null, timestamp:ISO, appendMode?:"side"}`.
- 10 entry `type` values (`harness/types.ts:443-454`): `message`,
  `thinking_level_change`, `model_change`, `compaction`, `branch_summary`,
  `custom`, `custom_message`, `label`, `session_info`, `leaf`. Reader
  tolerates unknown future types (`storage-base.ts:46-69`).
- **Tree, not a flat log.** `parentId` forms a DAG (`null` = root); the
  active conversation is `leafId → root` (`storage-base.ts getPathToRoot`).
  The visible head is a *separate* pointer maintained by `LeafEntry`
  (`{type:"leaf", targetId, appendParentId?}`, `harness/types.ts:435-440`);
  `appendMode:"side"` advances the raw cursor without selecting a visible
  branch. Dead ends = entries not on the leaf's ancestry — maps directly to
  toolpath's implicit dead-end model.
- Blank lines between records are tolerated/skipped
  (`jsonl-storage.ts:184`). Inner `message` body is **not** schema-validated
  on read — only the envelope is (`jsonl-storage.ts:108-147`). Entry ids are
  file-scoped and only 8 hex chars.

### 3. Messages and content blocks

- `MessageEntry.message: AgentMessage`. On-disk roles realistically:
  `user`, `assistant`, `toolResult`, `bashExecution`. (`custom`,
  `branchSummary`, `compactionSummary` roles are *reconstructed at read*
  from their own entry types, not stored as `message` entries —
  `session.ts:46-99`, `messages.ts`.)
- Content blocks (`packages/llm-core/src/types.ts`): `text`
  (`text`, `textSignature?`), `thinking` (`thinking`, `thinkingSignature?`,
  `redacted?`), `image` (`data` base64, `mimeType`), `toolCall`
  (`id`, `name`, `arguments`, `thoughtSignature?`, `executionMode?`).
- `UserMessage.content`: string OR `(text|image)[]`. `AssistantMessage`:
  always a block array, plus `api`, `provider`, `model`, `responseModel?`,
  `responseId?`, `diagnostics?`, `usage`, `stopReason`, error fields,
  `timestamp` (epoch ms).
- **Two timestamp encodings (footgun):** entry-level `timestamp` is an ISO
  string; inner `message.timestamp` is epoch-ms number.
- `stopReason` ∈ `stop|length|toolUse|error|aborted`.

### 4. Tools and file operations

- `toolCall` (assistant content block) ↔ `toolResult` (separate `message`
  entry) linked by `toolResultMessage.toolCallId == toolCall.id`.
  `ToolResultMessage` (`llm-core/src/types.ts:306-314`): `{toolCallId,
  toolName, content:(text|image)[], details?:unknown, isError, timestamp}`.
  Errors = `isError:true` with text in `content`; no separate error field.
- **No stored diffs.** File edits are just tool calls; recover a
  *touched-files list* from tool-call `arguments`, not a patch. The
  server-side `sessions.files.list` (`SessionFileEntrySchema`,
  `gateway-protocol/.../sessions.ts:78-89`) classifies paths as
  `modified|read` but has no hunks. → derived `Path` should carry
  **structural / tool-input-derived** changes only, **no `raw` perspective**
  (parallels opencode's gitignored fallback).

### 5. Token usage (two shapes)

- **Persisted (source of truth):** `Usage` on each `AssistantMessage`
  (`llm-core/src/types.ts:261-275`): `{input, output, cacheRead, cacheWrite,
  totalTokens, cost{…}}`. **Per-step delta**, not cumulative (accumulator
  uses `+=`). **No reasoning field.** Compaction zeroes stale assistant
  usage (`compaction-usage.ts stripStaleAssistantUsageBeforeLatestCompaction`)
  — summing naively after a compaction undercounts.
- **Runtime only:** `NormalizedUsage` (`src/agents/usage.ts:52-60`) adds
  `reasoningTokens`; surfaced on stream events and trajectory artifacts but
  **not** in the persisted transcript. So a transcript reader gets
  total/input/output/cache but **cannot** populate a reasoning
  `breakdowns["output"]["reasoning"]` sub-class.
- Convention flags: `Usage.total` = `totalTokens`; OpenClaw's
  `prompt_tokens` = input + cacheRead (cacheWrite excluded). A separate
  `SessionEntry.totalTokens` / `deriveSessionTotalTokens` is a
  prompt/context snapshot that **excludes output** — do not confuse with a
  turn total.

### 6. Lineage and session kinds

- Three lineage mechanisms (union them):
  - (a) **intra-session entry DAG** — `id`/`parentId`. → toolpath Step DAG
    verbatim.
  - (b) **cross-session fork / sub-agents** — `AcpSessionLineageMeta`
    (`packages/acp-core/src/session-lineage-meta.ts:10-22`):
    `parentSessionKey`/`spawnedBy`, `spawnDepth`, `subagentRole`,
    `subagentControlScope`; `SessionHeader.parentSession`; sub-agent
    completion `AgentInternalEventSchema` `task_completion` with
    `childSessionKey`/`childSessionId`. → Graph-of-Paths / cross-Path edge.
  - (c) **compaction → new branch** — `CompactionEntry` (`summary`,
    `firstKeptEntryId`, `tokensBefore`, `fromHook`); `sessions.compaction.branch`
    creates a new key+id with `sourceKey` back-link. → new Path with parent ref.
  - Caveat: `spawnedBy`/`parentSessionKey` are session **keys**;
    `childSessionId`/`originSessionId` are session **ids** — needs the
    key↔id resolver (`session-id-resolution.ts`).
- `BranchSummaryEntry` (`fromId`, `summary`, `details:{readFiles,
  modifiedFiles}`, `fromHook`) marks an abandoned branch.
- Session kinds (`classify-session-kind.ts`): `cron | direct | group |
  global | spawn-child | unknown`. → Path `kind`/`meta` tag.

### 7. Channels and actors (the OpenClaw-specific axis)

- **No `sender` field on transcript messages.** Human identity is
  structural in the session **key** (`buildAgentPeerSessionKey`,
  `src/routing/session-key.ts:222-273`):
  `agent:<agentId>:<channel>:<peerKind>:<peerId>` (+ DM-scope and
  `:thread:<id>` variants). `channel` ∈ whatsapp/telegram/slack/discord/
  matrix/signal/…; `peerKind` ∈ direct/dm/group/channel.
- DM: `peerId` *is* the human's channel user id. Group/channel: `peerId` is
  the room id; **individual sender is text-only** — `[from: Name (+E164)]`
  markers injected into prompt text (`docs/channels/group-messages.md`).
  Structured `senderId`/`senderName`/`pushName` exist at the inbound layer
  but are flattened into text, not persisted as transcript fields.
- `InputProvenance` (`src/sessions/input-provenance.ts:14-21`):
  `{kind:"external_user"|"inter_session"|"internal_system",
  originSessionId?, sourceSessionKey?, sourceChannel?, sourceTool?}` — the
  closest structured who/where on a user message.
- Agent identity: per-`AssistantMessage` `provider`/`model`/`api`;
  `model_change` entries; agent persona `agentId`/name/avatar/emoji.
- Closest `type:name` actor strings: human `<channel>:<peerId>` (DM) or
  `group:<groupId>` (+`[from:]` parse); agent `agent:<agentId>` or
  `<provider>:<model>`. **Decision for the crate (not this doc):** which
  axis is `agent:name` — the persona or the model.

## Doc folder structure (12 files)

Mirror the claude-code house style: each doc opens with a dated provenance
header; field tables show the JSON name / shape / optional|required;
"Observed" vs (here, mostly) "From source"; keep headings anchor-stable
(no em-dashes in linkable headings).

1. `README.md` — overview, sourcing/provenance header, doc map (ordered
   reading list), conventions, field index table, maintenance note.
2. `directory-layout.md` — §1 above.
3. `jsonl-envelope.md` — header + entry base + tree/`leaf`/`appendMode`
   mechanics + blank-line tolerance + no-inner-validation.
4. `entry-types.md` — the 10 `type` variants in detail, each with a field
   table.
5. `messages.md` — roles + content blocks; the dual-timestamp note;
   `stopReason` values.
6. `tools.md` — `toolCall`↔`toolResult` correlation; `isError`; file-ops
   story (tool-input only, no raw diff, `sessions.files.list`).
7. `usage.md` — persisted `Usage` vs runtime `NormalizedUsage`; per-step
   deltas; reasoning-only-at-runtime; compaction zeroing; `totalTokens`
   conventions + `deriveSessionTotalTokens` gotcha.
8. `lineage.md` — three lineage mechanisms; `BranchSummaryEntry`; session
   kinds; key↔id resolution.
9. `channels-and-actors.md` — §7 above: session-key grammar, channel list,
   DM vs group identity, `[from:]` markers, `InputProvenance`, persona vs
   model.
10. `known-issues.md` — dual timestamps; file-scoped 8-char ids; usage
    undercount after compaction; no raw diff; group-sender text-only;
    `version==3` hard-reject; legacy v1 migration
    (`migrateLegacySessionEntries`); the two-code-layer reconciliation.
11. `walkthrough.md` — a linear, annotated example session (header → user
    message → model_change → assistant turn with thinking/text/toolCall →
    toolResult → leaf → compaction), cross-linking each line back to the
    reference docs. Example is **reconstructed from the types** and clearly
    labeled as illustrative, not a captured fixture.
12. `format-changelog.md` — seeded with "version 3 (observed in source at
    `2026.6.10`)"; a place to record future field/behavior drift.

Also update `docs/agents/formats/README.md` to add an `openclaw/` entry in
the Contents list (after `gemini.md`/`opencode.md`, keeping the existing
ordering style).

## Conventions to follow

- Field tables: name as it appears in JSON (camelCase envelope keys like
  `parentId`; `snake_case`/provider-style where the source uses it),
  shape, optional|required (here: "from source" rather than "observed").
- Mark every unconfirmed serialized field explicitly.
- Cite upstream as `openclaw/openclaw path:line @ 68c533cf`; cite our future
  crate only once it exists.
- Keep linkable headings free of em-dashes and decorative punctuation.

## Authoring checklist (ordered)

1. `directory-layout.md`, `jsonl-envelope.md`, `entry-types.md` (the
   structural core).
2. `messages.md`, `tools.md`, `usage.md` (record content).
3. `lineage.md`, `channels-and-actors.md` (the OpenClaw-specific axes).
4. `known-issues.md`, `walkthrough.md`, `format-changelog.md`.
5. `README.md` last (its field index and doc map reference the others).
6. Update `docs/agents/formats/README.md` Contents.
7. While authoring, resolve the two-code-layer reconciliation item by
   re-reading `jsonl-storage.ts` + `session-manager.ts` + `transcript-jsonl.ts`
   and confirming identical serialization.

## Out of scope (future, separate spec)

- The `toolpath-openclaw` crate (reader, provider, `derive_path` mapping,
  projector, CLI wiring under `path p import/export openclaw`,
  `share`/`resume` integration, tests).
- All `CLAUDE.md` / `Cargo.toml` / `site/_data/crates.json` / release-script
  updates that come with a new crate.
- Decisions the docs only *surface* (persona vs model as `agent:name`;
  how to represent multi-channel human actors in a `type:name` string).
