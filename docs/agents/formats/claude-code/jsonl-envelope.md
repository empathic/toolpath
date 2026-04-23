# Session JSONL: the line envelope

A session JSONL is a file where each line is a standalone JSON object.
No framing, no leading sentinel, no trailer. Lines are terminated with
`\n`; blank lines occur but should be ignored.

This document covers the **envelope** — the top-level fields that wrap
each line. The `message` sub-object and tool-invocation details have
their own docs ([messages.md](messages.md), [tools.md](tools.md)).

## Field conventions

- **Envelope keys are camelCase** (`parentUuid`, `sessionId`,
  `isSidechain`).
- **Keys inside `message` that map to the Anthropic API are snake_case**
  (`stop_reason`, `input_tokens`). Some older samples use camelCase
  aliases for API-side keys; a tolerant parser accepts both.
- **Field presence depends on entry type.** There is no single union
  that applies to every line; `type` is the discriminant. See
  [entry-types.md](entry-types.md).

## Complete field catalogue

Every envelope field we have observed, in rough order of prominence:

### Identity / position

| Field          | Shape                      | Notes |
|----------------|----------------------------|-------|
| `type`         | string                     | Discriminant. Values in [entry-types.md](entry-types.md). Present on **every** line. |
| `uuid`         | UUIDv4 string              | Per-entry ID. Empty string (`""`) or absent on some metadata entries (`permission-mode`, `queue-operation`, `last-prompt`, `file-history-snapshot`). |
| `timestamp`    | ISO-8601 string            | e.g. `"2026-04-02T13:59:26.313Z"`. Millisecond precision. Absent on pure-metadata entries. |
| `sessionId`    | UUIDv4 string              | Usually equals the filename stem. On continuation files, the first real entry carries the **previous** session's ID — see [session-chains.md](session-chains.md). |
| `parentUuid`   | UUIDv4 string \| `null`    | Prior entry in the conversation DAG. `null` for the first entry of a session and for `compact_boundary` entries (which use `logicalParentUuid` instead). |

### Context at time of entry

| Field          | Shape             | Notes |
|----------------|-------------------|-------|
| `cwd`          | absolute path     | Working directory when the entry was written. Will match the pre-sanitization path used in `projects/`. |
| `gitBranch`    | string            | Current branch. **Empty string** (not `null`, not absent) when the cwd isn't a git repo. |
| `version`      | string            | Claude Code client version, e.g. `"2.1.90"`. Observed values include `2.1.37`, `2.1.90`, `2.1.110`, `2.1.112`. Absent on some metadata entries. |
| `userType`     | string            | Almost always `"external"`. Other values (e.g. `"ant"` for Anthropic-internal) have been reported but are not common. |
| `entrypoint`   | string            | How the session was launched: `"cli"` is typical. Other values (e.g. `"claude-desktop"`) exist. |

### API correlation

| Field        | Shape        | Appears on | Notes |
|--------------|--------------|------------|-------|
| `requestId`  | string       | `assistant` | Anthropic API request ID, e.g. `"req_011CZf4hD7fj…"`. Useful for deduping streamed messages. |
| `messageId`  | UUID string  | `file-history-snapshot` (and as a reference from other entries) | The Anthropic API message ID, *distinct* from the entry's own `uuid`. Known collision bug on resume (see [known-issues.md](known-issues.md)). |

### Conversation threading

| Field             | Shape       | Notes |
|-------------------|-------------|-------|
| `isSidechain`     | bool        | `true` for entries belonging to a Task-tool-spawned subagent. See [entry-types.md §Sidechains](entry-types.md#sidechains). |
| `agentId`         | short hex string | Identifies a subagent thread. Present on sidechain entries. Format is a short hash (e.g. `"a7bf2fd"`). |
| `slug`            | string      | Human-readable conversation slug (e.g. `"crystalline-giggling-sunset"`). **Persists across rotations**, making it one of the more reliable signals for linking continuation files. |

### Tool mechanics

| Field                      | Shape   | Appears on | Notes |
|----------------------------|---------|------------|-------|
| `toolUseResult`            | object  | `user` entries carrying `tool_result` blocks | Top-level structured summary of the tool's output. Sibling of `message`, not nested. See [tools.md](tools.md). |
| `sourceToolAssistantUUID`  | UUIDv4  | `user` entries carrying `tool_result` blocks | `uuid` of the assistant entry that issued the matching `tool_use`. |

### Entry-type-specific fields

| Field                | Shape   | Appears on | Notes |
|----------------------|---------|------------|-------|
| `message`            | object  | `user`, `assistant`  | Conversation payload. See [messages.md](messages.md). |
| `snapshot`           | object  | `file-history-snapshot` | `{messageId, trackedFileBackups, timestamp}`. |
| `isSnapshotUpdate`   | bool    | `file-history-snapshot` | Whether this snapshot updates a prior one. |
| `permissionMode`     | string  | `permission-mode` | `"default"`, `"acceptEdits"`, `"plan"`, `"bypassPermissions"`. |
| `attachment`         | object  | `attachment` | Delta to available tools, e.g. `{type: "deferred_tools_delta", addedNames: [...], addedLines: [...], removedNames: [...]}`. |
| `operation`          | string  | `queue-operation` | `"enqueue"` / `"dequeue"`. |
| `content`            | string  | `queue-operation` | Queued message text. **Conflicts in name with `message.content`** — distinguish by entry `type`. |
| `lastPrompt`         | string  | `last-prompt` | Cached last user prompt. |
| `subtype`            | string  | `system`, `compact_boundary` | Discriminant within metadata entries. See [entry-types.md](entry-types.md). |
| `durationMs`         | number  | `system` (turn_duration) | Milliseconds the assistant turn took. |
| `messageCount`       | number  | `system` (turn_duration) | Number of messages in the turn. |
| `isMeta`             | bool    | various     | API-visibility flag. `true` marks entries the loader should hide from the transcript sent back to the API. |
| `isCompactSummary`   | bool    | synthetic user message after `compact_boundary` | Always paired with `isVisibleInTranscriptOnly: true`. |
| `isVisibleInTranscriptOnly` | bool | see above | Entry is visible in the UI but not replayed to the model. |
| `logicalParentUuid`  | UUID    | `compact_boundary` | Points at the pre-compact last message. `parentUuid` is `null` on these. |
| `compactMetadata`    | object  | `compact_boundary` | `{trigger: "auto"|"manual", preTokens: number}`. |
| `thinkingMetadata`   | object  | some user entries | `{level, disabled, triggers[]}`. Indicates extended-thinking configuration. |

### Hook-injected fields

When a hook contributes to an entry (e.g. `Stop` hook writing a summary
`system` entry), additional fields appear:

| Field                    | Shape           | Notes |
|--------------------------|-----------------|-------|
| `hookCount`              | number          | How many hooks ran for this event. |
| `hookInfos`              | array           | Per-hook info objects. |
| `hookErrors`             | array           | Per-hook error objects. |
| `preventedContinuation`  | bool            | Whether hook output blocked Claude from continuing. |
| `stopReason`             | string          | Hook-provided reason (distinct from `message.stop_reason`). |
| `level`                  | string          | Severity, e.g. `"suggestion"`, `"info"`. |

## Unknown fields

New fields appear across versions. A parser should flatten unknown keys
into an `extra` map rather than reject the line.

## Parser surface vs. format surface

This doc catalogues fields at the *format* level — every key we have
observed on an envelope. Our parser doesn't type all of them equally.
`toolpath-claude`'s `ConversationEntry`
([`crates/toolpath-claude/src/types.rs`](../../../../crates/toolpath-claude/src/types.rs))
promotes the fields a typical consumer needs into named Rust fields and
lets everything else land in a `#[serde(flatten)] extra: HashMap<String,
Value>`.

| Field name (JSON)          | Where it lives in `ConversationEntry` |
|----------------------------|---------------------------------------|
| `type`                     | `entry_type: String`                  |
| `uuid`                     | `uuid: String`                        |
| `timestamp`                | `timestamp: String`                   |
| `sessionId`                | `session_id: Option<String>`          |
| `parentUuid`               | `parent_uuid: Option<String>`         |
| `cwd`                      | `cwd: Option<String>`                 |
| `gitBranch`                | `git_branch: Option<String>`          |
| `version`                  | `version: Option<String>`             |
| `userType`                 | `user_type: Option<String>`           |
| `isSidechain`              | `is_sidechain: bool` (default false)  |
| `message`                  | `message: Option<Message>`            |
| `requestId`                | `request_id: Option<String>`          |
| `toolUseResult`            | `tool_use_result: Option<Value>`      |
| `snapshot`                 | `snapshot: Option<Value>`             |
| `messageId`                | `message_id: Option<String>`          |
| *everything else*          | `extra: HashMap<String, Value>`       |

Concretely, fields like `slug`, `agentId`, `entrypoint`,
`sourceToolAssistantUUID`, `permissionMode`, `attachment`, `operation`,
`content` (on `queue-operation`), `lastPrompt`, `subtype`, `durationMs`,
`messageCount`, `isMeta`, `isCompactSummary`,
`isVisibleInTranscriptOnly`, `logicalParentUuid`, `compactMetadata`,
`thinkingMetadata`, and the hook-injected envelope fields
(`hookCount`, `hookInfos`, `hookErrors`, `preventedContinuation`,
`stopReason`, `level`) are all documented here but accessed via
`entry.extra["<jsonKey>"]` in code. This is deliberate — it keeps the
typed surface small while round-tripping arbitrary format drift.

If you're building a new consumer in another language, the same split
tends to work: a core struct for the hot fields, a map for the rest.
