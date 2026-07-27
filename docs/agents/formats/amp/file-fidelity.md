# File-change fidelity

**Amp's file fidelity is Codex-grade.** `apply_patch` results embed a real
unified diff per file, inline, in the thread document — so a derived
`Path` gets a genuine `raw` perspective without reconstructing anything from
tool arguments or snapshots.

All `[observed, 0.0.1785170481-ga5b614]`.

## There is no dedicated edit tool

Amp's 29-tool inventory contains **no** `read_file` / `edit_file` /
`write_file`. Every file write is `apply_patch`; every read, listing, and
search is `shell_command`. This is the Codex model, not the Claude Code model.

Consequence for `tool_category`: `FileRead` and `FileSearch` are effectively
**unreachable** for native Amp tools — a `cat` and a `grep` both classify as
`Shell`, because that is genuinely what they are. Do not pattern-match shell
command strings to fake finer categories; that would invent structure the
source does not have.

## The request side: `apply_patch`

A single `patchText` string in Codex's patch envelope.

Creating a file:

```
*** Begin Patch
*** Add File: notes.md
+scratch — feature elicitation
+this file will be edited and searched.
*** End Patch
```

Editing a file:

```
*** Begin Patch
*** Update File: notes.md
@@
-scratch — feature elicitation
+fixture — feature elicitation
 this file will be edited and searched.
*** End Patch
```

Observed action verbs: `*** Add File:` and `*** Update File:`. A delete verb
almost certainly exists but was not exercised — `[unverified]`.

Paths in `patchText` are **relative to the session's working directory**.

## The result side: a real diff, already computed

This is what makes Amp good. The `apply_patch` tool result carries a
per-file record with a complete unified diff:

```jsonc
{ "run": { "status": "done", "progress": {}, "result": {
  "files": [ {
    "uri": "file:///tmp/amp-elicit/notes.md",
    "type": "update",
    "additions": 1,
    "deletions": 1,
    "diff": "Index: /tmp/amp-elicit/notes.md\n===================================================================\n--- /tmp/amp-elicit/notes.md\n+++ /tmp/amp-elicit/notes.md\n@@ -1,2 +1,2 @@\n-scratch — feature elicitation\n+fixture — feature elicitation\n this file will be edited and searched.\n"
  } ],
  "summary": "update: /tmp/amp-elicit/notes.md (+1/-1)"
} } }
```

| Field | Maps to | Notes |
| --- | --- | --- |
| `uri` | `FileMutation.path` | `file://` URI with an **absolute** path — strip the scheme; relativize against `base.working_dir`. |
| `type` | `FileMutation.operation` | `"add"` / `"update"` observed; toolpath's vocabulary already uses these words. |
| `diff` | `FileMutation.raw_diff` | Subversion-style header (`Index:` + `===`) followed by standard `---`/`+++`/`@@` hunks. |
| `additions` / `deletions` | structural counts | Consistent with the diff in every observed case. |
| `summary` | — | Human string; redundant with the fields above. |

`files` is an **array**, so one `apply_patch` call can mutate several files;
`FileMutation.tool_id` should be the originating `TU-…` id for each.

`FileMutation.before` / `.after` are **not** derivable — Amp ships the diff,
not the file contents. For an `Add File` the diff's `+` lines *are* the full
content, so `after` could be reconstructed for adds specifically; for updates
it cannot.

### The diff header carries absolute host paths

Note the sanitization consequence, already handled in the fixtures: the
`Index:`/`---`/`+++` lines embed the capturing machine's absolute paths. Any
fixture must rewrite them consistently with `uri` or the diff and the path
disagree.

## How errors are actually signalled

**Not** by `run.status`, which was `"done"` for all 11 tool results in the
feature-elicit capture — including step 7's deliberate failure. `status` is a
lifecycle state.

The real signal is inside the payload, and it is tool-specific:

| Tool | Error signal |
| --- | --- |
| `shell_command` | `run.result.exitCode != 0` |
| `apply_patch` | `[unverified]` — no failing patch was captured |
| `Task`, `skill` | `[unverified]` |

The captured failure:

```jsonc
{ "run": { "status": "done", "result": {
    "output": "cat: does-not-exist.txt: No such file or directory\n",
    "exitCode": 1 } },
  "type": "tool_result", "toolUseID": "TU-033wt8ohXapDIiGHiONt0i" }
```

The `--stream-json` view of the same result sets **`is_error: false`** — so
the stream's explicit error flag is *not* trustworthy either, at least for
shell exit codes. Derive `ToolResult.is_error` from `exitCode`, and record
that anything non-shell is currently a guess.

## Contrast with the other providers

| Provider | Where a diff comes from |
| --- | --- |
| **Amp** | Inline in the `apply_patch` tool result (`files[].diff`) |
| Codex | Inline in `patch_apply_end` |
| Copilot CLI | Inline in `result.detailedContent` for native `edit`/`create` |
| opencode | Reconstructed from a sibling bare-git snapshot repo |
| Claude Code / Gemini | Reconstructed from tool inputs (`old_string`/`new_string`) |

Amp sits with Codex and Copilot in the top tier: no reconstruction, no
snapshot repo, no gitignore blind spots.

## What is *not* captured

- **Files changed outside `apply_patch`.** A `shell_command` running
  `sed -i` or `>` mutates the workspace with no structured record. Only the
  command string survives. This is the same blind spot Codex and Claude Code
  have with shell writes.
- **Working-tree state.** No git branch, commit, or remote is recorded
  anywhere in the thread (see [events.md](events.md#envinitial--the-environment-stamp)),
  so a derived `Path.base` has `vcs_revision`/`vcs_branch`/`vcs_remote` all
  `None` and diffs cannot be anchored to a commit.
