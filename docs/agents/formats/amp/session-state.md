# Thread identity and session state

Amp calls a session a **thread**. Threads are server-authoritative; the local
machine keeps only pointers and preferences.

All `[observed, 0.0.1785170481-ga5b614]` unless tagged.

## Identifiers

| Kind | Shape | Example | Where |
| --- | --- | --- | --- |
| Thread | `T-<uuidv7-ish>` | `T-019fa4db-29cf-70c9-8d9b-81524df70e52` | `.id`, `session_id` in the stream, thread log filename |
| Message (protocol) | `M-<base62>` | `M-033wt7sbSDccKHLOPDTqjB` | `.messages[].protocolMessageID` |
| Message (ordinal) | 1-based integer | `1`, `2`, `24` | `.messages[].messageId` |
| Tool call (Amp) | `TU-<base62>` | `TU-033wt8676K5StumvhRV8kd` | `tool_use.id`, `tool_result.toolUseID` |
| Tool call (provider) | `call_<b64ish>` | `call_kMXdmho5JPlQ3QL3GWSIkeRD` | `tool_use.providerToolUseId` |
| Executor | `amp-x-<uuid>` / `neo-<uuid>` | `amp-x-bb46b03a-…` | thread log only |
| Installation | UUID | — | `device-id.json`, `env.initial.platform.installationID` |

The thread id prefix is `T-` followed by what looks like a UUIDv7 (the leading
`019fa4…` is a millisecond timestamp), so **thread ids sort chronologically**.
`[inferred]`

Use `protocolMessageID` as `Turn.id`. The integer `messageId` is a positional
ordinal that would collide across threads and is not stable if the thread is
ever edited.

## `v` — the revision counter

The export's top-level `v` counts mutations applied to the thread, not schema
version:

| Thread | Messages | `v` |
| --- | --- | --- |
| trivial | 4 | 20 |
| install-session | 6 | 41 |
| feature-elicit | 24 | 73 |

It tracks mutations, not messages — the ratio to message count is not constant
(5.0, 6.8, 3.0 for the three threads above), because a short thread still pays
for thread-level setup while a long one amortizes it. Never branch parsing on
`v`.

## Per-thread version anchoring

`env.initial.platform.clientVersion` records the Amp build that **created**
the thread. The capture machine held threads from two different builds
(`0.0.1785164324-gd1fcef` and `0.0.1785170481-ga5b614`) minutes apart, because
Amp self-updated between them.

When interpreting an export, trust `clientVersion` over `amp --version` — the
running binary may be newer than the thread.

## Message counting is not what `threads list` shows

`amp threads list` has a `Messages` column. For the feature-elicit thread it
reads **1**, while the export contains **24** messages. It counts **human user
messages**, not protocol messages. (The trivial thread, prompted twice, shows
`2`.)

Anything surfacing an Amp session count — `p list amp --format tsv`,
`SessionMetadata.line_count` — must decide which it means and say so. The
export's `.messages | length` is the honest protocol count.

## `session.json` — local app state

`~/.local/share/amp/session.json`. **Not** conversation state:

```jsonc
{
  "agentMode": "medium",
  "pluginAgentModeKey": null,
  "launchCount": 0,
  "neoInvadersHighScore": 0,
  "shortcutsHintUsed": false,
  "neoWelcomeDismissed": true,
  "threadListSidebarVisible": false,
  "threadStatusVisorHidden": false,
  "lastThreadId": "T-019fa447-…",
  "lastExecuteThreadId": "T-019fa4db-…",
  "lastThreadByTerminal": {
    "wezterm:12": { "updatedAt": 1785177320102, "lastExecuteThreadId": "T-019fa4db-…" },
    "wezterm:6":  { "updatedAt": 1785167595629, "lastThreadId": "T-019fa447-…" }
  }
}
```

Useful bits:

- **`lastThreadId` / `lastExecuteThreadId`** are tracked separately — interactive
  and execute-mode threads do not share a "last" pointer. `amp last` continues
  the last thread `[official]`.
- **`lastThreadByTerminal`** keys on `<terminal-program>:<window-or-pane-id>`,
  so "continue where I left off" is per terminal pane.
- The rest is UI preference state (including a built-in game's high score).

This file is the only place a *local* consumer can find "the most recent
thread" without a network call — a useful cheap default for a picker, but it
names at most two threads.

## `history.jsonl`

`~/.local/share/amp/history.jsonl`, one object per line:

```json
{"text":"hello","cwd":"/Users/example"}
```

Prompt text and the directory it was typed in. No thread id, no timestamp, no
response. It is a shell-style recall buffer, not a session index — it cannot
be joined back to threads.

## Thread lifecycle facts worth knowing

- **`amp -x` on a new thread archives it when the command finishes**, unless
  `--no-archive-after-execute` is passed `[official]`. Archived threads are
  hidden from `amp threads list` without `--include-archived`. Every capture
  in this recon used the flag.
- **Titles are server-generated** from the conversation ("Filesystem tool
  exercise", "Reply with ok", "General assistance"); there is no client-side
  title. `amp threads rename` changes it `[official]`.
- **Default visibility is `private`** and is settable per repository via
  `amp threads visibility` `[official]`.
- `activatedSkills` on the thread records which skills were loaded — an
  Amp-side concept worth preserving as a `ConversationEvent` rather than
  dropping.
