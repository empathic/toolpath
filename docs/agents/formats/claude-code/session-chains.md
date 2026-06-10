# Session chains, rotation, and compaction

A single logical Claude Code conversation can span multiple JSONL files.
Claude Code **rotates** to a new file under several conditions, and
**compacts** the in-memory conversation mid-file when the context
window fills. Reconstructing a full logical conversation means
understanding both mechanics.

## Rotation triggers

Claude Code begins a new JSONL file (new session UUID, new filename) in
at least these situations:

- **Context overflow / autocompaction in some versions** — older
  versions rotated to an `acompact-<hash>.jsonl` on autocompact.
  Current versions usually keep compaction inline; see
  [§Compaction](#compaction--compact_boundary).
- **Exiting plan mode** — leaving plan mode reliably produces a new
  file.
- **Explicit resume** — `claude --resume <session-uuid>` continues
  into a new file.
- **`--fork-session`** — explicitly fork a session into a branch.
- **Manual user action** (e.g. `/clear` followed by continuation in
  some setups).

Don't rely on any filename prefix as a rotation signal — modern
versions give successor files the same UUID-naming scheme as fresh
sessions.

## Detecting a continuation

When a file exists that continues a prior session, the continuation is
signalled by one or more of these markers:

### 1. The bridge entry (primary signal)

The **first real entry** of a continuation file carries the **previous
session's** `sessionId`, not its own filename UUID.

```
file:  session-B.jsonl
line 1 (first real entry):  { "sessionId": "session-A", ... }  ← bridge
line 2+:                    { "sessionId": "session-B", ... }
```

This is the most reliable signal. Algorithm for classifying any file:

```python
def is_continuation_file(path):
    first_entry = read_first_entry(path)
    file_uuid = filename_stem(path)
    return first_entry.get("sessionId") != file_uuid
```

A file whose first entry's `sessionId` matches the filename is a
**chain head** (fresh session or the start of a multi-file chain).
A file whose first entry's `sessionId` is different is a **successor**
of that earlier sessionId.

### 2. The `slug` field

A conversation slug (e.g. `"crystalline-giggling-sunset"`) persists
across rotations. Files sharing the same `slug` belong to the same
logical conversation.

### 3. Duplicate `compact_boundary`

A continuation file may begin with a byte-for-byte identical
`compact_boundary` record copied from the parent, followed by the
synthetic compaction summary.

### 4. `parentUuid` bridge

The new file's first real entry's `parentUuid` points to the parent
file's last entry's `uuid`.

## Chain resolution

Given a set of files in a project directory, you can build the full
chain graph by:

1. For each file, read the first real entry and compute
   `(filename_uuid, first_entry_sessionId)`.
2. If `first_entry_sessionId != filename_uuid`, record an edge:
   `first_entry_sessionId → filename_uuid` (predecessor → successor).
3. Walk backward from any given file to the chain **head** (no
   predecessor) and forward to the chain **tail** (no successor).

A chain head's `sessionId` equals its filename UUID; every other
file in the chain has an incoming edge.

**Cycle defense:** don't assume the graph is always acyclic. Corrupt
data has produced self-loops and cycles in practice. Track visited
nodes during traversal.

## Reading a logical conversation

To present a multi-file conversation as a single stream:

1. Read the chain in order (head → tail).
2. Concatenate entries.
3. **Skip bridge entries.** The first real entry of any non-head file
   is a replay of the parent's last entry (it carries the parent's
   `sessionId`) and would cause duplicates if kept.

```python
def read_chain(chain_files):
    for i, path in enumerate(chain_files):
        for j, entry in enumerate(read_jsonl(path)):
            is_first_real = (j == first_real_line(path))
            is_bridge = (
                is_first_real and i > 0 and
                entry.get("sessionId") != filename_stem(path)
            )
            if is_bridge:
                continue
            yield entry
```

Or simpler: **keep only entries whose `sessionId` matches the filename's
session ID**. This drops bridge entries and any other replayed
prefix records.

## Compaction — `compact_boundary`

When the context window fills, Claude Code runs an autocompaction pass
that replaces the older portion of the conversation with a summary.
The JSONL records this inline with a `compact_boundary` entry.

### The boundary marker

```jsonc
{
  "type": "compact_boundary",    // or "system" with "subtype": "compact_boundary"
  "uuid": "...",
  "parentUuid": null,            // always null on the boundary
  "logicalParentUuid": "...",    // == preservedSegment.tailUuid (the kept tail's last message)
  "compactMetadata": {
    "trigger": "auto",           // "auto" or "manual" (user ran /compact)
    "preTokens": 1008497,        // conversation size before compaction
    "postTokens": 11528,         // size after the summary replaced the middle
    "durationMs": 106367,        // how long the compaction pass took
    "preservedSegment": {        // the recent tail kept verbatim
      "headUuid": "...",         //   first kept message
      "anchorUuid": "...",       //   == the synthetic summary's uuid (below)
      "tailUuid": "..."          //   last kept message (== logicalParentUuid)
    },
    "preservedMessages": {       // explicit enumeration of the kept tail
      "anchorUuid": "...",
      "uuids": ["...", "..."]    //   every uuid in [headUuid .. tailUuid]
    }
  },
  "sessionId": "...",
  "timestamp": "..."
}
```

Key property: `parentUuid` is `null`, resetting the DAG. The actual
prior message is referenced via `logicalParentUuid` so UIs can still
render the pre-compact history.

`compactMetadata` is richer than just `{trigger, preTokens}` — current
2.1.x boundaries enumerate **exactly which messages survived**:
`preservedSegment` gives the contiguous recent tail (`headUuid` →
`tailUuid`), and `preservedMessages.uuids` lists every uuid in it. The
`anchorUuid` is the synthetic summary message's own uuid. Older
versions emit only `{trigger, preTokens}` with no preserved-set
detail, so treat the segment fields as optional.

### The synthetic summary

Immediately after the boundary, a synthetic `user`-role message carries
the compacted-history summary as its content:

```jsonc
{
  "type": "user",
  "message": {
    "role": "user",
    "content": "...compacted summary text..."
  },
  "isCompactSummary": true,
  "isVisibleInTranscriptOnly": true,
  ...
}
```

These flags tell the loader:
- **`isCompactSummary: true`** — this is the synthesized summary, not
  real user input.
- **`isVisibleInTranscriptOnly: true`** — show it in the UI transcript
  but don't replay it to the model (the model sees the actual
  compacted context, which was already injected on the API side).

When rendering a transcript, skip these synthetic summary entries or
mark them specially; treating them as real user messages will confuse
consumers.

### Re-emitted messages with duplicate UUIDs

Observed in long `[1m]`-context 2.1.x sessions that auto-compact a
>1M-token conversation: in the run of entries **immediately before**
the `compact_boundary`, Claude re-materializes an earlier block of the
conversation (e.g. the original task and setup turns) as fresh entries
that **reuse the original `uuid`s**, with `parentUuid` chains
re-threaded into a synthetic linear sequence. So a single file can
contain the same `uuid` twice — once in its original position early in
the file, once in this re-emitted block.

These re-emitted entries:
- are **not** listed in `compactMetadata.preservedMessages` (that lists
  only the recent tail) — so they are a separate phenomenon from the
  marked preserved segment;
- carry the same `uuid` but a **different** `parentUuid` than their
  earlier counterpart (the original has the true lineage; the copy is
  re-parented linearly);
- sit between the preserved tail and the boundary marker.

This breaks the usual "uuid is unique within a file" assumption. A
consumer that keys on `uuid` (or stores steps under a `(file, uuid)`
primary key) **must dedupe** — keep the **first** occurrence, which
carries the real `parentUuid` lineage; the re-emitted copies are the
ones to drop. See [known-issues.md §Duplicate UUIDs at compaction
boundaries](known-issues.md#duplicate-uuids-at-compaction-boundaries).

### Compaction strategies

Several compaction strategies exist internally:

- **Full compaction** — the default autocompact. Everything older than
  a threshold is replaced by a summary.
- **Session-memory compaction** — summary preserved in session memory
  across rotations.
- **Microcompaction** — after the prompt cache's 1h TTL expires for
  older tool outputs, those outputs are cleared while keeping the last
  ~5 tool results. Less aggressive than full compaction.

From a format perspective, all three produce the same
`compact_boundary` + synthetic-summary sequence; the `trigger` value
and the `preTokens` count are the discriminators.

## Subagent / sidechain files

Older Claude Code versions stored subagents in separate files:

```
projects/<project>/subagents/agent-<hash>.jsonl
projects/<project>/subagents/agent-<hash>.meta.json
projects/<project>/subagents/agent-acompact-<hash>.jsonl   # compacted
```

Newer versions inline sidechain entries into the main session JSONL
with `isSidechain: true` and an `agentId` field. See
[entry-types.md §Sidechains](entry-types.md#sidechains).

## Pitfalls

- A file's `sessionId` on **non-first** entries is a less reliable
  signal than the first entry's `sessionId`. Hook-injected or
  cross-referenced entries sometimes carry a sessionId that matches a
  different file.
- The `sessions-index.json` index sometimes gets out of sync with the
  actual files on disk. Re-scan rather than trust the index blindly.
- A continuation can cross project directories if `cwd` changed
  mid-session (rare but possible).
