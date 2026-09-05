# toolpath-facto — ingesting what was DONE into the knowledge graph

Parked backlog (raised 2026-09-05). **Not started.** A design for an extractor
that lives in `bdelanghe/witness`, not in this repo — recorded here because it is
entirely about how toolpath documents are read, and because the `dead_end`
question it leans on is a live toolpath design issue (see the caveat at the end).
Kept on this standalone branch (no PR) rather than a shipped tree.


Design only. Owner of the pipelines is semantic-layer-entity-graph; this is a proposal.

## The gap

facto has extractors for what was DISCUSSED (zulip-facto, calendar-facto, meet-entities)
and for what is DECLARED (flake-lock, signed-apps, nix-atuin, git-trailers), and the
`discussedAndDeclared` view joins those two. Nothing records what was **done** — the
agent work itself is invisible to the graph. You can ask what was decided in a meeting
and what revision is pinned, but not which session did the work between them.

That record already exists locally. `~/.toolpath/documents/` holds 103 derived
documents (88 claude, 13 codex, 1 pi, 1 opencode; 222MB) plus a 152-record manifest.
Each is a toolpath `Path`: a DAG of `Step`s with actor strings, a `path.base`
directory, per-artifact changes, and token accounting.

## Conventions this obeys

Verified directly against the running build
(`/nix/store/ylmyi01rxf7cc7ril718cy6siy03s8hx-facto-ingest/lib/facto-ingest.js`):

- `vid(kind, key)` = `<kind>-<sha256(kind + "\0" + key)[0:24]>`, input-addressed over a
  **natural key, never props**.
- `eid(src, label, dst)` = `edge-<sha256(src + " " + label + " " + dst)[0:24]>` — note the
  literal `edge-` prefix, the same shape as `vid`'s `<kind>-` prefix.
- `EDGE_TYPES` entries carry `inverse_label` (confirmed: mentioned_in, said_by, attendee).
- A 900-second bucket grid is present in the same bundle.

**The `\0` separator is confirmed empirically, not merely read.** The pipeline owner read
the source as using `\n`; this bundle shows `\x00`. Resolved by computing both against a
vertex that already exists in the live graph:

    sha256("person" + "\x00" + "bobby@empathic.dev")[0:24] = be5ad053666068bfd97d17ac   <- match
    sha256("person" + "\n"   + "bobby@empathic.dev")[0:24] = a732e7cbf6afeb16535f4071

and the live Person vertex is `person-be5ad053666068bfd97d17ac`. A 24-hex coincidence is
2^-96, so this is settled: the separator is NUL. Worth recording *how* it was settled —
reading two sources and finding they disagree is not resolvable by reading harder; it
needed a value both could be checked against.

Not yet established: `timeProps()` emitting the `<name>` / `<name>Ms` / `<name>TRS`
triple with `http://dbpedia.org/resource/Unix_time` does **not exist on witness main**.
It is unmerged on `feat/claude-export` (bdelanghe/witness#37). Treat it as a convention
*landing* in #37 and re-verify against a bundle built after that merges. Likewise
`unique_per_pair` plus a required `schema.vocab` term per edge type, and `bucketOf()` in
app-attention.ts as the shared bucketer, are the pipeline owner's report rather than
verified here.

## Identity — the part that must not be got wrong

Because vids are input-addressed, **the key must be the session id and nothing else.**
Any key component derived from document *contents* — a step count, a content digest, a
last-modified time — changes when the document is re-derived, minting a second vertex
and orphaning the first. And since `eid` hashes both endpoints, every edge on that
vertex is orphaned with it. `p cache sync` re-derives on every stat change, so this
would not be a rare event; it would be the normal case.

**Session key: `path-claude-code-<first-8-of-session-uuid>`.** Not the raw harness uuid.

This is already the machine's spelling for a Claude session, verified at
`bdelanghe/witness/src/spend.ts:103`, which filters
`select(.path.id == "path-claude-code-${short}")`, and documented at
`spend.ts:189` as the join `session_id ↔ transcript filename ↔ path-claude-code-<short>`.
It is what attention.nix stamps on OTLP records and what `verb attention` joins lobby
sessions to. Keying on it means toolpath-facto, the attention poller and the spend
witness all land on one vertex with no mapping table. Keying on the raw uuid instead
creates two identities for one session and a join nobody wrote.

For the non-Claude harnesses the analogous key is `path-<harness>-<inner-id>`, which is
already toolpath's own cache-id scheme (`<source>-<inner-id>`, e.g. `codex-<uuid>`) — so
the same string is the cache filename and the vid key.

`session` is a **new vid kind** (the 23 in use are app, bot, bucket, channel,
conversation, device, dm, doc, domain, flake, machine, mailthread, meeting,
memory-file, org, package, person, project, pull, rev, role, thread, tool). The kind
string is part of the hash, so it is fixed forever at first write. `session` is the
right name: it is the harness-neutral noun, and `conversation` is already taken by the
Claude account export.

## Vertices

| kind | key | new? | properties |
|---|---|---|---|
| `session` | `path-claude-code-<short>` / `path-<harness>-<id>` | **new** | harness, stepCount, **deadEndCount**, startedAt+Ms+TRS, endedAt+Ms+TRS, durationS, inputTokens, outputTokens, cacheReadTokens |

`deadEndCount` is a **stored property, not a query-time derivation**, and that is a
deliberate call. Dead ends — steps not on the ancestry of `path.head` — are the only
place in this entire graph that records work which did *not* survive; every other
extractor is an inventory of things that exist. Deriving it lazily would mean re-walking
the DAG of a 222MB corpus on every query, which in practice means the question never
gets asked. Store it at ingest, when the document is already parsed.
| `repo` | `owner/repo` | existing | — (already minted by github-facto) |
| `person` | email, lowercased | existing | — |
| `rev` | commit sha | existing | — |
| `tool` | tool name | existing | — |
| `bucket` | `String(epochSeconds)` on the 900s grid | existing | — |

Only `session` is minted. Everything else is joined to, which is the point: a session
that touched `empathic/toolpath` attaches to the *same* repo vertex github-facto
already created.

Actor strings are `type:name` (`human:alex`, `agent:claude-code`, `tool:rustfmt`). The
`human:` prefix maps to `person` only when an email is resolvable — a bare name is not
a person key, and inventing one would collide in exactly the way the org kind already
suffers from. Where no email resolves, drop the edge rather than mint a name-keyed
person.

## Edges

| label | inverse | from → to | why |
|---|---|---|---|
| `worked_on` | `worked_on_by` | session → repo | the primary join; from `path.base`'s git origin |
| `ran_in` | `ran` | session → bucket | 900s grid, so session ↔ meeting ↔ app-focus ↔ tab all meet on time |
| `produced` | `produced_by` | session → rev | commits the session's steps touched |
| `driven_by` | `drove` | session → person | resolved actor, human: only |
| `used_tool` | `used_by` | session → tool | `tool:` actors and tool-call steps |

`schema.vocab`: `prov:wasAssociatedWith` fits `driven_by` and `prov:generated` fits
`produced` — PROV-O is the right vocabulary for a provenance format, which is what
toolpath is. For `worked_on` and `used_tool` I could not find a standard term that
means this without distortion. **Record the gap explicitly rather than inventing
`schema:performs`** — an absent vocab is a real answer and a wrong one is worse.

`ran_in` deliberately reuses the existing bucket join instead of a time range. Bucketing
is free, already shared by calendar-facto / web-entities / app-attention, and means no
consumer has to re-bucket or do interval arithmetic.

## What is NOT ingested

**No content. Identity and measurement only.** Every existing ingester stores the
identity of a thing and never its contents — no transcript text, no message bodies, no
doc bodies, no query strings. This holds that line exactly:

- **In:** step counts, dead-end counts, actor strings, durations, token counts, touched
  file *paths*, commit shas, harness name.
- **Out:** prompt text, model output, tool-call arguments and results, diff hunks, file
  contents, error messages, and anything under `change[].raw`.

222MB of derived documents is mostly conversation. None of it belongs in a graph db —
not for size reasons, but because the graph is an index of entities and their relations,
and the moment it holds prompt text it becomes a second copy of the corpus with worse
access properties and a much worse disclosure surface. Touched paths are the boundary
case: a path is identity (which file), a diff is content (what was written). Paths in,
diffs out.

## Three questions this makes answerable

1. **"Which sessions worked on the repo that this meeting decided to change, and what
   did they cost?"** Today `discussedAndDeclared` gets from a meeting to a pinned
   revision but there is no vertex for the work in between. session→bucket→meeting plus
   session→repo closes it, and the token properties make the cost answerable in the
   same query.
2. **"What did we spend on work that never landed?"** Dead ends are first-class in
   toolpath (steps not on the ancestry of `path.head`) and invisible everywhere else —
   git has no record of them. `deadEndCount` beside `outputTokens` makes abandoned work
   measurable for the first time.
3. **"Which revisions have no session behind them?"** `rev` vertices with no
   `produced_by` edge are commits no recorded agent session produced — human work, or a
   gap in capture. Either answer is worth having, and the second one audits the
   witnessing itself.

## Risks

- **Backfill cost.** 103 documents × 222MB parsed on first run. Stream per document and
  never hold the corpus in memory; the manifest's stat data (mtime+size) is the natural
  incremental gate, mirroring `p cache sync`'s own stat-level detection.
- **Harness id collision.** Claude short ids are 8 hex chars — ~4.3B space, fine at this
  scale, but the `path-claude-code-` prefix is load-bearing for disambiguation across
  harnesses. Do not shorten it.
- **path.base may not be a repo.** Sessions run outside a checkout have no origin. Emit
  the session vertex with no `worked_on` edge rather than skipping the session.
- **Re-derivation churn.** Properties change on re-derive (step counts grow as a session
  continues); the vid does not. That is the correct behaviour and the reason the key
  rule above is absolute.

## Caveat added 2026-09-05, after this design was sent

`deadEndCount` is specified above as a stored property, on the argument that dead
ends are the only record of work that did not survive. That argument depends on
`dead_end` meaning "abandoned", and a measurement taken after the design was
written casts doubt on it: across four real Claude sessions the classification
marks 757/1250, 3555/5602, 448/1073 and 554/1180 steps as dead ends — roughly
60% — and **zero turns** in any session. Head is always the final `system` event,
so its ancestry is every turn plus head, and every other event falls outside it.

On that evidence `dead_end` currently reads closer to "not an assistant turn"
than to "abandoned work". Storing it as `deadEndCount` before that is settled
would put a number in the graph whose name promises more than it measures — the
exact failure the identity rules above exist to prevent. **Resolve the toolpath
side first.**
