# Per-session layout (`session-state/`)

Each session lives in its own subdirectory of `~/.copilot/session-state/`, keyed
by session ID `[official]`
([config-dir reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)):

`[observed, 1.0.67]` — one session directory contained:

```
~/.copilot/
  session-state/
    <session-id>/          # <session-id> is a UUID
      events.jsonl         # the append-only event stream (source of truth)
      workspace.yaml       # session metadata: cwd, git root, repo, branch, name, timestamps
      session.db           # per-session SQLite (distinct from the global session-store.db)
      checkpoints/         # snapshot history
      rewind-snapshots/    # file snapshots for /session rewind
      files/               # (empty in the sample) working file artifacts
      research/            # (empty in the sample)
      inuse.<pid>.lock     # present while a `copilot` process holds the session
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

`[observed, 1.0.67]` — flat YAML session metadata. The real file:

```yaml
id: <session-uuid>
cwd: /Users/alex/Devel/empathic/toolpath
git_root: /Users/alex/Devel/empathic/toolpath
repository: empathic/toolpath
host_type: github
branch: main
client_name: github/cli
name: List Directory Contents        # auto-generated session name
user_named: false
summary_count: 0
created_at: 2026-07-01T14:28:54.677Z
updated_at: 2026-07-01T14:31:30.280Z
```

Note there is **no commit/revision** field here — for the commit, use
`session.start`'s `context.headCommit` (see [events.md](events.md)).

`toolpath-copilot` prefers `session.start`'s `context` for `path.base` (it also
carries `headCommit`) and uses `workspace.yaml` only as a fallback. Its parser
is a tolerant key-scan (no YAML dependency), since this file's schema is not
officially documented. `session.db` and the global
[`session-store.db`](session-store-db.md) are alternative metadata sources but
not required for derivation.

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
