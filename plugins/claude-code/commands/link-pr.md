---
description: Share the session and link it in a PR description — use when the user asks to share or attach this conversation to a PR
argument-hint: "[pr number or url]"
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh:*), Bash(gh pr view:*), Bash(gh pr edit:*)
---

## Context

- Toolpath CLI: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh"`
- Auth: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" exec auth status`
- Current session id: !`"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" current-session`

## Your task

Share an agent session to Pathbase, then add the resulting link to a GitHub PR description.

User arguments: $ARGUMENTS

Always invoke the CLI through the wrapper, with literal absolute paths (never `$PWD` or other variables — they fail the permission check):

```
"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" exec <path-cli arguments...>
```

### Target PR

- A PR number or URL in the arguments wins.
- Otherwise the current branch's PR: `gh pr view --json number,url,body`.
- If the conversation just opened or discussed a specific PR, that's the one the user means.
- No PR found → ask which PR.

### Share

Same rules as `/path:share`:

- Share the current conversation — the "Current session id" from the context above (fall back to the newest row of `"${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh" sessions` if it reads `unknown`).
- If the Auth context shows no login and the user didn't pass `--anon`, stop and ask: anonymous upload, or `path auth login` in their own terminal first (never run it yourself)?
- Compose a title and description for the upload, same as `/path:share`: a 2–4 word title naming the work and one or two sentences summarizing what the session did — for this command that's usually the work behind the PR. User-supplied `--title`/`--description` win.
- Run, passing through any of `--anon`, `--public`, `--repo`, `--name`, `--url` from the arguments:

  ```
  ... exec share --harness claude --project <absolute cwd> --session <session-id> --title <title> --description <description>
  ```

Note the Pathbase URL it prints.

### Link it in the PR

1. Fetch the current body: `gh pr view <n> --json body -q .body`.
2. If the body already contains this Pathbase URL, don't add it again — report that it's already linked and stop.
3. Otherwise append (using the Write tool for a temp file, then `gh pr edit <n> --body-file <file>` — don't try to inline a multi-line body in shell):

   ```

   ---

   Agent session: [<the title you composed for the share>](<pathbase-url>)
   ```

   If an `Agent session:` line already exists for a different session, add a new line under it rather than replacing it.

### Report

Give the user both links: the PR and the Pathbase session. On share failure, apply `/path:share`'s guidance (auth, `--anon`, server); on `gh` failure, show the error — likely not logged in (`gh auth login`) or no PR for the branch.
