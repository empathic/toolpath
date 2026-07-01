# Per-session layout (`session-state/`)

Each session lives in its own subdirectory of `~/.copilot/session-state/`, keyed
by session ID `[official]`
([config-dir reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)):

```
~/.copilot/
  session-state/
    <session-id>/
      events.jsonl       # the append-only event stream (source of truth)
      workspace.yaml     # session metadata: git root, repo, branch  [reverse-eng]
      checkpoints/       # titled snapshots for rewind                [reverse-eng]
      …                  # "plans" and "tracked files" workspace artifacts [official, names unconfirmed]
```

> **Naming note.** One paraphrased source rendered the directory as
> `sessions/`. Every other source — official docs, issues
> [#2012](https://github.com/github/copilot-cli/issues/2012)/[#3551](https://github.com/github/copilot-cli/issues/3551),
> the jonmagic write-up — says **`session-state/`**. Treat `sessions/` as a
> transcription error. `[reverse-eng, High agreement]`

## `events.jsonl`

The primary log: a line-delimited JSON stream of everything that happened in the
session. `[official]` calls it the session "event log (`events.jsonl`)"; the
parsing bug in issue #2012 (raw `U+2028`/`U+2029` breaking `JSON.parse()` on
`/resume`) confirms it is parsed **one `JSON.parse()` per line** — i.e. genuine
JSONL, not a single JSON array. Full event catalogue in [events.md](events.md).

This is the file a forward provider reads to reconstruct the conversation.

## `workspace.yaml`

`[reverse-eng, Medium]` (jonmagic write-up) — session metadata in YAML. Reported
fields:

| Field | Meaning |
|---|---|
| `git_root` | Absolute path to the repository root. |
| `repository` | Repository identifier (owner/name or remote). |
| `branch` | Active branch at session time. |

For a derivation this is the cleanest source of the `path.base` URI and git
context (analogous to Codex's `session_meta.cwd` + `git`). It being YAML (not
JSON) is the one format surprise here — a provider needs a YAML parser for it,
or can fall back to the `session.start` event's `cwd`/`model` (see
[events.md](events.md)) and the `sessions` row in
[`session-store.db`](session-store-db.md) for repo/branch.

## `checkpoints/`

`[reverse-eng, Medium]` — a directory of titled snapshots that power the rewind
feature (`/session checkpoints`, `/session` rewind). Each checkpoint reportedly
carries a title, an overview, and "next steps." The relationship between a
checkpoint and a point in `events.jsonl` is **unverified** — see
[file-fidelity.md](file-fidelity.md), since checkpoints appear to be where file
*content* state lives (as opposed to the event stream, which carries tool-call
args).

## Session keying and naming

- The subdirectory name is the **session ID**, reported as a UUID
  `[reverse-eng, Medium]`. Official docs consistently say "session ID" without
  fixing the format `[official]`.
- A session also has a human-readable **name** (`--name`, or auto-generated)
  used for resume-by-name and resume-by-prefix `[official]`. The mapping from
  name → ID is held in [`session-store.db`](session-store-db.md), not in the
  directory name.

A discovery routine therefore has two indices to choose from: walk
`session-state/*/` directly (source of truth, but you parse each `events.jsonl`
or `workspace.yaml` to get metadata), or read the `sessions` table in
`session-store.db` for id/name/repo/branch/summary cheaply. See
[resume-and-sessions.md](resume-and-sessions.md) for how the CLI resolves an
identifier across these.
