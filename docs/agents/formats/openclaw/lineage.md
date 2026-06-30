# Lineage and session kinds

OpenClaw has lineage at **two scales** — within a session (the entry DAG)
and across sessions (forks, sub-agents, compaction branches) — plus a
session-kind classifier that separates user chats from cron/automation and
sub-agent runs. A toolpath derivation that wants the full provenance graph
must union all of these.

## 1. Intra-session entry DAG

Every entry carries `id` + `parentId`
([jsonl-envelope.md](jsonl-envelope.md#the-tree-and-the-visible-leaf)),
forming a tree/DAG inside one file. The leaf is found via `getLeafId()`;
branches are real (`getChildren`, `getTree`); side branches use
`appendMode:"side"`. Dead ends are entries not on the leaf's ancestry.

This maps to toolpath's Step DAG **verbatim**: `id` → step id, `parentId` →
parent reference, leaf ancestry → `path.head`, off-ancestry entries →
implicit dead ends.

> Legacy v1 logs that predate entry ids get synthetic `parentId` chains via
> `migrateLegacySessionEntries` (`src/trajectory/export.ts:946-989`). A
> reader ingesting old files should expect ids/parents to have been
> back-filled rather than original.

## 2. Cross-session lineage (forks and sub-agents)

Three independent primitives connect *sessions* to each other:

### `parentSession` on the header

The session header's optional `parentSession` is a **path to a parent
session file** ([jsonl-envelope.md](jsonl-envelope.md#the-header-line)). The
simplest cross-file fork edge.

### `AcpSessionLineageMeta`

`packages/acp-core/src/session-lineage-meta.ts:10-22`:

```ts
type AcpSessionLineageMeta = {
  sessionKey: string;
  kind?: string;
  channel?: string;
  parentSessionId?: string;     // = parentSessionKey ?? spawnedBy
  spawnedBy?: string;
  spawnDepth?: number;
  subagentRole?: "orchestrator" | "leaf";
  subagentControlScope?: "children" | "none";
  spawnedWorkspaceDir?: string;
  spawnedCwd?: string;
};
```

`parentSessionKey` (new) / `spawnedBy` (legacy) is the parent-session
pointer; `parentSessionId` normalizes the two. `spawnDepth` and
`subagentRole` describe the sub-agent tree. This is persisted with the
session record (`SessionsPatchParamsSchema`,
`gateway-protocol/src/schema/sessions.ts:300-346`).

### Sub-agent completion events

Sub-agent results flow back via `AgentInternalEventSchema`
(`gateway-protocol/src/schema/agent.ts:741-758`):
`{ type:"task_completion", source:"subagent"|"cron"|"image_generation"|…,
childSessionKey, childSessionId, status }`. Stream events also carry
`spawnedBy`.

> **Keys vs ids.** `spawnedBy` / `parentSessionKey` are session **keys**,
> while `childSessionId` / `originSessionId` are session **ids**. To stitch a
> cross-session DAG you need the key↔id resolver
> (`src/sessions/session-id-resolution.ts`). Sub-agent keys are also
> detectable from key shape (`isSubagentSessionKey`,
> `src/sessions/session-key-utils.ts`).

**toolpath mapping:** a session-of-sessions DAG. Model the union as a
`Graph` of `Path`s, or as cross-`Path` parent edges, using
`parentSessionId`/`spawnedBy` for the edges and `subagentRole` to tell
orchestrators from leaves.

## 3. Compaction as a new branch

A `compaction` entry ([entry-types.md §compaction](entry-types.md#compaction))
truncates history in place. But OpenClaw can also turn a compaction into a
**new session**: `sessions.compaction.branch`
(`SessionsCompactionBranchResultSchema`,
`gateway-protocol/src/schema/sessions.ts:462-478`) creates a **new session
key + new sessionId** from a checkpoint, with `sourceKey` linking back to
the predecessor. Checkpoints (`SessionCompactionCheckpointSchema`,
`sessions.ts:50-65`) record `tokensBefore`, `tokensAfter`,
`firstKeptEntryId`, and `preCompaction`/`postCompaction` transcript
references (`{ sessionId, sessionFile, leafId, entryId }`).

**toolpath mapping:** a compaction branch is a new `Path` whose root has a
parent reference into the predecessor (`sourceKey` +
`preCompaction.entryId`).

## Session kinds

`classifySessionKind(key, entry)` (`src/sessions/classify-session-kind.ts`):

```ts
type SessionKind = "cron" | "direct" | "group" | "global" | "spawn-child" | "unknown";
```

Priority (most specific first):

1. Sentinel keys `"global"` / `"unknown"`.
2. **`cron`** — `isCronSessionKey(key)` (key rest begins `cron:`;
   `cron:<name>:run:<id>` for a specific run).
3. **`spawn-child`** — `entry.spawnedBy` is set (checked *before* key shape
   so ACP spawn-children with opaque keys aren't mislabeled `direct`).
4. **`group`** — `entry.chatType === "group"|"channel"`, or the key contains
   `:group:` / `:channel:`.
5. Fallback **`direct`**.

Complementary: `deriveSessionChatType` (`src/sessions/session-chat-type.ts`)
→ `direct|group|channel|…` from the key. Automation run-kind is also
signaled on the request (`AgentParamsSchema.bootstrapContextRunKind:
"default"|"heartbeat"|"cron"`, `acpTurnSource: "manual_spawn"`).

**toolpath mapping:** a clean discriminator for a `Path` `kind`/`meta` tag —
`cron`/`spawn-child` are automation/sub-agent, `direct`/`group` are user
sessions, `global` is the agent-wide session. `spawn-child` detection
depends on `entry.spawnedBy` being populated; `subagentRole` refines it
further.
