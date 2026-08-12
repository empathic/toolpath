---
description: Resume a shared agent session in Claude Code
argument-hint: "pathbase-url"
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh:*)
---

## Context

- Toolpath CLI: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh"`

## Your task

Bring a shared agent session into this project so the user can resume it in Claude Code. You cannot switch the running session yourself — the deliverable is the projected session plus the exact resume step.

User arguments: $ARGUMENTS

The input is a Pathbase URL (`https://host/owner/repo/slug`), an `owner/repo/slug` shorthand, a local toolpath JSON file, or a cache id. If no input was given, ask for one.

Always invoke the CLI through the wrapper, and write paths as literal absolute strings — never `$PWD` or other variables (they fail the permission check):

```
"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" exec <path-cli arguments...>
```

### Steps

1. **Fetch** (Pathbase URL or shorthand only — skip for a cache id or local file):

   ```
   ... exec p import pathbase <input> --force
   ```

   Note the cache id from the output.

2. **Project** the document into this project:

   ```
   ... exec p export claude --input <cache-id-or-file-path> --project <absolute cwd>
   ```

   - Success: the output ends with the resume recipe and the full session id.
   - Error saying the session **already exists in this project**: that's not a failure — the session is already local (and may be newer than the shared copy). Take the session id from the error message and go to step 3. Never retry with `--force` unless the user explicitly asks to overwrite their local session.

3. **Hand off.** Tell the user both options, with the real session id filled in:
   - `/resume <session-id>` — right here, no restart (the built-in resume takes an id and re-scans this project's sessions).
   - `claude -r <session-id>` — from a terminal in this directory.

### Notes

- The document must be a single agent session (what `path share` produces). If the export reports it isn't, say so — graphs and multi-path documents can't be resumed.
- Sessions shared from other harnesses (Codex, Gemini, ...) project into Claude Code fine — tool calls are remapped.
- Reasoning blocks from the original session are not replayed to the model after resume (they lack API signatures); the conversation itself is intact.
