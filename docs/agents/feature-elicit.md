# Feature elicitation — capture a real-world fixture per harness

A single harness-agnostic prompt that walks any agent through every common
tool category in roughly five minutes. The resulting session file (claude
JSONL / codex rollout / gemini chat-file / pi JSONL / opencode SQLite
rows) is a real-world fixture for the cross-harness matrix corpus —
denser and more representative than the inline-built
`ConversationView` the matrix tests today.

## Why this exists

The cross-harness matrix in `crates/path-cli/tests/cross_harness_matrix.rs`
runs against a synthetic IR fixture with three tool calls and one error.
That's enough to catch projector-side bugs but doesn't exercise the
breadth of features each harness can produce in the wild — long content,
many tool calls, MCP tools, retry semantics, model-driven thinking, etc.
Real-world fixtures come from real-world sessions.

This file is the *deterministic trigger* that produces such sessions on
demand. Run it once per harness on a fresh scratch directory; commit the
resulting session file as a fixture; re-run after upstream releases when
you want to refresh the corpus.

## Two ways to run it

### Automated (recommended)

```bash
./scripts/capture-elicit-fixtures.sh                  # all installed harnesses
./scripts/capture-elicit-fixtures.sh claude codex     # subset
```

The script:
- Creates a per-harness scratch dir under `$TMPDIR/toolpath-elicit.*/`.
- Invokes each harness's non-interactive prompt mode with the contents of
  [`feature-elicit.prompt.txt`](./feature-elicit.prompt.txt).
- Snapshot-diffs the harness's session storage to find the new session file.
- Copies it into `test-fixtures/<harness>/`.
- Skips harnesses whose CLI isn't on `$PATH` and reports them.

Per-harness invocation (edit `scripts/capture-elicit-fixtures.sh` if your
version uses different flags):

| Harness | Invocation | Output landing |
|---|---|---|
| Claude | `claude -p "<prompt>"` | `~/.claude/projects/<sanitized>/<uuid>.jsonl` |
| Codex | `codex exec "<prompt>"` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` |
| Gemini | `gemini -p "<prompt>"` | `~/.gemini/tmp/<slot>/chats/session-*.json` |
| Pi | `pi -p "<prompt>"` (varies; manual fallback documented below) | `~/.pi/agent/sessions/--<encoded-cwd>--/*.jsonl` |
| Opencode | `opencode run "<prompt>"` then `path p export opencode` | rows in `opencode.db`, exported to JSON |
| OpenClaw | `scripts/openclaw-docker.sh up` then `agent "$(cat docs/agents/feature-elicit.prompt.txt)"` | `~/.openclaw-docker/agents/main/sessions/<uuid>.jsonl` |

Cursor is **not** in the automated list. The IDE has no scriptable
chat-panel entry point, and the `cursor-agent` CLI writes its real
session to a separate protobuf blob store (`~/.cursor/chats/...`)
that `toolpath-cursor` doesn't parse. Capture Cursor fixtures
manually — see below.

### Manual

If a harness's batch mode doesn't work (Pi varies; opencode without
`run`) or you want to drive it interactively to see the UX:

```bash
mkdir -p /tmp/toolpath-elicit && cd /tmp/toolpath-elicit
rm -rf ./*
# launch your harness, then paste the contents of feature-elicit.prompt.txt
# as the first user message.
```

When the run completes, copy the session file out of its harness-native
location (table above) into `test-fixtures/<harness>/`.

### Cursor

1. Open Cursor.app on a scratch workspace, open a new agent-mode chat,
   paste the prompt, wait for the run to finish.
2. Find the new composer UUID:

   ```bash
   sqlite3 "file:$HOME/Library/Application Support/Cursor/User/globalStorage/state.vscdb?mode=ro" \
     "SELECT substr(key, length('composerData:')+1) FROM cursorDiskKV WHERE key LIKE 'composerData:%' ORDER BY rowid DESC LIMIT 5"
   ```
3. Dump it into the fixture file:

   ```bash
   cargo run -p toolpath-cursor --example dump_fixture -- \
     --composer <uuid> --no-trim --output test-fixtures/cursor/convo.json
   ```

`dump_fixture` also has a `--from-jsonl <path>` mode that synthesizes
a `CursorSession` from a `~/.cursor/projects/.../<chat>.jsonl`
transcript — useful as a fallback when you only have the message log
and not the real `state.vscdb` row, but the JSONL is lossy (no tool
results, `[REDACTED]` text) so the fixture is best-effort, not
state.vscdb-equivalent.

## What the prompt covers

Ten tasks, no harness-specific tool names, no network access required.
Every common category gets touched: shell exec (1, 8), file write (2, 8),
file read (3), file edit (4), file search by name (5), file search by
content (6), errored read (7), sub-agent dispatch / delegation (9, where
the harness supports it), reflection / thinking (throughout), final
summary (10). The full prompt body lives in
[`feature-elicit.prompt.txt`](./feature-elicit.prompt.txt) so the doc
and the script stay in sync — edit one place.

## Completeness checklist

After the run, before committing the session file as a fixture, confirm
the harness's session contains at least:

- [ ] **2+ user turns** (the task list, possibly a clarification)
- [ ] **6+ assistant turns**
- [ ] **1+ shell tool call** (`bash` / `exec_command` / `run_shell_command`)
- [ ] **1+ file write** (`write` / `write_file` / `apply_patch`)
- [ ] **1+ file edit** (`edit` / `replace` / `apply_patch`)
- [ ] **1+ file read** (`read` / `read_file`)
- [ ] **1+ file search by name** (`glob` / `list_directory`)
- [ ] **1+ file search by content** (`grep` / `search`)
- [ ] **1+ errored tool result** (the missing file in step 7)
- [ ] **1+ delegation event** (sub-agent dispatch in step 9, where the harness supports it; note as a known gap if not)
- [ ] **1+ thinking / reasoning block** (if the harness supports it)
- [ ] Final assistant message with the summary

If any are missing, the harness either skipped a step (rerun with an
explicit prod) or doesn't support that tool category (note it as a
known gap).

## Wiring the fixture into the matrix

Once the file lands at
`test-fixtures/<harness>/convo.{jsonl,json}` (workspace root),
each harness's existing reader API (`ConversationReader::read_conversation`,
`RolloutReader::read_session`, `ConvoIO::read_session`, …) consumes it
directly. Plug into the `Harness::load_fixture()` slot in
`crates/path-cli/tests/cross_harness_matrix.rs` and the matrix runs
against real-world input instead of the synthetic IR.

Cursor is the exception: there's no per-session file on disk, so its
fixture format is the serialized `toolpath_cursor::CursorSession`
struct (composerData + ordered bubbles + referenced content blobs).
The `CursorHarness::load_fixture` slot deserializes the JSON straight
back into `CursorSession` and runs `session_to_view` on it.

## Maintenance

Re-run when:
- An upstream harness ships a minor release that changes its tool set or
  schema. Diff the new fixture against the old one to see what shifted.
- The matrix grows a new invariant that needs richer input than the
  synthetic fixture provides.
- A bug we found via live testing didn't surface in the matrix; capture
  a fixture that exercises it and add to the corpus.

Don't keep stale fixtures around indefinitely — corpus-level baselines
should track upstream, not the snapshot from the day the test was
written.
