# Path sync client: append, bulk upload, periodic job

Design for the client side of path sync in `path-cli`: incremental append
of a session to its remote path, `path share --all` rebuilt on top of it,
and the periodic `path sync` job that runs it. Server side, it assumes the
routes on pathbase #486 (streamed JSONL open and append, freeze route in
#506) and #469 (`path.parent`), plus one small addition named below.

This file records the decisions the owner made on 2026-10-13 and the
algorithm they imply. It lives at the same path in both repos; the toolpath
copy is the one the implementing PRs cite.

## Terms

- **Record**: the entry in `$CONFIG_DIR/remotes.json` for one session on
  one destination (toolpath #356). Keyed by harness, then session id, then
  the path's API URL
  `<server>/api/v1/u/<owner>/repos/<repo>/graphs/<g>/paths/<p>`. Value:
  `{ steps, last_step, head, source_stamp? }`, where `steps` is how far
  into the local derive the remote chain covers (a continuation's parents
  included) and `last_step` is the id at that position. One record per
  session per destination (server + owner + repo); a new write replaces
  the old. Anonymous uploads write none.
- **Destination**: where a session goes, resolved from config as `share`
  resolves it today (server, owner, repo).
- **Settled step**: a derived step that a later turn follows. The current
  message group and any trailing steps whose tool calls have no result yet
  are unsettled.

## Decisions

1. **A pushed step is immutable.** A mutable path is append-only. A
   result or token count that would change a step after it was pushed
   does not reach the server. Editing stored history is a separate RFC (a
   mutation-log sidecar path), not this work.
2. **Only settled steps are pushed.** This is what makes 1 safe in
   practice: harnesses deliver tool results and close a message group
   before the next turn, so a step with a later turn after it is final. A
   shared session lags its source by the current turn.
3. **Mismatch is handled, not fatal.** When the local derive no longer
   starts with what the record says was sent, the client continues in the
   same remote path, pushing the steps the server lacks, and leaves the
   server's extra steps where they are.
4. **A missing remote is recreated.** `404` on append means the graph is
   gone: create a fresh graph, re-key the record, report it.
5. **Freezing is the server's.** `409 write_to_frozen` on append means
   create a continuation and re-key the record.
6. **The periodic job's behaviour is configurable**: scope, interval,
   destinations, and what it reports.

## Append: one session, one destination

Input: a session (harness + id), its destination, the record if any.

1. **Derive** the session with the harness reader. Let `S` be the steps
   in document order and `n = len(S)`.
2. **Settle.** Let `k` be the index of the last settled step plus one:
   drop from the tail every step in the last message group (same
   `group_id`) and every trailing step with a `tool_use` that has no
   `result`. Steps `S[0..k)` are pushable. If `k == 0`, stop.
3. **No record → create.** Stream `S[0..k)` as a new graph (open with
   the first batch, append the rest, trailing `Head` = the head among the
   pushed steps). Write the record from the create response:
   `steps = k`, `last_step = S[k-1].id`, `head`, `source_stamp`.
4. **Record, fast check (every pass, O(1)).** If `k >= record.steps` and
   `S[record.steps - 1].id == record.last_step`, the prefix holds. If
   `k == record.steps`, nothing to send; refresh `source_stamp` only.
   Otherwise append `S[record.steps..k)` to the recorded path URL plus
   `/steps`, with a trailing `Head`, in batches as the streaming client
   already does. On `2xx`, advance the record.
5. **Record, mismatch (only when 4 fails, O(n) ids).** `GET` the path's
   step ids from the server (see Pathbase additions). Let `R` be that
   set. Append, in document order, every `S[i]` with `i < k` and
   `S[i].id ∉ R`, with a trailing `Head`. On `2xx`, write the record from
   the derive: `steps = k`, `last_step = S[k-1].id`. On
   `400 invalid_document` (a new step's parent is neither stored nor
   earlier in the batch; the derivation changed shape), go to 7.
6. **`409 write_to_frozen`** on any append: the path is frozen. Build a
   continuation: a new document whose single pushed path carries
   `path.parent = { uri: <graph document URL>, path: <toolpath path id>,
   step: record.head }` on its `PathOpen`, and whose steps are
   `S[record.steps..k)` (owned steps only). Root steps of that document are
   implicitly children of `parent.step`; the server restores the edge on
   the composed read. Create it as a new graph in the same destination and
   re-key the record to the new path URL with `steps = k` and
   `last_step = S[k-1].id`, the same write as every other success.
   `steps` always means how far into the local derive the remote chain
   covers, parents included, so the fast check in 4 needs no offset.
   Report "continued".
7. **`404`** on append, or `400 invalid_document` from step 5: create a
   fresh graph from `S[0..k)` exactly as in 3, replace the record, report
   "recreated".

What the client never does: freeze, PUT, replay with idempotency keys,
compare step content, or keep a list of pushed ids.

### Why the fast check is enough

Under decision 1 the server keeps whatever it has, in the order it has it.
A reorder inside the prefix that happens to keep the same last id is
therefore invisible by design, not a bug. The only facts the client needs
are "how far did we get" and "is that still where we are", which `steps`
and `last_step` answer.

### Prefix stability

Steps 4 and 5 assume the deriver is prefix-stable: a later derive of the
same source starts with the same steps in the same order. toolpath #144
makes the Claude deriver prefix-stable (events at stream position) and its
second qualifier pass found the Codex deriver is not yet (an empty assistant
turn that is dropped and later reappears). Until a deriver is prefix-stable,
every pass for that harness takes the mismatch path, which is slower but
still correct.

## Bulk upload: `path share --all`

Enumerate sessions in scope exactly as #355 did (projects, harnesses,
include/exclude, the summary table). For each session and its destination:

| Record | Source stamp | Action |
| --- | --- | --- |
| none | any | create (append step 3) |
| present | known and equal to the record's | skip, no derive |
| present | unknown, or different | derive, then append steps 4–7 |

Sessions run sequentially per destination; the server serialises writes
per path anyway. A failure on one session is reported and does not stop
the pass. Exit non-zero if any eligible session failed.

`path share` of a single session takes the same code path, so a repeat
share of a recorded session appends instead of creating a second graph.

## Periodic job: `path sync`

A timer around `path share --all`. One pass = one bulk upload over the
configured scope, then a status line. `path sync install` writes the
`[sync]` config and a launchd agent or systemd user timer that runs
`path sync` by absolute path; `uninstall` removes the timer and keeps the
records. Config, scope resolution, the upload lock and the service plumbing
are lifted from the closed toolpath #268 (`sync_config.rs`,
`sync/sources.rs`, `sync/lock.rs`, `sync/state.rs`, `sync_service.rs`);
its activity clock, journal, segmenter and PUT classification are not.

Configurable: scope (`include`, `harnesses`, per-project `sync = false`),
`interval`, destination per project, verbosity of the status output. The
file-watcher mode is a later design.

## Pathbase additions

One PR, stacked on #469:

- `GET /api/v1/u/{owner}/repos/{repo}/graphs/{id}/paths/{path_id}/step-ids`:
  the path's step ids in document order. With `?through_parents=true`,
  the ids along the parent chain too, nearest first, so a continuation
  client can learn what is already stored upstream in one call.

Already on the branches and relied on here: streamed open and append with
`Head` lines and `409 write_to_frozen` (#486), `parent` on `PathOpen`
resolved on this host and stored as written otherwise (#469),
`400 invalid_document` for a step citing an unstored parent (#469).

## Acceptance test

The definition of done for the whole chain. Lives in `path-cli`'s
integration tests, gated on a local pathbase (`TEST_PATHBASE_URL`).

1. Copy a real Claude Code session file into a temp harness directory,
   truncated to its first N lines. Run one pass. Assert the server path's
   step ids equal the local settled derive's, and the record matches.
2. Grow the file by M lines. Run a pass. Assert only the new settled steps
   were sent (count the request bodies), the server path equals the
   derive, the record advanced.
3. Repeat 2 until the file is complete. Assert the server path equals the
   full settled derive (the test appends a closing user line so the last
   group settles).
4. Freeze the graph with `POST .../freeze`. Grow the file. Run a pass.
   Assert a new graph exists with `parent = { uri, path, step = old head }`,
   its steps are the new ones only, and the record is re-keyed to it.
5. Delete the first graph. Run a pass with new lines. Assert "recreated".
6. Same loop for a Codex rollout. Until the Codex deriver is prefix-stable
   this is the test that fails.

## Out of scope, recorded elsewhere

- Pull records, resume linking (`resumed_from` → `path.parent` at derive),
  resumption uploads: one follow-up PR after #356.
- Rewind-and-reappend of the unsettled tail as an append-route option, if
  the one-turn lag turns out to matter. Needs no record or format change.
- The mutation-log sidecar RFC for editing frozen history.
- The file-watcher sync mode.
- The UI: the delete dialog fed by the dependents reads, stub rendering for
  severed children, the stitched view.
