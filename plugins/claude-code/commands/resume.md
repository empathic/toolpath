---
description: Resume a shared agent session in Claude Code, or send this session to an ssh host
argument-hint: "pathbase-url | --remote <user@host> [-C <remote-dir>] [-- <claude args>]"
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh:*)
---

## Context

- Toolpath CLI: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh"`
- Current session id: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" current-session`

## Your task

Bring a shared agent session into this project so the user can resume it in Claude Code. You cannot switch the running session yourself — the deliverable is the projected session plus the exact resume step. With `--remote <user@host>` the direction reverses: send a session to an ssh host and run it there; see "Remote" below.

User arguments: $ARGUMENTS

The input is a Pathbase URL (`https://host/owner/repo/slug`), an `owner/repo/slug` shorthand, a local toolpath JSON file, or a cache id. If no input was given and `--remote` is absent, ask for one.

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

### Remote (`--remote <user@host>`)

Send a session to an ssh host and run it there under tmux. The remote needs `claude` and `tmux` on it, and the project directory. The deliverable is the attach command.

This mode needs a `path` built with the `resume-remote` cargo feature, version 0.25.0 or later. Check first:

```
... exec resume --help
```

If the output does not list `--no-attach`, stop and tell the user: the installed `path` lacks `--no-attach` (path-cli 0.25.0 or later, built with the `resume-remote` feature); build one with `cargo install path-cli --features resume-remote` or point `TOOLPATH_BIN` at a build that has it.

The destination must mean the same host, port, and user to `ssh` as to `path`: the command connects with its own ssh client and reads no `~/.ssh/config`, while the attach line it prints goes through `ssh`. An alias, a `ProxyJump`, or a `User` or `Port` override in `~/.ssh/config` for that host launches the session in one place and attaches somewhere else.

1. **Import** the session to send. With no other input, that is the current conversation — the "Current session id" from the context above. If it reads `unknown`, take the newest row of `"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" sessions` (TSV, newest first: project, session id, timestamp, step count, first user message) as the current conversation.

   ```
   ... exec p import claude --project <absolute cwd> --session <session id> --force
   ```

   The cache id is `claude-<session id>`. If the user gave a Pathbase URL, file, or cache id, skip this step and use that input in step 2 instead.

2. **Resume remotely**, without attaching:

   ```
   ... exec resume claude-<session id> --remote <user@host> --no-attach
   ```

   The remote project directory defaults to this cwd with the local home swapped for the remote home; pass through `-C <remote-dir>` from the user's arguments to override it. Anything after `--` in the user's arguments goes after `--` on this command and reaches the remote `claude` (for example `-- --permission-mode acceptEdits`, which lets the remote session work without a person answering its permission prompts). For a document whose source is not Claude Code, add `--harness claude`. The command prints its plan on stderr, ships the session when the remote lacks it, launches `claude -r` in a detached tmux session, and prints the attach command. A session that already exists on the remote is launched as is; a live tmux session is left running, and the plan says so.

3. **Hand off.** The last stdout line is `ssh -t ssh://<user@host> tmux ...`. Give the user that line, in a code block, as the command to run in a terminal. You cannot attach from here. Tell the user that the remote conversation ends with this `/path:resume` prompt, so the remote Claude needs its next instruction stated explicitly.

The uploaded session ends with this `/path:resume` prompt; nothing after the import is uploaded.

### Notes

- The document must be a single agent session (what `path share` produces). If the export reports it isn't, say so — graphs and multi-path documents can't be resumed.
- Sessions shared from other harnesses (Codex, Gemini, ...) project into Claude Code fine — tool calls are remapped.
- Reasoning blocks from the original session are not replayed to the model after resume (they lack API signatures); the conversation itself is intact.
