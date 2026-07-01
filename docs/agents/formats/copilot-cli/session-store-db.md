# `session-store.db` (the cross-session index)

`~/.copilot/session-store.db` is a **SQLite** database `[official]`
([config-dir reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference),
which describes it as holding "cross-session data such as checkpoint indexing
and search"). It is an **index**, not the source of truth — the per-session
`events.jsonl` files (see [session-state.md](session-state.md)) are authoritative
for content; this DB exists so the CLI can list, search, and resume sessions
without parsing every event stream.

## Reported schema

`[reverse-eng, Medium]` — six tables, from the
[jonmagic write-up](https://jonmagic.com/posts/github-copilot-session-search-and-resume-cli/)
(single source; table/column names are paraphrased there, so treat exact
identifiers as unconfirmed):

| Table | Reported contents |
|---|---|
| `sessions` | One row per session: id, an **auto-generated summary**, repository, branch, timestamps. |
| `turns` | User messages and assistant responses. |
| `checkpoints` | Titled snapshots with overviews / next steps (mirrors the on-disk `checkpoints/` dir). |
| `session_files` | Every file touched during the session. |
| `session_refs` | Commits, PRs, and issues linked to the session. |
| `search_index` | An **FTS5** full-text index across all the above content. |

Two things stand out for a derivation:

- **`sessions` gives cheap metadata** — id, summary, repo, branch, timestamps —
  without reading any `events.jsonl`. The summary is auto-generated (no LLM call
  at read time), which makes it a natural source for a `first_user_message`-style
  listing field and for `ConversationMeta`.
- **`session_files` is a manifest of touched files** — useful to cross-check
  `ConversationView.files_changed` and, combined with `checkpoints`, to recover
  file diffs the event stream may not carry inline (see
  [file-fidelity.md](file-fidelity.md)).

## How it relates to `events.jsonl`

```
                writes both
copilot CLI ───────────────────┐
                                ▼
   session-state/<id>/events.jsonl   ← append-only source of truth (content)
                                │
                                │ indexed into
                                ▼
   session-store.db (sessions, turns, checkpoints,
                     session_files, session_refs, search_index)  ← discovery / search
```

`[inferred]` — the division of labor: `events.jsonl` is the stream the session
is reconstructed from; `session-store.db` is a derived index that powers
`/resume`'s picker, name/prefix matching, and full-text search. We have **not**
verified whether the DB is always consistent with the event streams (e.g. after
a crash), so a provider should treat **`events.jsonl` as primary** and the DB as
an optional accelerator for listing.

## Reading it safely

If a future `toolpath-copilot` reads this DB, open it **read-only** (as
`toolpath-opencode` and `toolpath-cursor` do their SQLite stores) so a running
Copilot CLI session isn't disturbed and no write lock is taken:

```
sqlite "file:~/.copilot/session-store.db?mode=ro"
```

Because the schema is single-source `[reverse-eng]` and unversioned, prefer
defensive queries (`SELECT` only the columns you need, tolerate missing
tables/columns) over assuming the layout above is exact. The authoritative
listing path remains walking `session-state/*/` directly; the DB is the fast
path when it's present and parseable.
