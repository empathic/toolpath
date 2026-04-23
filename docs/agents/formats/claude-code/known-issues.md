# Known issues and corruption modes

A parser that wants to be robust against real-world Claude Code output
needs to defend against format-level bugs, races, and drift. This is
the list we've built up from upstream issue trackers, our own testing,
and adjacent tools.

## Format bugs Anthropic has shipped

### Dangling `parentUuid` references

An entry's `parentUuid` may reference a UUID that doesn't exist
anywhere in the file. Observed intermittently; causes DAG reconstruction
to fail if you assume every `parentUuid` resolves.

**Defense:** during traversal, treat missing parents as roots rather
than erroring out.

### `file-history-snapshot.messageId` collisions on resume

On resume, `file-history-snapshot` entries reuse the `uuid` of prior
message entries as their `messageId`. This means if you index entries
by UUID, the snapshot collides with the real message.

**Impact:** up to 25% of entries unreachable via `parentUuid`
traversal in heavily-resumed sessions.

**Defense:** don't use `messageId` as a unique key across entry types.
Key by `(type, uuid)` or by `(type, messageId)` specifically for
snapshots.

### Missing `stop_reason` on assistant entries

Assistant entries are frequently persisted with `stop_reason: null`
even for turns that completed normally — the entry is written before
the streaming API response finalizes.

**Defense:** don't treat `stop_reason == null` as "in-flight". Infer
`"end_turn"` for assistant entries missing it, unless you have direct
evidence the turn was interrupted.

### Autocompact can destroy in-progress work

Under some conditions an autocompact pass can replace in-progress
scratch work with a summary, effectively losing it. This is a Claude
Code bug, not a format bug — but if you're archiving sessions for
replay, a post-compact file may be less useful than the pre-compact
snapshot.

**Defense:** if continuity matters, checkpoint before compaction.

## Race conditions

### Multi-terminal writes to the same project

Multiple Claude Code instances launched in the same cwd may write to
**the same project directory** at overlapping times, producing
interleaved turns across session files or (more rarely) interleaved
lines within a single file.

**Defense:** a session reader should gate discovery by `created_at`
timestamps or by content-match against known user input, so that one
instance doesn't accidentally pick up another's session.

### `sessions-index.json` drift

The optional `~/.claude/sessions/sessions-index.json` can be missing,
stale, or inconsistent with the files actually on disk.

**Defense:** treat the index as a cache; rescan the filesystem when
precision matters.

## Format-level invisibles

These aren't bugs, but they trip up consumers who expect the JSONL to
be a complete record of what happened.

### Permission prompts

When Claude Code gates a tool call behind a permission prompt, the
prompt itself and the user's accept/deny decision leave **no JSONL
entry**. The only observable signal is a gap in timestamps between
the `tool_use` and the corresponding `tool_result`.

**Defense:** if you're trying to distinguish "Claude took 5 seconds to
run Bash" from "user took 5 seconds deciding to allow Bash," you can't.
Treat timestamp gaps around tool results with uncertainty.

### Mid-turn text-only assistant entries

A single logical assistant turn can span multiple `assistant` JSONL
entries — thinking, text, and `tool_use` can each be separate entries.
Text-only assistant entries can appear **between** tool-call batches,
not only at end-of-turn.

**Defense:** don't treat "assistant emitted text" as "turn is over."
Use `stop_reason: "end_turn"` or a following `system/turn_duration`
entry as the authoritative end-of-turn signal.

### Agent-progress nesting variance

Some agent-progress entries wrap the message double-nested
(`data.message.message` instead of `data.message`) depending on
version. If you're parsing Task-tool progress events, try both paths.

## Drift between versions

The format is stable at the coarse level but drifts in details.
Patterns:

- **New optional fields** appear additively. Unknown envelope fields
  should be preserved through reads/writes.
- **Entry-type enumerations** grow. New `system.subtype` values and
  new top-level `type` values appear without notice.
- **Tool input schemas** change when Anthropic ships built-in tool
  updates. Preserve unknown input keys.
- **`usage` subfields** (notably `server_tool_use`, `iterations`,
  `cache_creation`) were added in 2.0.x → 2.1.x. Older files lack
  them.

**Defense:** always flatten unknown fields into a catch-all rather
than rejecting. Version-gate any behavior that assumes a specific
subfield is present.

## Corruption modes to expect

- **Truncated last line.** If Claude Code was killed mid-write, the
  final line of a session JSONL may be partial or invalid JSON.
  Expect an unterminated-string or unexpected-EOF parse error on
  the tail.
- **Empty lines.** Blank lines appear mid-file in some samples. Skip
  them rather than error.
- **Whitespace-only lines.** Same handling as empty lines.
- **Large output overflow.** Very large tool outputs (pages of text)
  may be spilled to `projects/<project>/<session>/tool-results/`
  rather than inlined. A parser that assumes `tool_result.content` is
  always inline may return stub content instead of the real output.
- **Cycles in the parent DAG.** Rare, but possible with corrupted
  data. Traverse defensively.

## Things that look like bugs but aren't

- **`gitBranch: ""`** — empty string, not null, is the correct
  representation when cwd isn't a git repo.
- **`stop_reason: null`** — see above. Not a crash; a timing artifact.
- **Multiple assistant entries per turn** — intentional. Thinking,
  text, and tool calls get their own entries.
- **Tool-result-only user entries** — intentional. These are synthesized,
  not user input. Fold them into the preceding assistant turn when
  rendering.
- **First entry's `sessionId` != filename stem** — intentional. Bridge
  entry marking a session continuation. See
  [session-chains.md](session-chains.md).
- **`compact_boundary.parentUuid: null`** — intentional. The real
  prior message is in `logicalParentUuid`.
