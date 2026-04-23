# Claude Code on-disk format

> **Reference revision:** 2026-04-23
> **Tracks:** Claude Code 2.1.x
> **First-hand samples:** 2.1.37, 2.1.90, 2.1.110, 2.1.112
>
> When you change anything in this directory, bump the revision date
> here and in [format-changelog.md](format-changelog.md) so downstream
> readers can tell whether a cited rule is current.

Claude Code (Anthropic's CLI coding agent) persists every conversation,
every settings change, and a fair amount of supporting state to disk
under `~/.claude/`. Anthropic documents some of this in passing but has
never published a specification of the JSONL line format, the session
directory layout, or the rules that govern things like session chaining
and compaction.

These documents are our working reference for what Claude Code actually
writes to disk, at what version, and why. The target audience is anyone
building a tool that reads, writes, or transforms Claude Code session
data.

## How the docs are organized

Each doc is focused on one aspect of the format. Read them in this order
if you're new; otherwise, skip to what you need. If you prefer concrete
examples to field catalogues, start with the **walkthrough** (#11) and
use the reference docs for lookup.

1. **[directory-layout.md](directory-layout.md)** — what files and
   directories live under `~/.claude/` and how they're named.
2. **[jsonl-envelope.md](jsonl-envelope.md)** — the top-level fields that
   wrap every line of a session JSONL.
3. **[entry-types.md](entry-types.md)** — the `type` discriminant and
   every entry variant we've observed, including sidechains.
4. **[messages.md](messages.md)** — the `message` object, content-part
   types (text/thinking/tool_use/tool_result), and role values.
5. **[tools.md](tools.md)** — how tool calls are recorded:
   `tool_use`/`tool_result` pairing, `tool_use_id` linkage, and the
   per-tool shape of the top-level `toolUseResult` summary.
6. **[usage.md](usage.md)** — `message.usage` and the prompt-cache TTL
   breakdown.
7. **[session-chains.md](session-chains.md)** — when Claude Code rotates
   to a new file, how continuations are signalled, and the `compact_boundary`
   mechanic.
8. **[peripheral-files.md](peripheral-files.md)** — everything that
   isn't a session JSONL: `history.jsonl`, `todos/`, `shell-snapshots/`,
   `file-history/`, `statsig/`, etc.
9. **[writing-compatible-jsonl.md](writing-compatible-jsonl.md)** —
   empirically discovered constraints if you want Claude Code to load
   JSONL that your tool produced.
10. **[known-issues.md](known-issues.md)** — format-level bugs,
    corruption modes, and version drift to defend against.
11. **[walkthrough.md](walkthrough.md)** — a representative session
    read linearly, line by line, with cross-links back to the
    reference docs at each step.
12. **[format-changelog.md](format-changelog.md)** — version-keyed
    record of field and behavior changes across Claude Code releases.

## Scope and sourcing

The format is undocumented at the envelope level. These docs are
compiled from:

- Inspection of real session files under `~/.claude/projects/` across
  Claude Code versions 2.1.37 through 2.1.112.
- Reading code that produces or consumes the format (both ours and other
  tools in the local empathic monorepo).
- Round-trip experiments: writing JSONL and observing whether Claude
  Code's loader accepts it.

Where a claim is empirical or version-dependent, the doc says so. Where
it's a guess, the doc says so.

## Conventions

- **Field names** are shown as they appear in JSON. Envelope keys are
  camelCase (`parentUuid`), API-adjacent keys inside `message` are
  snake_case (`stop_reason`).
- **"Observed"** means seen in real files on disk.
- **"Expected"** means we have structural reasons to believe it's
  there (e.g. a loader rejects it when absent) even if we haven't
  enumerated it in samples.
- **Versions in parentheses** (e.g. "2.1.90+") indicate when a field or
  behavior first appeared or changed, to the extent we know.
- **Keep headings anchor-stable.** Cross-links use GitHub's auto-anchors
  (lowercased, punctuation stripped, spaces to hyphens). Avoid em-dashes
  and other decorative punctuation in a heading that is, or might be,
  linked to — they render inconsistently and tend to bite renames.

## Field index

Quick lookup: which doc defines a given field? Fields local to a single
entry type are grouped under it.

| Field                          | Defined in |
|--------------------------------|------------|
| `agentId`                      | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md §Sidechains](entry-types.md#sidechains) |
| `attachment`                   | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md](entry-types.md) |
| `cache_creation` / `cache_*_input_tokens` | [usage.md](usage.md) |
| `caller` (on `tool_use`)       | [messages.md](messages.md) |
| `compactMetadata`              | [entry-types.md](entry-types.md), [session-chains.md](session-chains.md) |
| `content` (envelope, on `queue-operation`) | [entry-types.md](entry-types.md) |
| `content` (inside `message` / `tool_result`) | [messages.md](messages.md) |
| `cwd`                          | [jsonl-envelope.md](jsonl-envelope.md), [writing-compatible-jsonl.md](writing-compatible-jsonl.md) |
| `durationMs`                   | [entry-types.md](entry-types.md) |
| `entrypoint`                   | [jsonl-envelope.md](jsonl-envelope.md) |
| `gitBranch`                    | [jsonl-envelope.md](jsonl-envelope.md) |
| `hookCount` / `hookInfos` / `hookErrors` | [jsonl-envelope.md](jsonl-envelope.md) |
| `id` (inside `message`)        | [messages.md](messages.md) |
| `id` (on `tool_use`)           | [messages.md](messages.md), [tools.md](tools.md) |
| `iterations`                   | [usage.md](usage.md) |
| `inference_geo`                | [usage.md](usage.md) |
| `input_tokens` / `output_tokens` | [usage.md](usage.md) |
| `isCompactSummary`             | [entry-types.md](entry-types.md), [session-chains.md](session-chains.md) |
| `isMeta`                       | [jsonl-envelope.md](jsonl-envelope.md) |
| `isSidechain`                  | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md §Sidechains](entry-types.md#sidechains) |
| `isSnapshotUpdate`             | [entry-types.md](entry-types.md) |
| `isVisibleInTranscriptOnly`    | [entry-types.md](entry-types.md), [session-chains.md](session-chains.md) |
| `lastPrompt`                   | [entry-types.md](entry-types.md) |
| `leafUuid`                     | [entry-types.md](entry-types.md) |
| `level`                        | [jsonl-envelope.md](jsonl-envelope.md) |
| `logicalParentUuid`            | [entry-types.md](entry-types.md), [session-chains.md](session-chains.md) |
| `message`                      | [messages.md](messages.md) |
| `messageCount` (envelope)      | [entry-types.md](entry-types.md) |
| `messageId`                    | [jsonl-envelope.md](jsonl-envelope.md), [entry-types.md](entry-types.md), [known-issues.md](known-issues.md) |
| `model` (inside `message`)     | [messages.md](messages.md) |
| `operation`                    | [entry-types.md](entry-types.md) |
| `parentUuid`                   | [jsonl-envelope.md](jsonl-envelope.md), [known-issues.md](known-issues.md) |
| `permissionMode`               | [entry-types.md](entry-types.md), [writing-compatible-jsonl.md](writing-compatible-jsonl.md) |
| `preventedContinuation`        | [jsonl-envelope.md](jsonl-envelope.md) |
| `requestId`                    | [jsonl-envelope.md](jsonl-envelope.md) |
| `role`                         | [messages.md](messages.md) |
| `server_tool_use`              | [usage.md](usage.md) |
| `service_tier`                 | [usage.md](usage.md) |
| `sessionId`                    | [jsonl-envelope.md](jsonl-envelope.md), [session-chains.md](session-chains.md) |
| `signature` (on `thinking`)    | [messages.md](messages.md), [writing-compatible-jsonl.md](writing-compatible-jsonl.md) |
| `slug`                         | [jsonl-envelope.md](jsonl-envelope.md), [session-chains.md](session-chains.md) |
| `snapshot`                     | [entry-types.md](entry-types.md) |
| `sourceToolAssistantUUID`      | [jsonl-envelope.md](jsonl-envelope.md), [tools.md](tools.md) |
| `speed`                        | [usage.md](usage.md) |
| `stop_reason` / `stop_sequence` | [messages.md](messages.md), [known-issues.md](known-issues.md) |
| `stopReason` (envelope)        | [jsonl-envelope.md](jsonl-envelope.md) |
| `subtype`                      | [entry-types.md](entry-types.md) |
| `summary` / `leafUuid`         | [entry-types.md](entry-types.md) |
| `thinking` / `signature`       | [messages.md](messages.md) |
| `thinkingMetadata`             | [jsonl-envelope.md](jsonl-envelope.md) |
| `timestamp`                    | [jsonl-envelope.md](jsonl-envelope.md) |
| `tool_use` / `tool_result`     | [messages.md](messages.md), [tools.md](tools.md) |
| `tool_use_id`                  | [messages.md](messages.md), [tools.md](tools.md) |
| `toolUseResult`                | [jsonl-envelope.md](jsonl-envelope.md), [tools.md](tools.md) |
| `trackedFileBackups`           | [entry-types.md](entry-types.md), [peripheral-files.md](peripheral-files.md) |
| `type` (envelope)              | [entry-types.md](entry-types.md) |
| `type` (content-part)          | [messages.md](messages.md) |
| `usage`                        | [usage.md](usage.md) |
| `userType`                     | [jsonl-envelope.md](jsonl-envelope.md) |
| `uuid`                         | [jsonl-envelope.md](jsonl-envelope.md) |
| `version`                      | [jsonl-envelope.md](jsonl-envelope.md) |

For the mapping from these JSON keys to Rust fields in
`ConversationEntry`, see the parser-surface table in
[jsonl-envelope.md](jsonl-envelope.md#parser-surface-vs-format-surface).

## Maintenance

When a new field, entry type, or behavior shows up in the wild, update
the relevant doc in the same change. The index above is the table of
contents; keep it in sync. When you add or rename a field, update the
field index in this README too.
