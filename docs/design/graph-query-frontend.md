# A graph query frontend over local and Pathbase queries

Status: design exploration, 2026-07. Nothing here is implemented.
Companion to `query-acceleration.md` (which explores making today's jaq
surface fast); this document explores a different axis — making local
and remote querying *the same thing*, using the query stack from
[facto](https://github.com/empathic/facto) (`graphdb`).

## The gap

Toolpath has two query models that share nothing:

- **Local** (`path query`): jaq over a flat array of wrapped steps.
  Good at step-content munging — projection, aggregation, selection
  over the step JSON. Bad at anything relational: multi-hop DAG
  traversal, delegation chains, joins across sessions.
- **Pathbase**: fixed-function surfaces. REST lists with `?limit` and
  nothing else. Internal GraphQL exposes containment filtering
  (`Path.steps(dataContains: …)` → JSONB `@>`) plus hand-carved
  traversal resolvers — `Step.ancestors`, `Step.descendants`,
  `Step.nearestAncestorWith(actorKind, changeType)`. Every new
  relational question costs a new resolver and new SQL.

The same question — "which sessions touched `src/main.rs` and what did
their dead-end branches try?" — is awkward-to-impossible in both, and
has to be asked in two unrelated languages depending on where the data
happens to live.

`nearestAncestorWith` is the tell: traversal questions exist, and
today each one is answered by writing engine code. A graph query
frontend turns them into queries.

## What facto provides

facto (`graphdb`) is a TAO-style graph-serving engine with three query
languages — **GraphJQ** (jq-flavored pipelines), **Cypher** (openCypher
read subset), **SPARQL 1.1** — that all lower to one shared IR
(`graphdb-ir::query_ast::Query`) run by one planner + executor, with a
written denotational semantics (`GRAPHJQ-SEMANTICS.md`) that makes
planner rewrites provably answer-preserving.

The properties that matter here:

- **The engine is storage-agnostic.** `graphdb-engine::traversal::
  execute<R: GraphReader>(reader, ext, config, graph_id, query)` — the
  engine consumes only the `GraphReader` trait from `graphdb-core`
  (~11 async read methods: `vertex_get`, `neighbor_ids_batch`,
  `find_ids_by_predicate`, `get_object_doc`, …). SQL push-down
  (`solution_query`) has a default returning `None`, so a host
  implements only the core reads. Data never has to live in facto's
  own Postgres.
- **The frontends are pure.** `graphdb-graphjq` / `graphdb-cypher` /
  `graphdb-sparql` are parser crates (text → IR) that depend on
  neither the engine nor any storage or async runtime. They compile
  anywhere the CLI compiles, and the IR is plain serde JSON.
- **Bounded serving semantics.** Depth clamps, frontier caps,
  per-query timeouts, and an honest `truncated` flag — a bad query
  degrades to a flagged partial answer, never an unbounded scan and
  never a silently wrong one.

## The shape: one IR, two executors

```
  GraphJQ text ─┐
  Cypher text ──┼─► frontend (pure, runs in path-cli) ─► query_ast IR
  JSON AST ─────┘                                           │
                              ┌─────────────────────────────┤
                              ▼                             ▼
                   LOCAL: graphdb-engine          REMOTE: POST IR (or text)
                   over GraphReader impl          to Pathbase /query;
                   on the SQLite index            server runs the same
                   (files stay canonical)         engine over path_steps
```

The IR is the wire contract. `path query` parses whichever frontend
the user (or agent) wrote, then either executes locally or sends the
IR to Pathbase — same semantics document, same planner, same executor
code, so the same query provably means the same thing in both places.
Scope selection stays a CLI concern: local scope flags pick cache
docs; a Pathbase URL/`owner/repo` scope routes the IR remotely.

### Local: `GraphReader` over the index

This composes with `query-acceleration.md`'s Approach A rather than
competing with it. The SQLite index gains graph shape — either the
step rows plus a derived `edges` table, or facto's core-schema layout
(`vertices` / `edges` / `property_index`) directly. Files remain
canonical; the index remains disposable; the `GraphReader` impl is
~11 methods of SQLite reads. The engine's batched-neighbor access
pattern (`neighbor_ids_batch`) is exactly an indexed
`WHERE src_id IN (…) AND label = ?` scan.

### Remote: `GraphReader` over what Pathbase already has

Pathbase's schema converged on the same model independently:

| facto core | pathbase today |
|---|---|
| `vertices` (JSONB props) | `path_steps.data` (JSONB, system of record) |
| `property_index` (hot scalars) | denormalized `actor_kind`, `change_type`, `timestamp` columns |
| `edges` | `step_parents` (the DAG), `graph_paths` (membership) |
| `has(k; p)` index pushdown | GIN `jsonb_path_ops` containment index |

A server-side `GraphReader` over these tables is a mapping layer, not
a migration. Visibility enforcement stays where it lives today: the
reader impl applies `Visibility::can_read` scoping to every id/
neighbor fetch, so the query engine can never see rows the REST/
GraphQL surfaces wouldn't show ("one rule, one helper, no drift"
extends to the new surface). The fixed GraphQL resolvers
(`ancestors`, `nearestAncestorWith`, `toolUsage`) become one-line
queries, and new questions stop costing resolver code.

## The property-graph mapping

One graph per scope (locally: the whole cache; on Pathbase: a
repo/viewer scope — open question below). Vertex/edge vocabulary,
derived deterministically from toolpath documents at index time:

**Vertices**
- `step` — props: the step body (actor, timestamp, intent, outcome,
  token usage…), plus materialized `dead_end` (computed at index time
  from head-ancestry, exactly as the jaq wrapper does today) and the
  identity triple (`cache_id`, `path_id`, `step_id`).
- `path` — the session: `kind`, `source`, `base`, `title`
  (first user message), token totals.
- `artifact` — a file/URL touched by any step, id = the normalized
  artifact key. Shared across sessions — this vertex is what makes
  cross-session file queries one hop instead of a scan.
- `actor`, `tool` — small dimension vertices (`human:ben`,
  `tool:rustfmt`); optional but cheap, and they make grouping
  traversals natural.

**Edges**
- `parent`: step → step (the DAG; = `step_parents` on the server)
- `in_path`: step → path (membership)
- `head`: path → step
- `touches`: step → artifact (props: change perspective summary,
  ± lines)
- `delegates`: step → path (sub-agent sessions)
- `invokes`: step → tool (= `step_tool_invocations`)

**Example queries.** GraphJQ (curl-friendly, pipeline-shaped):

```
# Sessions that touched a file
V("artifact", id: "src/query/plan.rs")
  | in("touches") | out("in_path") | dedup
  | { id: id(), title: .title, source: .source }

# Fork points: live ancestors of dead-end steps
V("step") | has("dead_end"; true)
  | repeat(out("parent"); max_depth: 8)
  | has("dead_end"; false) | dedup
```

Cypher (the same IR, the dialect agents already know):

```cypher
MATCH (s:step)-[:touches]->(:artifact {id: "src/query/plan.rs"}),
      (s)-[:in_path]->(p:path)
RETURN DISTINCT p.title, p.source

MATCH (s:step)-[:in_path]->(p:path)
WHERE s.actor STARTS WITH "agent:"
RETURN p.title, sum(s.output_tokens) AS toks
ORDER BY toks DESC LIMIT 10
```

Neither is expressible as *one* reasonable jaq program over the flat
wrapped-step array (the first needs a join through a shared artifact,
the second is fine in jaq — but the fork-point query is a bounded
transitive closure, which jaq simply doesn't have).

## Would it be agent friendly?

Yes — and more interestingly, the stack is agent-friendly in layers,
so different consumers can enter at different levels:

1. **Cypher is the agent dialect.** openCypher is abundantly
   represented in model training data (the Neo4j corpus); agents write
   `MATCH … WHERE … RETURN` fluently, with none of the learning-curve
   problem a novel language has. It is also facto's most-tested
   frontend (261 test markers + a TCK harness). An agent-facing
   `query` tool that accepts Cypher needs almost no prompt budget.
2. **GraphJQ is the human/shell dialect.** jq-flavored, curl-friendly,
   pipeline-shaped — but effectively zero training-data presence, so
   agents would need the language card in context. Fine for humans;
   second choice for agents.
3. **The JSON AST is the structured-generation dialect.** An agent (or
   a tool harness) can emit the IR directly under a JSON schema —
   syntax errors become unrepresentable, and the harness can
   constrain generation. This is the most reliable path for
   programmatic agent use.
4. **The feedback loop is built for iteration.** `validate` never
   4xxs and returns parse errors with line/column/span; `compile`
   returns the normalized AST and warnings; `explain` returns the
   physical plan. That triple is exactly the self-correction loop
   agentic query-writing wants — an agent can check before running,
   and repair from structured errors.
5. **Bounded semantics protect both sides.** Depth clamps, frontier
   caps, timeouts, and the honest `truncated` flag mean an agent's
   bad query costs a bounded amount and a partial answer is *labeled*
   partial — agents act on results without a human sanity-checking
   each one, so "never silently wrong" matters more for them than
   for interactive use.
6. **A schema card completes it.** Agents need the vertex/edge
   vocabulary in context. `path kind` already prints field-reference
   schemas; the graph vocabulary above is small enough to ship the
   same way (`path kind graph` or similar).

The honest limit: for pure step-content munging — reshaping the JSON
of steps you've already found — jaq remains stronger than GraphJQ's
projection language. The two surfaces are complementary: the graph
frontend normalizes *relational* queries and the local/remote split;
jaq stays for content transformation. (Agents know jq well too, so
the pairing costs little.)

## Friction and open questions

- **`graphdb-core` unconditionally depends on sqlx-core** (driverless)
  and the engine is async (`async-trait`). For path-cli: tokio is
  already in the tree (watcher features), so a small runtime shim
  suffices. For the wasm playground: unvalidated; simplest is to gate
  graph query off emscripten like `sync` already is. If the sqlx dep
  bothers us, splitting it out of `graphdb-core` is a facto-side
  cleanup (both projects are ours).
- **facto crates are `publish = false`.** Embedding means publishing
  the four crates path-cli needs (`ir`, `core`, `engine`, + frontends)
  or a git dependency. Ours to decide.
- **Tenancy mapping on Pathbase**: graph-per-repo keeps `graph_id`
  natural but makes cross-repo queries a fan-out; a viewer-scoped
  virtual graph inverts that. Needs its own pass.
- **`dead_end` and derived vertices** (`artifact`, `actor`, `tool`)
  must be materialized identically at both index sites (local SQLite,
  Pathbase ingest) or the "same query, same answer" promise breaks.
  The derivation belongs in one shared place — plausibly a small
  `toolpath-graph` crate defining the document → vertices/edges
  mapping, used by both.
- **Two query surfaces in one CLI** need a coherent story: `path
  query` (jaq, content) vs. a graph surface (`path query --graph` /
  `path g`?) — naming and docs matter so agents and humans pick the
  right tool without a decision tree.
- **facto is a proof-of-concept** by its own README (no auth, single
  backend). Embedding the engine/IR crates is the low-risk slice —
  the server, blobs, importers, and Wikidata layers stay behind.

## Cheapest end-to-end validation

Before committing to any of it: a spike that (1) implements
`GraphReader` over an in-memory graph built from one cached toolpath
doc (no SQLite work), (2) runs the two example queries above through
`graphdb_cypher::compile` + `graphdb_engine::traversal::execute`, and
(3) hands the same IR JSON to a copy of the engine mounted over a
pathbase-db test database. If that round-trips, the rest is schema
plumbing and product surface.
