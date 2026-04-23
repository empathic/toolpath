# Format changelog

A version-keyed record of field and behavior changes we've seen in the
Claude Code on-disk format. This is compiled from samples on disk and
from reading upstream code; it is **not** a changelog Anthropic
publishes, and the exact patch version where something landed is
usually a best guess.

## How to read this

- **"Observed across 2.1.37 – 2.1.112"** means every sampled version in
  that range contained it. Versions outside that range are not first-hand.
- **"2.1.x+ (origin unclear)"** means we saw it in 2.1.x samples but
  don't know which patch introduced it.
- **"2.0.x+"** means we believe from upstream code / changelogs that
  the field was present in 2.0.x, even though we don't have first-hand
  samples from that era.
- **"Pre-sample era"** entries describe the pre-2.0 layout from code
  reading and upstream references, not from files we've inspected.

When precision matters, treat a version here as an upper bound ("no
later than") unless the note says otherwise.

## Format-revision stamp

This reference tracks Claude Code **2.1.x**. First-hand samples span
client versions 2.1.37, 2.1.90, 2.1.110, and 2.1.112. Reference
revision: **2026-04-23**.

---

## Claude Code 2.1.x

### Envelope

| Field / behavior                                       | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| `type`, `uuid`, `timestamp`, `sessionId`, `parentUuid` | Observed across 2.1.37 – 2.1.112                   |
| `cwd`, `gitBranch`, `version`, `userType`, `entrypoint`| Observed across 2.1.37 – 2.1.112                   |
| `requestId` on assistant entries                       | Observed across 2.1.37 – 2.1.112                   |
| `slug` (human-readable conversation slug)              | 2.1.x+ (origin unclear); persists across rotations |
| `agentId` on sidechain entries                         | 2.1.x+ (inline-sidechain layout)                   |
| `isSidechain: true` for inline sidechains              | 2.1.x+; replaces the older separate-file layout    |
| `thinkingMetadata` on some user entries                | 2.1.x+ (origin unclear)                            |
| Hook-injected envelope fields (`hookCount`, `hookInfos`, `hookErrors`, `preventedContinuation`, `stopReason`, `level`) | 2.1.x+ (origin unclear) |

See [jsonl-envelope.md](jsonl-envelope.md) for field definitions.

### Entry types

| Entry type                                             | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| `user`, `assistant`, `system`                          | Observed across 2.1.37 – 2.1.112                   |
| `file-history-snapshot`                                | Observed across 2.1.37 – 2.1.112                   |
| `permission-mode`                                      | Observed across 2.1.37 – 2.1.112                   |
| `summary`                                              | Observed across 2.1.37 – 2.1.112                   |
| `attachment` (deferred-tool deltas)                    | 2.1.x+ (origin unclear)                            |
| `queue-operation` (typed-ahead message enqueue/dequeue)| 2.1.x+ (origin unclear)                            |
| `progress` (streaming tool output)                     | 2.1.x+ (origin unclear)                            |
| `last-prompt`                                          | 2.1.x+ (origin unclear)                            |
| `compact_boundary` as top-level `type`                 | Newer variant; coexists with older `type: "system"` + `subtype: "compact_boundary"` |
| `system.subtype` values: `turn_duration`, `stop_hook_summary`, `task_started`/`task_progress`/`task_notification` | 2.1.x+ (origin unclear) |

See [entry-types.md](entry-types.md).

### `message.usage` subfields

| Subfield                                               | Since                                              |
|--------------------------------------------------------|----------------------------------------------------|
| `input_tokens`, `output_tokens`                        | Always                                             |
| `cache_creation_input_tokens`, `cache_read_input_tokens` (flat) | Always (when caching was used)            |
| `cache_creation: { ephemeral_5m_input_tokens, ephemeral_1h_input_tokens }` | 2.0.x+                         |
| `service_tier`                                         | 2.0.x+                                             |
| `server_tool_use: { web_search_requests, web_fetch_requests }` | 2.1.x+                                     |
| `iterations`                                           | 2.1.x+                                             |
| `inference_geo`, `speed`                               | 2.1.x+ (often empty on observed samples)           |

See [usage.md](usage.md).

### Content parts

| Part / behavior                                        | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| `text`, `tool_use`, `tool_result`                      | Observed across 2.1.37 – 2.1.112                   |
| `thinking` with `signature`                            | Observed on models that support extended thinking (`claude-opus-4-6`, later `-4-7`) |
| `redacted_thinking`                                    | Format-defined; rarely on disk in coding sessions  |
| `image`, `document`, `server_tool_use`, `web_search_tool_result` | Format-defined; rarely on disk in coding sessions |

`thinking` parts require a valid Anthropic-issued `signature`; otherwise
they are silently dropped on resume. See [messages.md](messages.md).

### Session chains and compaction

| Behavior                                               | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| Bridge entry (first real entry of successor file carries the **previous** session's `sessionId`) | Observed across 2.1.37 – 2.1.112 |
| `slug` persistence across rotations                    | 2.1.x+                                             |
| Duplicate `compact_boundary` at top of successor files | 2.1.x+ (observed intermittently)                   |
| Inline compaction (`compact_boundary` + synthetic `user` summary with `isCompactSummary: true` / `isVisibleInTranscriptOnly: true`) | 2.1.x+ is the default |
| Rotation on autocompact to a separate `acompact-<hash>.jsonl` | Older behavior; not observed in 2.1.x         |
| Inline sidechains (`isSidechain: true` + `agentId` in the main file) | 2.1.x+ default                           |
| Separate-file sidechains under `subagents/agent-<hash>.jsonl` | Older behavior; compatibility path only     |
| `compactMetadata.trigger`: `"auto"` / `"manual"`       | Observed in 2.1.x                                  |
| `compactMetadata.preTokens`                            | Observed in 2.1.x                                  |

See [session-chains.md](session-chains.md).

### Peripheral files

| Path                                                   | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| `~/.claude/todos/`                                     | **Marked legacy**; current versions inline TodoWrite state into `tool_result` entries |
| `~/.claude/history.jsonl`                              | Observed across 2.1.37 – 2.1.112; Unix-millis timestamps (distinct from session JSONL ISO-8601) |
| `~/.claude/history.jsonl` per-entry `sessionId`        | Absent on older entries; present on newer ones     |
| `~/.claude/sessions/sessions-index.json`               | Observed in some versions; can be stale or missing |
| `~/.claude/file-history/<session-uuid>/<contentHash>@v<versionNumber>` | Observed across 2.1.37 – 2.1.112   |
| `~/.claude/shell-snapshots/snapshot-<shell>-<unix-millis>-<random>.sh` | Observed across 2.1.37 – 2.1.112   |

See [peripheral-files.md](peripheral-files.md).

### Tools

The built-in tool set is the fastest-moving surface. We do not track it
as an enumerated changelog here — treat [tools.md §Common tool `input`
shapes](tools.md#common-tool-input-shapes) as illustrative, and check
Anthropic's tool documentation for the authoritative current list.

What *is* stable enough to version-track:

| Behavior                                               | Status in 2.1.x                                    |
|--------------------------------------------------------|----------------------------------------------------|
| `tool_use` / `tool_result` two-entry pairing           | Observed across 2.1.37 – 2.1.112                   |
| `sourceToolAssistantUUID` back-reference on tool-result carrier | Observed across 2.1.37 – 2.1.112          |
| Top-level `toolUseResult` as sibling of `message`      | Observed across 2.1.37 – 2.1.112 for structured-output tools |
| Parallel tool calls (multiple `tool_use` in one assistant entry) | Observed across 2.1.37 – 2.1.112         |
| Very-large-output spill to `projects/<project>/<session>/tool-results/` | Documented behavior; not observed first-hand in our samples |

---

## Pre-sample era (before 2.1.37)

Drawn from upstream code reading and adjacent tooling, not from files
we've inspected directly. Treat as orientation, not as verified fact.

- **Subagents stored per-file** under
  `projects/<project>/subagents/agent-<hash>.jsonl`, with a matching
  `.meta.json` sidecar, and autocompacted subagents under
  `agent-acompact-<hash>.jsonl`.
- **Autocompaction rotated to a new file** named `acompact-<hash>.jsonl`
  rather than recording an inline `compact_boundary`.
- **`message.usage`** was flatter — no `cache_creation` TTL breakdown,
  no `service_tier`, no `server_tool_use`, no `iterations`.
- **`history.jsonl`** entries did not always carry `sessionId`.

---

## Process

When you add a field, entry type, or behavior note to any other doc in
this directory, add a corresponding row here in the same change. Cite
the version where you can; cite "2.1.x+ (origin unclear)" when you
can't. Bump the format-revision stamp at the top of this file *and*
the matching stamp in [README.md](README.md) whenever you update the
changelog.
