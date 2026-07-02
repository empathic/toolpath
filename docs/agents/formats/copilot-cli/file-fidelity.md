# File-change fidelity

## Forward (derivation): Codex-grade ✅ `[observed, 1.0.67–1.0.68]`

**Resolved** — the original "biggest open question" (do edits embed diffs, or
must they be reconstructed from snapshots?) has the happy answer: Copilot's
`edit`/`create` **`tool.execution_complete` embeds the real file-state diff
inline**, as a git-style unified diff in `result.detailedContent` (plus a
human summary in `result.content` and `toolTelemetry.metrics.linesAdded/`
`linesRemoved`). This is the same fidelity class as Codex's `patch_apply_end`
(see [`../codex.md`](../codex.md)) — no `checkpoints/` reconstruction needed
for derivation.

`toolpath-copilot`'s forward path uses both sources, best-first:

1. `tool.execution_start` args (`{path, old_str, new_str}` / `{path,
   file_text}`) yield the `FileMutation` (structural perspective + an
   arg-derived diff), attributed via `tool_id = toolCallId`.
2. The matching complete's `result.detailedContent` (when it contains a hunk)
   **upgrades the mutation's `raw_diff`** to the native file-state diff — this
   is authoritative (the tool diffs the actual file, so e.g. replacing an
   "empty" file shows `-`/`+` where the args alone show only `+`).

The `checkpoints/` and `rewind-snapshots/` directories still exist per session,
but they serve the rewind feature, not derivation; their format remains
`[unverified]` (tracked in
[known-gaps-and-sourcing.md](known-gaps-and-sourcing.md)).

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

The diff must be a **well-formed unified diff with exactly one file header** —
a stray/empty second `--- `/`+++ ` pair (which `toolpath_convo::unified_diff`
emits, since it prepends its own `a/<path>` header on top of `similar`'s
empty-filename one) makes Copilot fall back to flat, uncolorized text. The
projector builds the diff with `similar` directly and sets the header once.

The diff must also contain **at least one `@@` hunk** `[observed via pty
capture, 1.0.68]`: Copilot's diff view renders parsed hunk rows and hides
header lines, but a headers-only diff (e.g. creating an *empty* file — a
`""→""` text diff has no hunk) makes it dump `diff --git`/`index`/mode lines as
raw text. The native tool renders an empty-file create as one added empty line
(`@@ -1,0 +1,1 @@` + `+`); the projector does the same, and omits
`detailedContent` entirely for any other hunkless case.

**The `assistant.message.toolRequests` mirror is what the timeline UI renders
from** `[observed via pty capture + bundle, 1.0.68]` — not `tool.execution_start`.
The editor-family row (title `Edit <path>`, `+N −M` counts, the colorized diff
body) only engages when the mirror's `arguments.path` is present; a mirror still
carrying foreign arg names (Claude's `file_path`/`old_string`) drops the call
into the *generic* row, which markdown-renders the diff as flat text — this was
the "no colorized diff" symptom. The projector therefore computes one
`(name, arguments)` remap per tool call (`projected_tool`) and uses it in
**both** the mirror and the execution events: file writes → `edit`/`create`
(`path`/`old_str`/`new_str`/`file_text`), file reads → `view` (`path`, with
Claude's `offset`/`limit` mapped to `view_range`).

Note on the rendered `+N −M` counts: they're recomputed by the TUI from the
diff (`q7`/`ske` in the bundle — header-prefix + hunk-regex heuristics), not
read from `toolTelemetry`. A projected edit can legitimately show `+1` where
the native session showed `+1 −1`: the native tool diffs actual file state
(an "empty" file still has one line), while the projector can only diff the
tool args (`old_str: ""` → nothing removed).
