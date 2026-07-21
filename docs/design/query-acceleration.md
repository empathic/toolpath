# Query acceleration for the local cache

Status: design exploration, 2026-07. Nothing here is implemented.

## Problem

`path query` answers a jaq filter over every step in the local cache
(`~/.toolpath/documents/*.json`). The engine streams one document at a
time (`query/mod.rs::stream_files`), wraps each step in its source
context, and hands the planner (`query/plan.rs`) arrays it can often
stream or decompose instead of slurping. That bounds *memory* well, but
every query still pays the full parse cost of the cache:

- read every doc file,
- serde-parse each into a `Graph`,
- re-serialize each step to `serde_json::Value` (`wrap_path`),
- convert to `jaq_json::Val`,
- run the filter.

Measured baseline (2026-07-21, release build, M-series laptop):

| Cache | Query | Wall time |
|---|---|---|
| 97 docs, 220 MB, 46,182 steps | `length` | ~1.2 s |
| same | `map(select(.dead_end)) \| length` | ~1.2 s |

Almost all of it is user CPU — JSON parsing, not disk. Scope flags
(`--source`, `--project`, `--parent-dir`, `--kind`) don't help the
un-scoped case, and even scoped queries parse every doc before the
in-code filters discard most of them.

The design question: what structure, maintained ahead of query time,
avoids the most parsing — without changing any answer?

## Constraints (all approaches)

- **Files stay canonical.** `~/.toolpath/documents/*.json` remains the
  cache; any derived structure is an index that can be deleted and
  rebuilt from the files at any time. `sync.json` stays the manifest —
  it holds real state (evicted records, peeked cwds) that is not
  rebuildable from the files.
- **The planner never changes an answer.** Whatever the accelerator
  serves must equal the slurp path byte-for-byte, same guarantee (and
  same test style) as `query/filter.rs` asserts for streaming today.
- **Output order is part of the answer**: docs in `list_cached` order
  (file mtime, newest first), steps in document order.
- **wasm builds without it.** The playground has no rusqlite (it's in
  path-cli's `cfg(not(target_os = "emscripten"))` dependency block); an
  accelerator must be an optional fast path, not a dependency of
  correctness.

## Approach A: SQLite step-row index over the cache files

### Shape

One database, `$CONFIG_DIR/index.db`, holding every wrapped step as a
row. The row stores the step exactly as `wrap_path` emits it today —
`{"step": …, "change": …, "cache_id": …, "path": …, "dead_end": …}` —
in compact JSON, so index time absorbs the work queries currently
repeat: dead-end set computation, path-context cloning, wrapping, and
the double conversion (typed `Graph` → `Value` → `Val` becomes one
text → `Val` parse).

```sql
PRAGMA journal_mode = WAL;
PRAGMA user_version = 1;

-- One row per indexed cache file: the freshness stamp.
CREATE TABLE documents (
  cache_id   TEXT PRIMARY KEY,
  modified   TEXT NOT NULL,     -- file mtime at index time
  size       INTEGER NOT NULL,  -- file size at index time
  indexed_at TEXT NOT NULL
);

-- One row per wrapped step.
CREATE TABLE steps (
  cache_id  TEXT NOT NULL REFERENCES documents(cache_id) ON DELETE CASCADE,
  seq       INTEGER NOT NULL,   -- position within the doc: output order
  json      TEXT NOT NULL,      -- compact wrapped step
  -- Hot fields, extracted by the schema itself so they can't drift:
  step_id   TEXT GENERATED ALWAYS AS (json_extract(json,'$.step.id')) VIRTUAL,
  actor     TEXT GENERATED ALWAYS AS (json_extract(json,'$.step.actor')) VIRTUAL,
  ts        TEXT GENERATED ALWAYS AS (json_extract(json,'$.step.timestamp')) VIRTUAL,
  dead_end  INT  GENERATED ALWAYS AS (json_extract(json,'$.dead_end')) VIRTUAL,
  source    TEXT GENERATED ALWAYS AS (json_extract(json,'$.path.meta.source')) VIRTUAL,
  kind      TEXT GENERATED ALWAYS AS (json_extract(json,'$.path.meta.kind')) VIRTUAL,
  base      TEXT GENERATED ALWAYS AS (json_extract(json,'$.path.base.uri')) VIRTUAL,
  PRIMARY KEY (cache_id, seq)
);
CREATE INDEX steps_actor    ON steps(actor);
CREATE INDEX steps_dead_end ON steps(dead_end);
CREATE INDEX steps_source   ON steps(source);
CREATE INDEX steps_ts       ON steps(ts);
CREATE INDEX steps_base     ON steps(base);
```

`VIRTUAL` generated columns cost no row storage; the indexes on them
store the extracted values — exactly where they're wanted. Scope flags
become `WHERE` clauses against these columns; no separate per-path
table is needed because every step row carries its path context.

### Query execution: superset predicate pushdown

Compiling arbitrary jaq to SQL is a correctness tarpit, and it's never
necessary. The pushdown layer only emits a **necessary condition** of
the filter — a `WHERE` clause keeping a *superset* of what the filter
keeps — because jaq still runs the real filter over every surviving
row. A superset predicate can fail to prune; it can never change an
answer. This is `plan.rs`'s existing ethos (conservative recognition,
slurp when unsure) extended one level down:

- `map(select(.dead_end))` → `WHERE dead_end = 1`, residual jaq
  unchanged on survivors.
- `map(select(.step.actor | startswith("agent:")))` →
  `WHERE actor GLOB 'agent:*'`.
- Conjunctions push each recognized conjunct; unrecognized conjuncts
  push nothing (the residual handles them).
- `map(select(any(.change[].structural.token_usage; …)))` → nothing
  recognizable, push `TRUE`, scan all rows — today's behavior.

A second, stricter tier: when the recognized predicate is provably
*equal* to the select body (not merely implied by it) and the tail is
one the planner already understands (`length`, top-N), the whole query
collapses into SQL — `SELECT count(*) FROM steps WHERE dead_end = 1` —
and no JSON is parsed at all.

Expected effect at the measured cache size:

| Tier | `map(select(.dead_end)) \| length` |
|---|---|
| today (parse everything) | ~1.2 s |
| step rows, no pushdown | ~0.4–0.6 s |
| superset pushdown + residual | ~0.2 s |
| exact absorption into SQL | ~5 ms |

Rows stream `ORDER BY` doc-mtime-desc, `seq` — matching today's
ordering — into the existing planner unchanged. The byte-equality test
extends to the index path: index-served output must equal slurp output
exactly, over a seeded cache.

### Freshness: the index is disposable

- **Write-through**: `cache::write_cached` is the single funnel every
  doc write already goes through (import, share, sync). It re-indexes
  the doc in the same call: delete rows for `cache_id`, wrap once,
  insert, stamp.
- **Query-time stat gate**: at query start, stat the cache files
  against `documents` stamps (single-digit ms at ~100 files). Stale or
  unknown docs re-index lazily; rows for deleted files are purged.
  Out-of-band edits self-heal — same philosophy as sync's stat gate.
- **Deletable at any time**: removing `index.db` (or `p cache
  reindex`) rebuilds from files at roughly the cost of one slurp query.
  No migration ceremony; no "index disagrees with files" liability
  beyond one stat pass.
- **wasm**: no rusqlite → no index → the slurp path runs as today. The
  index is an accelerator, not a dependency.

### Costs

- **Storage roughly doubles.** The `json` column re-stores every
  wrapped step: ~+130–150 MB beside the current 220 MB (compact JSON
  vs. the pretty-printed files). A later "lean rows" variant could omit
  `change` bodies and fall back to files when the filter touches
  `.change` — deferred; it needs filter-touches-what analysis.
- **Sync gets slightly slower.** Indexing at write time is the
  parse+wrap the doc would otherwise pay on first query; the cost moves
  to where it amortizes.
- **A pushdown layer to maintain.** Each recognized predicate shape is
  planner code with the same never-change-an-answer obligation the
  streaming recognizer carries. Superset semantics keep mistakes
  non-catastrophic (a wrong necessary-condition merely mis-prunes —
  caught by the byte-equality tests — rather than silently changing
  results, provided it stays a superset).

### Implementation slices

1. Index module + write-through + stat gate + ordered streaming into
   the existing planner, byte-equality tests. (No speedup yet; locks in
   correctness.)
2. Superset pushdown for `select` prefixes over the hot columns.
3. Exact absorption for recognized predicate + recognized tail.

### Measured (implemented 2026-07-21, path-cli 0.17.0 + 0.18.0)

Both this approach and the parallel scan landed; numbers on the real
97-doc / 220 MB cache (medians of 5), against the pre-work baseline:

| Query | baseline | rayon (0.17.0) | + index (0.18.0) |
|---|---|---|---|
| `length` (absorbed) | 1233 ms | 596 ms | **27 ms** |
| `map(select(.dead_end)) \| length` (absorbed) | 1251 ms | 605 ms | **26 ms** |
| `map(select(.step.actor \| startswith("agent:"))) \| length` | 1287 ms | 612 ms | **254 ms** |
| `.[] \| select(.dead_end) \| .step.id` | 1273 ms | 615 ms | 724 ms |
| `map(.step.actor) \| unique` (slurp) | 1374 ms | 1203 ms | 1255 ms |
| `group_by(.path.id) \| map(length) \| max` (slurp) | 1378 ms | 1215 ms | 1267 ms |

Learnings vs. the estimates above: exact absorption delivered (~46×,
flat in cache size); predicated decomposes get ~5×; but a predicated
*stream* whose surviving rows carry most of the bytes (dead-end rows
hold the fat diffs) gains nothing over the parallel scan — row-fetch +
per-row parse ≈ parallel whole-file parse — and unpredicated slurps
are the parallel scan's territory. One-time index build: ~2.3 s;
index size ~420 MB next to the 220 MB cache (rows + three hot-field
indexes) — the "storage roughly doubles" estimate held. The
parse-only-parallelism tier estimated here as ~0.4–0.6 s was measured
and then superseded: per-file *filter execution* on the workers (what
0.17.0 actually ships) reaches ~0.6 s on this skewed cache and ~4.7×
on an even one, floored by the single 110 MB doc.

## Other approaches (to explore)

Sketches only; each gets its own section as it's worked through.

- **Parallel scan, no index.** Keep the architecture exactly as-is and
  parse docs on a thread pool. ~1.2 s is single-core; 8 cores → 
  ~200 ms with zero new state to maintain. Doesn't change asymptotics
  and can't serve selective queries in ms, but it's the honest
  baseline any index must beat — and it composes with every other
  approach.
- **Binary sidecar cache.** Store each doc's pre-wrapped steps as a
  fast binary serialization (e.g. postcard/bincode) beside the JSON,
  invalidated by mtime. Kills the JSON-parse cost (the dominant term)
  without SQL, pushdown, or a database; no help for selective queries
  beyond the parse win.
- **Columnar / analytical engine.** DuckDB (or parquet + DataFusion)
  over the step stream: vectorized scans make even full-cache
  aggregations fast, and SQL becomes a user-facing query surface. Heavy
  dependency; jq remains the compatibility contract, so it would sit
  beside jaq, not replace it.
- **Result memoization.** Cache query outputs keyed by (filter text,
  scope, index generation). Repeated queries become O(1); orthogonal to
  everything above.
- **FTS5 topic search.** Not a jaq accelerator: a different query
  modality ("find the session where I…") over user-prompt text /
  titles. Pairs naturally with Approach A's database if chosen.

A different axis entirely — normalizing local and Pathbase querying
under one graph query language (facto's GraphJQ/Cypher/SPARQL over a
shared IR, with the local SQLite index as one `GraphReader` backend) —
is explored in `graph-query-frontend.md`. It composes with Approach A
rather than competing with it.
