# File-change fidelity

**This is the single most important open question for a `toolpath-copilot`
derivation**, because it determines whether the derived `Path` can carry a real
`raw` (unified-diff) perspective on file artifacts or only a structural
(args-level) one.

## What we believe happens

`[reverse-eng, Medium]` + `[inferred]`:

- File edits are issued as **tool invocations** and recorded in `events.jsonl`
  as `tool.execution_start` / `tool.execution_complete`, carrying the tool
  **name**, **args**, and **success** (issue
  [#3551](https://github.com/github/copilot-cli/issues/3551)).
- File *state* is tracked separately, via the **`checkpoints/`** snapshot
  directory plus the **`session_files`** table in
  [`session-store.db`](session-store-db.md) ("every file touched").
- The rewind/checkpoint UI reportedly shows diff stats (`+added −removed`),
  implying diffs are **computed from snapshots**, not stored inline in the event.

## The contrast with Codex (why this matters)

`toolpath-codex` gets excellent file fidelity for free because Codex's
`patch_apply_end` event **embeds the change itself** — a unified diff for
updates, or the full file content for adds (see
[`../codex.md`](../codex.md) and `PatchApplyEnd`/`PatchChange` in
`toolpath-codex`). The derivation reads the diff straight out of the event.

We found **no evidence** that Copilot's `tool.execution_complete` embeds a
literal unified diff or full post-edit file content `[unverified]`. The apparent
model is instead:

```
tool.execution_complete   →  WHAT changed (tool name + args: path, new content/edit)
checkpoints/ + session_files  →  the file STATE, from which diffs are COMPUTED
```

So Copilot looks closer to **opencode** than to Codex: opencode also lacks
inline diffs in its message stream and recovers them by diffing git snapshots
between turns (`FileMutation` with `tool_id: None`, attributed to the turn as a
whole). See the `FileMutation` doc comment in `toolpath-convo` — it explicitly
contemplates both the call-id-attributed case (Codex/Claude) and the
snapshot-diff case (opencode).

## Working assumption for a derivation

Until a real session proves otherwise, treat Copilot file fidelity as
**snapshot-derived, not event-embedded**:

1. From `tool.execution_*` args, build `ToolInvocation`s with
   `ToolCategory::FileWrite` and the path/edit in `input`. This always yields a
   **structural** perspective on the change.
2. For a **`raw` perspective**, reconstruct before/after content by walking the
   `checkpoints/` snapshots (and/or `session_files`) and diffing adjacent states,
   then emit `FileMutation { path, raw_diff, before, after, tool_id: None }`.
   This is the opencode-style path and is **the harder, deferred half** of the
   crate.
3. If a captured session turns out to embed diffs/content inline after all
   (the happy Codex-style case), prefer that — attribute the `FileMutation` to
   the tool call via `tool_id` and skip the snapshot reconstruction.

A first-cut provider can ship with **structural-only** file changes (step 1) and
note the missing `raw` perspective as a known limitation, exactly the posture
`toolpath-opencode` takes for gitignored paths. The `checkpoints/` snapshot
format is itself `[unverified]` and must be reverse-engineered from a real
`~/.copilot/session-state/<id>/checkpoints/` before step 2 is implementable.

## Open questions to resolve against a sample

- Does `tool.execution_complete` (or any event) carry the file's new content or
  a diff inline? If yes, fidelity is Codex-grade and step 2 is unnecessary.
- What is the on-disk format of a `checkpoints/` entry — full file copies, a
  git-style object store, a patch series?
- Is there a stable mapping from a checkpoint back to the `events.jsonl`
  position (so a diff can be attributed to the turn/tool that caused it)?

These are the first things to check when `~/.copilot/session-state/` is
available — they're repeated in the
[verification checklist](known-gaps-and-sourcing.md#verify-once-we-have-samples).

## Reverse (projection): making an edit render `[observed, 1.0.67]`

For `path resume`/`path p export copilot` the *rendered diff* comes from the
tool call's **`result.detailedContent`** — a git-style unified diff. Copilot's
real file tools are:

- **`edit`** — `arguments {path, old_str, new_str}`; `result.content` a summary
  (`File <path> updated with changes.`) and `result.detailedContent` a
  `diff --git a/<path> b/<path>\nindex …\n--- a/<path>\n+++ b/<path>\n@@ …`.
- **`create`** — `arguments {path, file_text}`; the diff uses
  `create file mode 100644` and `--- a/dev/null`.

`CopilotProjector` detects `ToolCategory::FileWrite` tool calls and re-emits
them in this shape (mapping a Claude `Edit`/`Write`'s `old_string`/`new_string`/
`content` into `old_str`/`new_str`/`file_text` and synthesizing the git diff),
so the change renders in the resumed session instead of showing as a bare tool
call. Paths in the diff drop the leading `/` (git convention).

The complete event also needs a **`toolTelemetry`** block for Copilot to render
a *colorized* diff (without it the diff shows as flat text). Its `properties`
values are *stringified* JSON; the one that matters is
`codeBlocks` — `[{"fileExt": ".rs", "languageId": "rust", "linesAdded": N,
"linesRemoved": M}]` — which declares the diff's language for highlighting.
`metrics.linesAdded`/`linesRemoved` supply the `+N −M` summary. The projector
derives `languageId` from the path extension and the line counts from the diff.
