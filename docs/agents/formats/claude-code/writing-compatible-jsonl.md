# Writing JSONL that Claude Code will load

If your tool produces session JSONL that you want Claude Code to load
(for resume, for replay, for synthesizing a starting state), there are
rules beyond "valid JSON per line." The loader is strict about several
things. This document captures the constraints we've discovered
empirically; the list is not exhaustive.

## Where Claude Code looks

Claude Code reads sessions from
`~/.claude/projects/<sanitized-cwd>/*.jsonl`. To make a file visible to
`claude --resume` or the session picker:

1. Determine the **canonical** absolute path of the intended working
   directory (on macOS, resolve `/tmp` → `/private/tmp`).
2. Sanitize by replacing `/` and `_` with `-`.
3. Create the directory at `~/.claude/projects/<sanitized>/` if it
   doesn't exist.
4. Write `<session-uuid>.jsonl` inside it. The UUID must be a UUIDv4;
   the filename stem is the session ID.

See [directory-layout.md](directory-layout.md) for details.

## Hard rules

These are empirically confirmed. Violating them causes the loader to
reject or mis-render the session.

### 1. Assistant `message.content` must always be an array

Claude Code's loader unconditionally calls array methods
(`.map(…)` and similar) on `message.content` for `assistant` entries.
A bare-string content crashes the loader — even for a plain text
response.

```jsonc
// ❌ Broken
{"type": "assistant", "message": {"role": "assistant", "content": "Done."}}

// ✅ Correct
{"type": "assistant", "message": {"role": "assistant",
  "content": [{"type": "text", "text": "Done."}]}}
```

User entries tolerate both shapes.

### 2. `permission-mode` entries must be exactly three fields

Observed legal shape:

```json
{"type": "permission-mode", "permissionMode": "default", "sessionId": "..."}
```

Adding any additional field — even seemingly benign ones like `uuid: ""`,
`isSidechain: false`, or a `timestamp` — causes the loader to reject
the entry. If you're using a typed serializer that emits default
values, emit this entry as raw JSON instead.

### 3. `cwd` must be the canonical path

The `cwd` on an entry must match the pre-sanitization form of the
directory name under `projects/`. If you place a file in
`projects/-private-tmp-foo/` but put `"cwd": "/tmp/foo"` on the
entries, Claude Code's behavior is inconsistent — the session may
appear but tool invocations may fail.

Two metadata lines carry absolute paths under other keys:
`relocated.relocatedCwd`, and `originalCwd`, `preEnterOriginalCwd`,
and `worktreePath` inside `worktree-state.worktreeSession`. How the
loader uses them on resume is not known. A writer that moves a session
to another directory decides for each whether to rewrite it.

## Strong conventions

Real Claude Code entries always carry these envelope fields. The
loader does not reject their absence outright, but UI rendering and
downstream tools may behave oddly without them:

| Field         | Why it matters |
|---------------|----------------|
| `userType`    | `"external"` on all typical user-written entries. |
| `entrypoint`  | `"cli"` (or appropriate value for the launching surface). |
| `cwd`         | Used for git/branch context and to validate project alignment. |
| `version`     | Claude Code client version string. |
| `gitBranch`   | Current branch; **empty string**, not null, when cwd isn't a repo. |
| `sessionId`   | Must match the file's session UUID (or be a bridge entry — see below). `worktree-state` lines carry a second copy in `worktreeSession.sessionId`; keep both equal. |
| `uuid`        | UUIDv4 per entry. |
| `timestamp`   | ISO-8601 with millisecond precision. |
| `parentUuid`  | The prior entry's `uuid` (or `null` for the first entry). |
| `isSidechain` | Explicit `false` on non-sidechain entries. |

## Structural rules

### Write one chain

Claude Code writes every line of a session file into one `parentUuid`
chain. Each line names the line written before it, whatever its type:
`attachment` and `system` lines are on the chain with the `user` and
`assistant` lines.

`claude -r` reads one chain from a leaf to the root. When the file has
a second leaf, the resume can start from that leaf and stop where its
chain leaves the message chain: a side chain of attachments off the
last tool call renders the transcript up to that tool call and drops
the tool result and the reply after it. The rule that picks the leaf is
not known, so the file must not give it a choice.

- Every line except the first names the line before it. The one other
  line with `parentUuid: null` is a `compact_boundary`, whose
  `logicalParentUuid` names the line it follows. Write it in its place
  and continue the chain from it.
- An `attachment` or `system` line that follows a message stays between
  that message and the next one.
- A one-line `hook_success` side leaf off a `tool_use` line occurs in
  native files and loads. Do not depend on it for other types.

### Preserve `tool_use` / `tool_result` pairing

Every `tool_use` part in an assistant entry must be followed by a
corresponding user entry carrying a `tool_result` part with the same
`tool_use_id`. Assistant entries whose `stop_reason` is `"tool_use"`
without a matching result in the following user entry confuse the
loader.

If you're constructing synthetic conversation state with tool calls,
you must synthesize plausible results.

### Preserve `thinking` signatures

If you include `thinking` parts in assistant content, each must carry
a valid `signature`. A `thinking` part without a signature (or with a
signature that doesn't verify) will be **dropped** when the session is
resumed — the API won't replay it to the model. If you're constructing
synthetic state that includes thinking, consider whether you actually
need it; constructing a valid signature is not possible without an
Anthropic-signed source.

### Assistant entries typically have non-null `model`, `id`, `type`

- `model` — e.g. `"claude-opus-4-6"`.
- `id` — Anthropic-format API message ID (prefix `msg_`). Synthesized
  IDs should follow the shape `msg_<base62>`.
- `type: "message"` — literal.

Omitting these may render OK in the transcript but break resume.

### `stop_reason` tolerance

`stop_reason` is often `null` on real entries (written before the
stream finalizes), so the loader tolerates `null`. Safe values for
synthetic entries: `"end_turn"`, `"tool_use"`, `null`.

## Round-trip rules

If you read a session and want to write it back out losslessly:

- **Preserve unknown fields.** A field you don't recognize on read must
  be round-tripped on write. New Claude Code fields arrive
  additively.
- **Preserve field order at your peril.** The loader doesn't care, but
  diff tools and humans do. Emit envelope fields before `message`
  for readability.
- **Preserve `content` array ordering.** Text-before-tool_use and
  thinking-before-text within a single assistant entry matter for
  replay semantics.

## Starting a fresh session file

Minimum ingredients to make `claude --resume <uuid>` pick up a new
session:

1. Directory `~/.claude/projects/<sanitized-cwd>/` exists.
2. File `<uuid>.jsonl` exists inside it.
3. First line is a valid `permission-mode` entry for that session
   (three fields only).
4. Subsequent lines form a valid conversation — at minimum, a single
   `user` entry with the envelope fields from the strong-conventions
   table and a string or array `message.content`.

That's enough for Claude Code to see the session. A functioning
"continuable" session needs at least one assistant turn with a clean
`stop_reason` for resume to know where to pick up.

## What we haven't verified

- Exact tolerance for truncated or missing `usage` objects on
  synthetic assistant entries.
- Whether `toolUseResult` top-level summaries are ever consulted by
  the loader, or are purely for UI/debugging.
- How strict the loader is about `messageId` uniqueness across
  `file-history-snapshot` entries (given known collision bugs exist
  in Anthropic's own output).

When in doubt: copy a real entry's envelope shape field-for-field.

## Reference implementation

`path incept` is the in-tree tool that produces Claude-compatible JSONL
from a Toolpath document. Its implementation is the working example of
every rule in this doc:

- CLI entry point: [`crates/path-cli/src/cmd_incept.rs`](../../../../crates/path-cli/src/cmd_incept.rs)
- Projection logic: `ClaudeProjector` in
  [`crates/toolpath-claude/src/project.rs`](../../../../crates/toolpath-claude/src/project.rs)
  (implements `toolpath_convo::ConversationProjector`)

When you add a new rule here, update `cmd_incept.rs` or `project.rs`
in the same change; when you discover a rule empirically by fixing a
bug there, capture it here.
