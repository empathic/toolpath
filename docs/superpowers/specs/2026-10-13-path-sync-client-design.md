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
  `{ steps, last_step, head, has_parent, source_stamp? }`, where `steps`
  is how far into the local settled derive the remote chain covers (a
  continuation's parents included), `last_step` is the id at that
  position, and `has_parent` is true when the remote path carries a
  `path.parent`, i.e. it was created as a continuation. The server
  stores a path's edges only among its own steps (root steps descend
  implicitly from `parent.step`), so a batch appended to a continuation
  keeps only parents among them. Which ids a continuation owns and
  which it inherits is asked of the server, never inferred from
  positions in the local derive: once a session has been truncated and
  regrown, a continuation's own steps need not be a contiguous suffix
  of the derive, so the server is the only sound source. `has_parent`
  tells the client when to ask. Written on every record write; absent
  (an older record) reads as false. One record per
  session per destination (server + owner + repo); a new write replaces
  the old. Anonymous uploads and `path export pathbase` write none.
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
4. **A missing remote is recreated.** `404 not_found` on append means the
   graph is gone: create a fresh graph, re-key the record, report it.
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
   `steps = k`, `last_step = S[k-1].id`, `head`, `has_parent = false`,
   `source_stamp`.
4. **Record, fast check (every pass, O(1)).** If `k >= record.steps` and
   `S[record.steps - 1].id == record.last_step`, the prefix holds. If
   `k == record.steps`, nothing to send; refresh `source_stamp` only, so
   an unchanged session costs no request. Otherwise:
   - `has_parent == false` (a fresh graph, nothing inherited): append
     `S[record.steps..k)` to the recorded path URL plus `/steps`, with
     a trailing `Head`, in batches as the streaming client already
     does. Nothing is stripped, because nothing is inherited.
   - `has_parent == true`: read the path's ownership (below), and
     append the steps of `S[record.steps..k)` not in `O ∪ I`, each
     with its parents filtered as below. One read per pass for a
     continuation that grew is the accepted cost.

   On `2xx`, advance the record. On `400 invalid_document` (a new step
   does not fit what is stored, e.g. a parent the server lacks), go
   to 5.
5. **Record, mismatch (only when 4 fails, O(n) ids).** Read what the
   chain stores: for `has_parent == false`, the path's step ids
   (`O`, with `I` empty); for `has_parent == true`, its ownership. New
   steps are the settled steps `S[i]`, `i < k`, whose id is not in
   `O ∪ I`, in document order; append them, parents filtered as below,
   with a trailing `Head`. On `2xx`, write the record from the derive:
   `steps = k`, `last_step = S[k-1].id`. On `400 invalid_document` (a
   new step's parent is neither stored nor earlier in the batch; the
   derivation changed shape), go to 7. When the response carries
   `truncated_at`, `I` is partial: proceed, and a refusal with
   `inherited_step_redefined` recreates (7).
6. **`409 write_to_frozen`** on any append, from 4 or 5 alike: the path
   is frozen. Read what its chain stores as in 5 and take the settled
   steps not in `O ∪ I`; this is the continuation's step set, so steps
   an unrecorded earlier append stored are never claimed twice (none
   missing: refresh the record, create nothing). Build a new document
   whose single path carries
   `path.parent = { uri, path: <toolpath path id>, step }` on its
   `PathOpen`, where `uri` is the frozen graph's `url` as
   `GET …/graphs/{id}/meta` reports it (only the server knows its public
   URL), and `step` is the first new step's `parents[0]` when that id
   is in the frozen path's `O` (the server takes only a step the parent
   path owns), else `record.head`. Each step keeps only the parents
   earlier among the new steps. Create it as a new graph in the same
   destination and re-key the record to the new path URL with
   `steps = k`, `last_step = S[k-1].id` and `has_parent = true`. Report
   "continued". A continuation of a continuation is the same step.
7. **`404 not_found`** on append, `400 invalid_document` from step 5, or
   `400 inherited_step_redefined` on any append or on the continuation's
   create (an id stored above a parent the list could not show): create
   a fresh graph from `S[0..k)` exactly as in 3, replace the record with
   `has_parent = false`, report "recreated".

**Ownership of a continuation.** `GET` the path's step ids with
`through_parents=true`; the server lists the path's own ids first, then
its parents', nearest first, but does not mark where its own end, so
`GET` them without `through_parents` too. The second response is `O`,
the path's own ids; the rest of the first is `I`, the ids it inherits.
Every step appended to a continuation keeps only the parents in `O` or
earlier in the same batch. A merge edge into inherited history is lost
that way, and a step left with no parent descends from `parent.step`.

**Unsupported server.** A `2xx` whose body is not JSON (a release
Pathbase answers a route it lacks with its web app's page), or a `404`
or `405` with no error code, on any sync request (append, step ids,
graph meta) means the server does not support incremental sharing: the
session fails with a message naming the route, and nothing is
recreated. An error code this client does not know is the API
answering: the session fails with the server's code and message, and a
`404` with such a code is not a deleted graph to recreate. A create
response that lists no stored paths leaves nothing to record: a single
`path share` succeeds with a warning and writes no record;
`path share --all` deletes the graph it just made and fails that session
("server does not report stored paths; incremental upload unsupported")
rather than creating a duplicate every pass. When that delete fails, the
warning, the session's failure line and the pass's "Failed:" summary
name the graph's URL, so the user can delete it.

A single `path share` of a session with nothing settled prints one line
saying the session has no completed turn yet, and exits 0.

What the client never does: freeze, PUT, replay with idempotency keys,
compare step content, keep a list of pushed ids, or build a server URL
the server can report.

### Why the fast check is enough

Under decision 1 the server keeps whatever it has, in the order it has it.
A reorder inside the prefix that happens to keep the same last id is
therefore invisible by design, not a bug. The only facts the client needs
are "how far did we get" and "is that still where we are", which `steps`
and `last_step` answer.

### Prefix stability

Steps 4 and 5 assume the deriver is prefix-stable: a later derive of the
same source starts with the same steps in the same order. The Claude and
Codex derivers are (events at stream position; toolpath #144 and the
items IR). A deriver that is not takes the mismatch path on every pass,
which is slower but still correct.

## Bulk upload: `path share --all`

Enumerate sessions in scope exactly as #355 did (projects, harnesses,
include/exclude, the summary table). `--all` requires login: it fails
before any output when the user is logged out or the login does not
check out, and it refuses `--no-cache`. For each session, before
deriving, over the records that can stand for the pass's destination:
every record when destinations come from config (the destination is
known only after the derive), only the record at the named repo with
`--repo`, and only records on the named server with `--url`:

| Records (in scope) | Source stamp | Action |
| --- | --- | --- |
| none | any | derive, then create (append step 3) |
| any | known and equal to one record's | skip, no derive |
| any | unknown, or equal to none | derive, then append steps 4–7 |

A failed session's line and its "Failed:" entry name its destination
whenever it was resolved.

The destination is resolved after the derive, from the derived path's
recorded working directory (what `path share` uses), falling back to the
listing's directory when the derive records none: a listing's directory
can be a lossy decoding (a Claude project slug). A destination rule that
fails to parse fails that session only. The user's `pathstash` repo is
created lazily, the first time a session resolves to it on a server; a
failure there fails that session only.

Sessions run sequentially; the server serialises writes per path anyway.
A failure on one session is reported and does not stop the pass. Exit
non-zero if any eligible session failed.

`path share` of a single session takes the same code path, so a repeat
share of a recorded session appends instead of creating a second graph.

`path export pathbase` is not part of this: it uploads a whole document,
unsettled steps included, and writes no record, so a later append never
trusts it as a cursor.

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
  client can learn what is already stored upstream in one call. The walk
  stops at a parent the caller cannot read and names it in
  `truncated_at: {uri, path, step}`.

Already on the branches and relied on here: streamed open and append with
`Head` lines and `409 write_to_frozen` (#486); `parent` on `PathOpen`
resolved on this host (a readable frozen graph, a member path, a step it
owns; else `400 invalid_parent` or `409 parent_not_frozen`) and stored
unresolved when on another host (#469); `400 invalid_document` for a
step citing a parent neither stored in the path nor earlier in the batch;
`400 inherited_step_redefined` for an owned id stored along the parent
chain; `GET …/graphs/{id}/meta` reporting the graph's `url`.

## Acceptance test

The definition of done for the whole chain. Lives in `path-cli`'s
integration tests, ignored by default; `just acceptance` runs them against
a running Pathbase named by `TEST_PATHBASE_URL` and `TEST_PATHBASE_TOKEN`,
and fails saying what to set when they are missing.

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
   Assert a new graph exists whose `parent.uri` is the frozen graph's
   `url` as the server reports it and whose `parent.step` follows step 6,
   that its steps are the new ones only and its step ids through parents
   reach the frozen path's, and that the record is re-keyed to it with
   `has_parent` true.
   Then truncate the file below the continuation boundary, regrow it
   along new lines, and run a pass: assert the steps the chain lacks
   landed on the continuation, each with its parents among the
   continuation's steps intact and those into inherited history
   dropped, and that the record matches the derive. Freeze the
   continuation, grow, run a pass: assert a second continuation whose
   `parent.step` is a step the first one owns.
5. Delete the graph the record points at. Run a pass with new lines.
   Assert "recreated".
6. Same loop for a Codex rollout.

## Out of scope, recorded elsewhere

- Pull records, resume linking (`resumed_from` → `path.parent` at derive),
  resumption uploads: one follow-up PR after #356.
- Rewind-and-reappend of the unsettled tail as an append-route option, if
  the one-turn lag turns out to matter. Needs no record or format change.
- The mutation-log sidecar RFC for editing frozen history.
- The file-watcher sync mode.
- The UI: the delete dialog fed by the dependents reads, stub rendering for
  severed children, the stitched view.
