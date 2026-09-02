# Entry types

Every session-JSONL line has a `type` field that identifies what kind of
entry it is. A real parser must handle all of the following; a parser
that assumes only `user` / `assistant` / `system` will silently drop
a large fraction of the file.

## Summary table

| `type`                  | Has `message`? | Has `uuid`? | Purpose |
|-------------------------|----------------|-------------|---------|
| `user`                  | yes            | yes         | User prompt, slash command, or a synthesized user entry carrying tool results. |
| `assistant`             | yes            | yes         | Claude's response. Content is almost always an array of parts. |
| `system`                | varies         | yes         | Metadata entries. Discriminated further by `subtype`. |
| `attachment`            | no             | yes         | Tool-availability delta (e.g. a deferred tool being loaded). |
| `file-history-snapshot` | no             | no          | File-state snapshot for undo/rollback. |
| `permission-mode`       | no             | no          | Records the active permission mode. |
| `queue-operation`       | no             | no          | Enqueue/dequeue of a typed-ahead message while an assistant turn is in flight. |
| `last-prompt`           | no             | no          | Cached last user prompt. |
| `ai-title`              | no             | no          | Generated session title. |
| `custom-title`          | no             | no          | Explicitly set session title. |
| `agent-name`            | no             | no          | Agent name for the session. |
| `mode`                  | no             | no          | Active mode. Only `"normal"` observed. |
| `atis-latch`            | no             | no          | Opaque latch value. Purpose unknown. |
| `pr-link`               | no             | no          | Pull request linked to the session. |
| `frame-link`            | no             | no          | Frame URL linked to a local file. |
| `relocated`             | no             | no          | New working directory for the session. |
| `worktree-state`        | no             | no          | Git worktree the session is in; `null` after it leaves. |
| `summary`               | no             | no          | Conversation summary. May live in a different JSONL file than the conversation it describes. |
| `compact_boundary`      | no (usually)   | yes         | Marks an autocompaction event. Also appears as `system.subtype` in some versions. |
| `progress`              | no             | yes         | Long-running tool progress event. Should be skipped when reconstructing a transcript. |

---

## `user`

User prompts and synthesized user-role entries. More variety than the
name suggests. Several subclassifications exist:

### Direct user prompt

`message.content` is a bare string. No distinguishing field beyond that.

```json
{"type": "user", "message": {"role": "user", "content": "what does this file do?"}, ...}
```

### Slash command

`message.content` is a string containing XML-ish tags:

```
<command-message>jevan</command-message>
<command-name>/jevan</command-name>
<command-args>please enumerate…</command-args>
```

No separate `type` — the tags inside the content are the discriminator.

### Tool result carrier

The user entry that follows an assistant `tool_use`. `message.content`
is an array containing one or more `tool_result` blocks (see
[tools.md](tools.md)). Additional envelope fields:

- `toolUseResult` — top-level structured summary.
- `sourceToolAssistantUUID` — points at the assistant entry that issued
  the tool call.

The human did not type this turn; a consumer rendering the transcript
should typically fold it into the preceding assistant turn rather than
display it.

### Command output injection

Output from a local command (e.g. `!ls`) is injected back as a user
entry. Distinguishable by the content format; exact shape varies.

### Hook result

Output from `UserPromptSubmit` and similar hooks is injected as a user
entry.

### System caveat

An internal system-inserted user-role note. Rare; discriminator is a
subfield inside the content, not a top-level type.

### Classifying a user entry

The envelope alone does not tell you which subclass a `user` entry is —
you have to look at a handful of fields together. Decision tree:

```python
def classify_user(entry):
    # 1. Tool-result carrier: synthesized, not human-typed.
    if entry.get("toolUseResult") is not None:
        return "tool_result_carrier"
    if entry.get("sourceToolAssistantUUID") is not None:
        return "tool_result_carrier"

    msg = entry.get("message") or {}
    content = msg.get("content")

    # Array-form content with only tool_result parts: same class.
    if isinstance(content, list) and content and all(
        (p.get("type") == "tool_result") for p in content
    ):
        return "tool_result_carrier"

    # 2. Compaction summary: synthetic, flagged explicitly.
    if entry.get("isCompactSummary") is True:
        return "compact_summary"

    # 3. Slash command: string content with the XML-ish command tags.
    if isinstance(content, str) and "<command-name>" in content:
        return "slash_command"

    # 4. Command output injection: string content with the local-command-output tags.
    if isinstance(content, str) and (
        "<local-command-stdout>" in content or
        "<local-command-stderr>" in content
    ):
        return "command_output"

    # 5. Hook result: string content wrapped in UserPromptSubmit-style tags.
    if isinstance(content, str) and "<user-prompt-submit-hook>" in content:
        return "hook_result"

    # 6. System caveat: string content in <system-reminder>-class tags.
    if isinstance(content, str) and "<system-reminder>" in content:
        return "system_caveat"

    # 7. Otherwise: a direct user prompt.
    return "direct_prompt"
```

The tags embedded in content strings are the most reliable way to
distinguish synthesized-user entries from real ones. Order matters —
check `toolUseResult` / `sourceToolAssistantUUID` first, then
`isCompactSummary`, then fall through to content inspection.

---

## `assistant`

Claude's response. `message.content` is almost always an array of parts
(text, thinking, tool_use). See [messages.md](messages.md) for part
types.

Envelope specifics:
- Carries `requestId` (Anthropic API request ID).
- Carries `message.model`, `message.id`, `message.usage`,
  `message.stop_reason` (though `stop_reason` is frequently `null` on
  disk, even for completed turns — see
  [known-issues.md](known-issues.md)).

A single logical assistant turn can span **multiple `assistant` entries**
— Claude Code splits thinking, text, and tool_use into separate entries
within a turn. You cannot assume one `assistant` entry = one turn.

---

## `system`

Metadata entries. Discriminated by `subtype`. Treat `subtype` as an
open enumeration; new values appear across versions.

### `subtype: "turn_duration"`

Emitted after an assistant turn completes. Carries `durationMs` and
`messageCount`. Useful as an authoritative "turn ended" signal.

```json
{
  "type": "system",
  "subtype": "turn_duration",
  "durationMs": 57560,
  "messageCount": 24,
  ...
}
```

### `subtype: "compact_boundary"`

Marks an autocompaction. Carries `compactMetadata` (`trigger`, `preTokens`)
and `logicalParentUuid`. See [session-chains.md §Compaction](session-chains.md#compaction--compact_boundary).

In some versions this appears as a top-level `type: "compact_boundary"`
rather than `type: "system"` with this subtype; treat them as the same
concept.

### `subtype: "stop_hook_summary"`

Emitted by `Stop` hooks. Carries `hookCount`, `hookInfos`, `hookErrors`,
`preventedContinuation`, `stopReason`, `level: "suggestion"`.

### `subtype: "task_started"` / `"task_progress"` / `"task_notification"`

Task-tool lifecycle events.

### Other subtypes

Other values (e.g. `"init"`) have been observed. Log unknowns rather
than reject.

---

## `attachment`

Context the harness injects between messages: a tool-set change, a hook
result, a reminder to the model, the output style, the skill and agent
listings, a queued user message. `attachment.type` names the kind;
`total_tokens_reminder`, `batching_reminder_sent`,
`bash_output_audience_note`, and `hook_success` are the most frequent.
Attachments are on the
`parentUuid` chain (see "Write one chain" in
[writing-compatible-jsonl.md](writing-compatible-jsonl.md)); the one
native exception is a `hook_success` line that hangs off a `tool_use`
line as a side leaf next to the tool result. The
example is `type: "deferred_tools_delta"`, emitted when a deferred tool
is loaded into the active set.

```json
{
  "type": "attachment",
  "attachment": {
    "type": "deferred_tools_delta",
    "addedNames": ["WebFetch", "WebSearch"],
    "addedLines": ["WebFetch", "WebSearch"],
    "removedNames": []
  },
  "uuid": "...",
  "parentUuid": "...",
  ...
}
```

---

## `file-history-snapshot`

Captures file state at a given point, keyed by `messageId`:

```json
{
  "type": "file-history-snapshot",
  "messageId": "67602940-a209-437d-a791-72bf4c09c0ea",
  "snapshot": {
    "messageId": "67602940-a209-437d-a791-72bf4c09c0ea",
    "trackedFileBackups": {},
    "timestamp": "2026-04-02T13:59:26.313Z"
  },
  "isSnapshotUpdate": false
}
```

`trackedFileBackups` is usually empty in observed data; when populated,
it maps file paths to references into the content-addressed backups
under `~/.claude/file-history/`. These support Claude Code's undo
machinery.

Note: this entry type has **no `uuid`** — the `messageId` it carries
references a different entry's `uuid`. On resume there's a known bug
where these `messageId`s collide with real message `uuid`s; see
[known-issues.md](known-issues.md).

---

## `permission-mode`

Records the active permission mode. Strict three-field shape:

```json
{"type": "permission-mode", "permissionMode": "default", "sessionId": "..."}
```

Permission-mode values observed: `"default"`, `"acceptEdits"`, `"plan"`,
`"bypassPermissions"`.

**Strictness note:** adding any other fields to this entry (including
`uuid: ""`, `isSidechain: false`, or a trailing `parentUuid`) causes
Claude Code's loader to reject it. See
[writing-compatible-jsonl.md](writing-compatible-jsonl.md).

---

## `queue-operation`

Records typed-ahead message queueing during an assistant turn.

```json
{
  "type": "queue-operation",
  "operation": "enqueue",
  "content": "Probably phrase them as \"virtual artifacts\"?",
  "timestamp": "2026-04-16T19:33:16.814Z",
  "sessionId": "..."
}
```

`operation` values: `"enqueue"` (user queued a message), `"dequeue"`
(Claude Code consumed it). Note the top-level `content` field —
distinct from `message.content`.

---

## `last-prompt`

Cached last user prompt, for resume / history purposes:

```json
{"type": "last-prompt", "lastPrompt": "the repo is…", "sessionId": "..."}
```

---

## Session metadata lines

The line types below share one shape: `type`, `sessionId`, and one
payload key. None carries `uuid`, `timestamp` (except where noted),
`cwd`, or `parentUuid`. All are observed in a local store spanning
client versions 2.1.215 – 2.1.245; the first version that writes each
one is not known.

### `ai-title`

Generated session title. Rewritten as the session progresses.

```json
{"type": "ai-title", "aiTitle": "Fix the flaky test", "sessionId": "..."}
```

### `custom-title`

Explicitly set session title.

```json
{"type": "custom-title", "customTitle": "config-export", "sessionId": "..."}
```

### `agent-name`

Agent name for the session. In observed data it carries the same value
as the session's `custom-title`.

```json
{"type": "agent-name", "agentName": "config-export", "sessionId": "..."}
```

### `mode`

Active mode. Only `"normal"` is observed.

```json
{"type": "mode", "mode": "normal", "sessionId": "..."}
```

### `atis-latch`

Opaque latch value. `atis` is an empty string, a 16-character hex
string, or a ~190-character string. Its purpose is unknown.

```json
{"type": "atis-latch", "atis": "", "sessionId": "..."}
```

### `pr-link`

Pull request linked to the session. Carries a `timestamp`.

```json
{
  "type": "pr-link",
  "sessionId": "...",
  "prNumber": 233,
  "prUrl": "https://github.com/owner/repo/pull/233",
  "prRepository": "owner/repo",
  "timestamp": "2026-08-25T15:10:16.000Z"
}
```

### `frame-link`

Frame URL linked to a local file. Carries a `timestamp`. One sample
observed.

```json
{
  "type": "frame-link",
  "sessionId": "...",
  "path": "/home/user/project/design.html",
  "frameUrl": "https://...",
  "title": "...",
  "timestamp": "2026-08-25T15:10:16.000Z"
}
```

### `relocated`

New working directory for the session. `relocatedCwd` is an absolute
path.

```json
{"type": "relocated", "relocatedCwd": "/home/user/project", "sessionId": "..."}
```

### `worktree-state`

Git worktree the session is in. `worktreeSession` is an object on
entry and `null` after the session leaves the worktree.

```json
{
  "type": "worktree-state",
  "sessionId": "...",
  "worktreeSession": {
    "originalCwd": "/home/user/project",
    "preEnterOriginalCwd": "/home/user/project",
    "worktreePath": "/home/user/project/.claude/worktrees/topic",
    "worktreeName": "topic",
    "worktreeBranch": "user/topic",
    "originalBranch": "main",
    "originalHeadCommit": "b31f2c5...",
    "sessionId": "..."
  }
}
```

`originalBranch` and `originalHeadCommit` appear when the worktree was
created for the session. `enteredExisting: true` replaces them when
the session entered a worktree that already existed. The three path
fields are absolute. `worktreeSession.sessionId` equals the line's
`sessionId` in every observed sample.

A writer that moves a session to another directory or renames it must
decide what to do with `relocatedCwd`, the three `worktreeSession`
paths, and the inner `sessionId`. See
[writing-compatible-jsonl.md](writing-compatible-jsonl.md).

---

## `summary`

Conversation summary entries. Minimal shape:

```json
{"type": "summary", "summary": "...", "leafUuid": "..."}
```

Summaries are generated asynchronously relative to the conversation
they describe, and a summary may appear **in a different JSONL file**
than the session it summarizes. `leafUuid` is the UUID of the message
being summarized.

A parser that wants to associate summaries with their conversations
must cross-match by `leafUuid` across all files in the project
directory.

---

## `compact_boundary`

Marks an autocompaction. May appear either as a top-level `type` or as
`type: "system"` with `subtype: "compact_boundary"` — treat them as
equivalent.

```jsonc
{
  "type": "compact_boundary",
  "uuid": "...",
  "parentUuid": null,                     // always null
  "logicalParentUuid": "...",             // the real prior message UUID
  "compactMetadata": {
    "trigger": "auto",                    // or "manual"
    "preTokens": 180000
  },
  ...
}
```

Immediately followed by a synthetic `user`-role message with
`isCompactSummary: true` and `isVisibleInTranscriptOnly: true`
carrying the compacted summary as its content. See
[session-chains.md](session-chains.md) for how this interacts with
file rotation.

---

## `progress`

Long-running tool progress events. Emitted by tools that stream
intermediate output. Should be **skipped** when reconstructing a
conversation transcript — they represent incomplete state that is
superseded by the eventual `tool_result`.

---

## Sidechains

Sidechains aren't an entry type — they're a property (`isSidechain: true`)
that applies to `user`, `assistant`, and other entries alike. A
sidechain is a conversation thread spawned by the `Task` tool (a
subagent) or by the `/btw` slash command (aside questions).

### Representation

Sidechain entries carry:
- `isSidechain: true` on every entry in the thread.
- `agentId` — short hash identifying the subagent, e.g. `"a7bf2fd"`.
- The thread root has `parentUuid: null` and its `message.content` is
  the Task tool's input prompt (for Task-spawned agents).

Two layouts exist depending on Claude Code version:

1. **Inline** (newer, 2.1.x+) — sidechain entries are written into the
   same JSONL as the parent session, distinguished only by
   `isSidechain: true`.
2. **Separate file** (older) — subagents got their own
   `<project>/subagents/agent-<hash>.jsonl` files, with matching
   `.meta.json` sidecars.

### Usage accounting

Sidechain token usage does **not** count against the parent
conversation's context. Cache-read tokens from sidechains often
"mirror" the parent because the prompt cache is shared.

### Parent linkage

Sidechain entries carry their own `parentUuid` chain within the
sidechain thread. The link *to* the parent conversation is implicit —
established by the `Task` tool invocation in the parent, which carries
the `agentId` in its result.

---

## Type discrimination pseudocode

```python
def classify(entry):
    t = entry["type"]
    if t == "system":
        return ("system", entry.get("subtype", "unknown"))
    if t == "user":
        return ("user", classify_user(entry))   # see §Classifying a user entry
    return (t, None)
```
