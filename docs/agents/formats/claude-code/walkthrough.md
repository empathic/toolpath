# Walkthrough: a session, line by line

Every other doc in this directory is a field-by-field reference. This
one is the complement: a single representative session, read linearly,
with commentary at each line about what you're seeing and which doc
defines it.

Use this to:

- Orient yourself before diving into the reference docs.
- Check that your parser handles every common shape in order.
- Have a concrete example to point at when explaining the format to a
  new contributor.

The JSONL below is **synthesized for illustration** — UUIDs,
timestamps, and `tool_use` IDs are not taken from a real file. The
shapes, field names, and ordering follow what we've observed in real
sessions. For the exact field definitions, follow the cross-links.

## Setting

Claude Code was launched in `/Users/alex/demo`. The file we're reading
is at `~/.claude/projects/-Users-alex-demo/c0ff2e4a-1111-4222-9333-444455556666.jsonl`.
See [directory-layout.md](directory-layout.md) for how that path was
chosen.

The user asked Claude to read a file and then summarize it. Then the
session got long enough to trigger a compaction, and the user asked a
follow-up.

---

## Line 1 — `permission-mode`

```json
{"type":"permission-mode","permissionMode":"default","sessionId":"c0ff2e4a-1111-4222-9333-444455556666"}
```

**Purpose:** records which permission mode was active when the session
started.

**Things to notice:**

- Exactly three fields. Adding anything — even `uuid: ""` or
  `timestamp` — causes the loader to reject the line. See
  [writing-compatible-jsonl.md §`permission-mode` entries must be
  exactly three fields](writing-compatible-jsonl.md#2-permission-mode-entries-must-be-exactly-three-fields).
- `sessionId` matches the filename stem, so this is a **chain head**
  (not a continuation file). See [session-chains.md](session-chains.md).

---

## Line 2 — `user` (direct prompt)

```json
{"type":"user","uuid":"u-0001","parentUuid":null,"sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T09:00:01.123Z","isSidechain":false,"userType":"external","entrypoint":"cli","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","message":{"role":"user","content":"what's in src/main.rs?"}}
```

**Purpose:** the first real user prompt.

**Things to notice:**

- `parentUuid: null` because this is the first conversation entry in
  the session. See [jsonl-envelope.md §Identity /
  position](jsonl-envelope.md#identity--position).
- `gitBranch: ""` (empty string, not `null`) because the cwd isn't a
  git repo. See [known-issues.md §Things that look like bugs but
  aren't](known-issues.md#things-that-look-like-bugs-but-arent).
- `message.content` is a **bare string**. Legal on user entries;
  **illegal on assistant entries** — see line 3. See
  [messages.md §The string-or-array `content`
  trap](messages.md#the-string-or-array-content-trap).
- Envelope keys are camelCase (`parentUuid`, `sessionId`,
  `isSidechain`, `userType`, `gitBranch`). API-adjacent keys inside
  `message` will be snake_case when they appear (`stop_reason`,
  `input_tokens`). See
  [jsonl-envelope.md §Field conventions](jsonl-envelope.md#field-conventions).

---

## Line 3 — `assistant` (tool call)

```json
{"type":"assistant","uuid":"a-0001","parentUuid":"u-0001","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T09:00:02.456Z","isSidechain":false,"userType":"external","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","requestId":"req_01ABCD","message":{"role":"assistant","type":"message","id":"msg_01EFGH","model":"claude-opus-4-6","content":[{"type":"text","text":"I'll read it."},{"type":"tool_use","id":"toolu_001","name":"Read","input":{"file_path":"/Users/alex/demo/src/main.rs"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":12,"output_tokens":48,"cache_creation_input_tokens":8421,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":8421},"service_tier":"standard"}}}
```

**Purpose:** Claude acknowledges and calls `Read`.

**Things to notice:**

- `message.content` is an **array**. Even a plain-text response is
  wrapped as `[{"type":"text","text":"..."}]`. The loader will crash
  on a bare-string content here. See
  [writing-compatible-jsonl.md §Assistant `message.content` must always
  be an array](writing-compatible-jsonl.md#1-assistant-messagecontent-must-always-be-an-array).
- Two parts in one entry — a `text` part *and* a `tool_use` part.
  That's common: Claude narrates the action in the same entry that
  issues the call. See [messages.md §Content part
  types](messages.md#content-part-types).
- `stop_reason: "tool_use"` signals that the assistant expects a tool
  result next.
- `requestId` is the Anthropic API request ID (prefix `req_`).
  `message.id` is the Anthropic API message ID (prefix `msg_`).
  Both are distinct from the envelope `uuid`. See
  [jsonl-envelope.md §API correlation](jsonl-envelope.md#api-correlation).
- `usage.cache_creation` breaks down `cache_creation_input_tokens`
  into 5-minute and 1-hour TTL buckets. See
  [usage.md §Prompt caching](usage.md#prompt-caching).

---

## Line 4 — `user` (tool result carrier)

```json
{"type":"user","uuid":"u-0002","parentUuid":"a-0001","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T09:00:02.891Z","isSidechain":false,"userType":"external","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","sourceToolAssistantUUID":"a-0001","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_001","content":"fn main() { println!(\"hello\"); }\n","is_error":false}]}}
```

**Purpose:** carries the tool result back. **Not human-typed** — a
transcript UI should fold this into the previous assistant turn rather
than render it as a user message.

**Things to notice:**

- `type: "user"` but every content part is `tool_result`. Use the
  `classify_user()` decision tree in
  [entry-types.md §Classifying a user entry](entry-types.md#classifying-a-user-entry)
  to recognize this.
- `sourceToolAssistantUUID: "a-0001"` is the convenience back-reference
  to the issuing assistant entry. The primary link is
  `tool_result.tool_use_id` ↔ `tool_use.id`. See
  [tools.md §The two-entry lifecycle](tools.md#the-two-entry-lifecycle).
- No top-level `toolUseResult` — `Read` doesn't have a structured-
  output summary. Compare to `Grep` in
  [tools.md §The top-level `toolUseResult`](tools.md#the-top-level-tooluseresult).

---

## Line 5 — `assistant` (text only, same logical turn)

```json
{"type":"assistant","uuid":"a-0002","parentUuid":"u-0002","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T09:00:03.210Z","isSidechain":false,"userType":"external","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","requestId":"req_01IJKL","message":{"role":"assistant","type":"message","id":"msg_01MNOP","model":"claude-opus-4-6","content":[{"type":"text","text":"It's a minimal Rust program that prints \"hello\"."}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":14,"cache_read_input_tokens":8421,"service_tier":"standard"}}}
```

**Things to notice:**

- `stop_reason: null` even though the turn ended cleanly. That's a
  timing artifact — the entry was written before the streaming API
  response finalized. Don't infer "turn still in flight" from this.
  See [known-issues.md §Missing `stop_reason` on assistant
  entries](known-issues.md#missing-stop_reason-on-assistant-entries).
- One logical assistant turn spans **two** `assistant` entries
  (`a-0001` with the tool call, `a-0002` with the text reply). Never
  assume one `assistant` JSONL entry = one turn. See
  [entry-types.md §`assistant`](entry-types.md#assistant).
- `cache_read_input_tokens` is high while `cache_creation_input_tokens`
  is zero — the prompt cache warmed up on the previous turn is now
  being read.

---

## Line 6 — `system` (turn_duration)

```json
{"type":"system","subtype":"turn_duration","uuid":"s-0001","parentUuid":"a-0002","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T09:00:03.215Z","durationMs":2092,"messageCount":4}
```

**Purpose:** authoritative "turn ended" signal. More reliable than
`stop_reason` for detecting end-of-turn.

**Things to notice:**

- `subtype` is an open enumeration. Treat unknown values as "log and
  keep." See
  [entry-types.md §`system`](entry-types.md#system).
- `messageCount: 4` refers to the four entries in this turn
  (`u-0001`, `a-0001`, `u-0002`, `a-0002`).

---

## …much later in the session…

After many more turns, the conversation approaches the context window
limit and triggers an autocompaction.

## Line 117 — `compact_boundary`

```json
{"type":"compact_boundary","uuid":"cb-0001","parentUuid":null,"logicalParentUuid":"a-0116","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T10:14:52.000Z","compactMetadata":{"trigger":"auto","preTokens":180000}}
```

**Things to notice:**

- `parentUuid: null` — the DAG resets here. The real prior message
  lives in `logicalParentUuid`. See
  [session-chains.md §The boundary
  marker](session-chains.md#the-boundary-marker).
- In some versions this shows up as `type: "system"` with
  `subtype: "compact_boundary"` instead of the top-level
  `compact_boundary` type. Treat as equivalent. See
  [format-changelog.md](format-changelog.md).
- `compactMetadata.trigger: "auto"` — the client compacted on its own,
  not because the user ran `/compact`.

## Line 118 — synthetic summary (user entry)

```json
{"type":"user","uuid":"u-0118","parentUuid":"cb-0001","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T10:14:52.010Z","isSidechain":false,"userType":"external","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"Summary: user asked to read src/main.rs; I reported it's a minimal hello-world Rust program. We then…"}}
```

**Things to notice:**

- `isCompactSummary: true` flags this as the synthesized summary, not
  real user input.
- `isVisibleInTranscriptOnly: true` tells the loader: show it in the
  UI but don't replay it to the model (the model already sees the
  real compacted context on the API side).
- UIs should mark this specially — treating it as "the user said X"
  will confuse consumers.

---

## Line 119 — `user` (real follow-up after compaction)

```json
{"type":"user","uuid":"u-0119","parentUuid":"u-0118","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T10:15:10.000Z","isSidechain":false,"userType":"external","entrypoint":"cli","cwd":"/Users/alex/demo","gitBranch":"","version":"2.1.112","message":{"role":"user","content":"can you add error handling?"}}
```

Back to a normal direct prompt. Life resumes.

---

## What a rotation would look like

Say this session gets long enough that Claude Code rotates to a new
file. The next file (`d1eeeeee-2222-4333-a444-555566667777.jsonl`)
would open with a **bridge entry**:

```json
{"type":"user","uuid":"u-0200","parentUuid":"a-0199","sessionId":"c0ff2e4a-1111-4222-9333-444455556666","timestamp":"2026-04-23T10:40:00.000Z","...":"..."}
```

Note the `sessionId` is the **previous** session's ID, not the new
file's. That mismatch is the primary signal that this file is a
continuation. See [session-chains.md §Detecting a
continuation](session-chains.md#detecting-a-continuation). A reader
reconstructing the logical conversation should **skip the bridge
entry** — it's a replay of the parent's last line.

Subsequent entries in the new file carry the new `sessionId`
(`d1eeeeee-…`).

## What a sidechain would look like

A `Task` tool call spawns a subagent. In 2.1.x the sidechain entries
are **inlined into the same JSONL**, marked by:

- `isSidechain: true` on every entry in the subagent thread
- `agentId` — a short hash identifying the subagent (e.g. `"a7bf2fd"`)
- The thread root has `parentUuid: null`; its `message.content` is
  the Task tool's input prompt

Sidechain `usage` is accounted on the sidechain's own assistant
entries and does not roll up into the parent conversation's totals.
See [entry-types.md §Sidechains](entry-types.md#sidechains).

---

## If you made it here

You now have a mental model for the line-level shape of a Claude Code
session. The reference docs fill in the corners:

- The full envelope field catalogue: [jsonl-envelope.md](jsonl-envelope.md).
- Every entry type and its variants: [entry-types.md](entry-types.md).
- The `message` object and content parts: [messages.md](messages.md).
- Tool-call lifecycle in detail: [tools.md](tools.md).
- Token accounting: [usage.md](usage.md).
- Multi-file chains and compaction: [session-chains.md](session-chains.md).
- Everything outside the session JSONL: [peripheral-files.md](peripheral-files.md).
- Rules for writing your own: [writing-compatible-jsonl.md](writing-compatible-jsonl.md).
- Bugs, drift, corruption modes: [known-issues.md](known-issues.md).
- Version-keyed field history: [format-changelog.md](format-changelog.md).
