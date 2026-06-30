# OpenClaw on-disk format

> **Reference revision:** 2026-06-30
> **Tracks:** OpenClaw package `2026.6.10`, session format **version 3**
> **Sourced from:** upstream code at
> `openclaw/openclaw @ 68c533cfb339cbb8650832cb2a4bf38dba7022fa` — **no
> first-hand on-disk sample yet** (see [Sourcing](#sourcing-and-confidence)).
>
> When you change anything in this directory, bump the revision date here
> and add a note to [format-changelog.md](format-changelog.md).

[OpenClaw](https://github.com/openclaw/openclaw) is a local-first personal
AI assistant — "Your own personal AI assistant. Any OS. Any Platform. The
lobster way. 🦞" It runs agent sessions on your own device and bridges them
to messaging channels (WhatsApp, Telegram, Slack, Discord, Matrix, Signal,
…). It persists every session as an append-only JSONL transcript under
`~/.openclaw/`.

This is different from the other providers we derive from, which are all
single-user coding-agent CLIs. An OpenClaw session is scoped to an *agent
persona × channel × peer/thread*, and the human is a messaging contact, not
a local shell user — so identity, lineage, and "what is one session" all
work differently here. These docs are the working reference for what
OpenClaw writes to disk, for anyone building a tool that reads or writes it
(such as a future `toolpath-openclaw` provider).

## How the docs are organized

Read in this order if you're new; otherwise skip to what you need. If you
prefer a concrete example, start with the **walkthrough** (#10).

1. **[directory-layout.md](directory-layout.md)** — the `~/.openclaw/` tree,
   how the state directory is resolved (env overrides, no XDG), the
   per-agent `sessions/` directory, the `sessions.json` index, and the
   telemetry/SQLite stores that are *not* the transcript.
2. **[jsonl-envelope.md](jsonl-envelope.md)** — the version-3 header line,
   the entry base (`type`/`id`/`parentId`/`timestamp`), and the tree +
   visible-leaf mechanics that decide which branch is live.
3. **[entry-types.md](entry-types.md)** — the ten `type` variants in detail.
4. **[messages.md](messages.md)** — message roles, the `text`/`thinking`/
   `image`/`toolCall` content blocks, assistant metadata, and the two
   timestamp encodings.
5. **[tools.md](tools.md)** — `toolCall` ↔ `toolResult` correlation,
   `isError`, and why there's no stored diff (file ops are tool-input only).
6. **[usage.md](usage.md)** — persisted `Usage` vs runtime `NormalizedUsage`,
   per-step deltas, compaction-zeroing, and why the reasoning token count
   isn't on disk.
7. **[lineage.md](lineage.md)** — the intra-session entry DAG, cross-session
   forks/sub-agents, compaction branches, and the session-kind classifier.
8. **[channels-and-actors.md](channels-and-actors.md)** — the session-key
   grammar, channels, DM vs group identity, `[from:]` markers, and persona
   vs model as the agent actor. The OpenClaw-specific axis.
9. **[known-issues.md](known-issues.md)** — format-level gotchas to defend
   against.
10. **[walkthrough.md](walkthrough.md)** — a session read line by line, with
    cross-links back to the reference docs.
11. **[format-changelog.md](format-changelog.md)** — version-keyed record of
    field/behavior changes.

## Sourcing and confidence

Unlike the other references in this directory (claude-code, codex, cursor,
gemini, opencode, pi), this one is **not** backed by first-hand on-disk
samples or by our own parser:

- **No installed OpenClaw / no sample file.** Every claim is derived from
  **upstream source code** at the pinned commit, not from observed bytes.
- **No `toolpath-openclaw` crate yet.** We can't corroborate with our own
  tests. This is the same posture `opencode.md` was written in before its
  crate existed.

Consequences for how to read these docs:

- Field tables here mean "**from source**" (the producer's type), not
  "observed in the wild." Where a serialized form could not be confirmed
  from the code, the doc says so explicitly.
- The transcript is touched by **two code layers** (agent-core harness
  storage and the gateway session manager) with slightly different type
  names for the same JSON; we use the agent-core names and flag the
  reconciliation in
  [known-issues.md](known-issues.md#two-code-layers-for-one-format).

When the crate is built, re-verify against a real session, upgrade claims
from "from source" to "observed," and record it in the changelog.

## Conventions

- **Field names** are shown as they appear in JSON (camelCase envelope keys
  like `parentId`; provider-style keys like `provider`/`modelId` where the
  source uses them).
- **"From source"** = read from upstream TypeScript at the pinned commit.
  **"Observed"** is reserved for claims confirmed against a real file (none
  yet).
- **Citations** point to `openclaw/openclaw path:line` at commit
  `68c533cf`, or to a doc in this folder.
- **Keep headings anchor-stable.** Cross-links use GitHub auto-anchors
  (lowercased, punctuation stripped, spaces to hyphens). Avoid em-dashes in
  linkable headings.

## Field index

Quick lookup: which doc defines a given field?

| Field | Defined in |
|---|---|
| `agentId` | [directory-layout.md](directory-layout.md), [channels-and-actors.md](channels-and-actors.md) |
| `api` | [messages.md](messages.md) |
| `appendMode` / `appendParentId` | [jsonl-envelope.md](jsonl-envelope.md) |
| `arguments` (on `toolCall`) | [messages.md](messages.md), [tools.md](tools.md) |
| `branch_summary` / `fromId` | [entry-types.md](entry-types.md), [lineage.md](lineage.md) |
| `cacheRead` / `cacheWrite` | [usage.md](usage.md) |
| `channel` / `peerKind` / `peerId` | [channels-and-actors.md](channels-and-actors.md) |
| `compaction` / `firstKeptEntryId` / `tokensBefore` | [entry-types.md](entry-types.md), [usage.md](usage.md), [lineage.md](lineage.md) |
| `content` (message / block) | [messages.md](messages.md) |
| `cost` | [usage.md](usage.md) |
| `custom` / `custom_message` / `customType` | [entry-types.md](entry-types.md) |
| `cwd` | [jsonl-envelope.md](jsonl-envelope.md), [directory-layout.md](directory-layout.md) |
| `details` (tool result / summaries) | [tools.md](tools.md), [entry-types.md](entry-types.md) |
| `id` (entry) | [jsonl-envelope.md](jsonl-envelope.md) |
| `id` (session header) | [jsonl-envelope.md](jsonl-envelope.md) |
| `id` (on `toolCall`) | [messages.md](messages.md), [tools.md](tools.md) |
| `input` / `output` / `totalTokens` | [usage.md](usage.md) |
| `InputProvenance` / `kind` | [channels-and-actors.md](channels-and-actors.md) |
| `isError` | [tools.md](tools.md) |
| `label` (on `label` entry) | [entry-types.md](entry-types.md) |
| `leaf` / `targetId` | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md](entry-types.md) |
| `model` / `modelId` / `model_change` | [messages.md](messages.md), [entry-types.md](entry-types.md) |
| `parentId` | [jsonl-envelope.md](jsonl-envelope.md), [lineage.md](lineage.md) |
| `parentSession` (header) | [jsonl-envelope.md](jsonl-envelope.md), [lineage.md](lineage.md) |
| `parentSessionKey` / `spawnedBy` / `spawnDepth` / `subagentRole` | [lineage.md](lineage.md) |
| `provider` | [messages.md](messages.md), [channels-and-actors.md](channels-and-actors.md) |
| `redacted` (on `thinking`) | [messages.md](messages.md) |
| `role` | [messages.md](messages.md) |
| `session_info` / `name` | [entry-types.md](entry-types.md) |
| `stopReason` | [messages.md](messages.md) |
| `summary` (compaction / branch) | [entry-types.md](entry-types.md), [lineage.md](lineage.md) |
| `textSignature` / `thinkingSignature` / `thoughtSignature` | [messages.md](messages.md) |
| `thinking` (content block) | [messages.md](messages.md) |
| `thinking_level_change` / `thinkingLevel` | [entry-types.md](entry-types.md) |
| `timestamp` (entry, ISO) | [jsonl-envelope.md](jsonl-envelope.md) |
| `timestamp` (inner message, epoch ms) | [messages.md](messages.md) |
| `toolCall` / `toolCallId` / `toolName` | [messages.md](messages.md), [tools.md](tools.md) |
| `type` (header / entry) | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md](entry-types.md) |
| `usage` | [usage.md](usage.md) |
| `version` | [jsonl-envelope.md](jsonl-envelope.md), [format-changelog.md](format-changelog.md) |

## Maintenance

When a field, entry type, or behavior changes (or a real sample
contradicts something here), update the relevant doc in the same change,
keep this index and the doc map in sync, and add a
[format-changelog.md](format-changelog.md) entry. The point of this folder
is to be the single place OpenClaw format knowledge accumulates.
