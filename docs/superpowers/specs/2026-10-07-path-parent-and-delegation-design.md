# Path parents and delegation

Status: draft, 2026-10-07. Replaces `path.base.from` in the continuous-sync
design (`2026-09-02-continuous-sync-design.md` §3, PRs #469/#470).

## Summary

A path that continues work from a step in another, frozen path names that
step in a new field, `path.parent`. The field carries a `relation` that says
how the new path relates to the step: `continuation` (the same line of
work, resumed) or `fork` (new work rooted at an older step).

A subagent is not a separate path. Its steps are a branch inside the parent
path, forked at the tool-use step that spawned it and merged back at the
tool-result step. The RFC's step DAG already represents this; the only
convention added is that `step.parents[0]` is the main line.

`path.base` keeps its current meaning: the repository or directory state the
work ran against. The RFC's `toolpath:<path>/<step>` base form, which named
a step in another path, is deprecated in favour of `parent`.

## Why not `base.from`

`base` is where the work ran (`uri`, `ref`, `branch`); lineage is a
different fact and a continuation needs both. `from` does not say what the
relationship is. A fork, a resumed session and a delegated subagent are
three different shapes; one untyped pointer cannot tell them apart, and the
server needs to for uniqueness and for the stitched view.

## `path.parent`

```json
"path": {
  "id": "p2",
  "base": { "uri": "github:org/repo", "ref": "abc123" },
  "head": "s9",
  "parent": {
    "uri": "https://pathbase.dev/u/org/repo/graphs/5b4e…",
    "path": "p1",
    "step": "s17",
    "relation": "continuation"
  }
}
```

| Field | Meaning |
| --- | --- |
| `uri` | The document holding the parent path. Same URI forms as `$ref`, no fragment: a Pathbase graph URL, an `s3://` or `file://` `.path.json`, … |
| `path` | Toolpath id of the parent path in that document. Required even for a single-path document, so a reader can check it. |
| `step` | Toolpath id of the step this path's root steps descend from. |
| `relation` | `continuation` or `fork`. Required. Unknown values are rejected. |

Pathbase resolves URIs on its own host only; any other URI is stored as
written and not resolved. Other stores (S3, local files) are the client's
to resolve.

Semantics: every root step of this path (a step with no `parents`) has the
parent step as its implicit parent. The composed read restores that edge.

In JSONL, `parent` travels on the `PathOpen` line with `base` and
`graph_ref`.

### Relations

| `relation` | Meaning | Server rule |
| --- | --- | --- |
| `continuation` | The same work, resumed after the parent path froze. | `step` is on the parent path's main line (its head's first-parent chain). At most one `continuation` child per parent path. |
| `fork` | New work rooted at an older step of a frozen path. | `step` exists in the parent path. Unlimited. |

Why the client declares the relation rather than the server deriving it:
derivers put trailing steps after a session's last turn, so a genuine
resume often names an ancestor of the head, and the server cannot tell that
from a fork. The client knows which one it did.

Handoff to another harness and compaction (a new segment that opens with a
summary of the old) are both `continuation`. Handoff is visible from
`meta.source`; compaction is a property of the first step, not of the
relation.

### Validation and errors

| Code | Status | When |
| --- | --- | --- |
| `invalid_parent` | 400 | Malformed URI, another host, unknown graph, `path` or `step`, a `continuation` naming a step off the main line, or unknown `relation`. An unreadable parent is reported as unknown. |
| `parent_not_frozen` | 409 | The parent path is mutable. |
| `continuation_exists` | 409 | A `continuation` child of this parent path already exists; the body names it. |
| `parent_retargeted` | 400 | A write changes a stored `parent`. |
| `parent_unavailable` | 403 | A composed read reached a parent the viewer cannot read. |

These replace `invalid_base`, `source_not_frozen`, `base_retargeted` and
`base_unavailable` from #469/#470. An owned step whose id is stored anywhere
along the parent chain is still `inherited_step_redefined`.

## Delegation: subagents as branches

```
… ─ turn-7 ─ agent-call ─────────────────────── agent-result ─ turn-8 (head)
                 └─ sub-1 ─ sub-2 ─ sub-3 ─────────┘
```

- The tool-use step that spawns the subagent is the fork point. The
  subagent's first step has `parents: [agent-call]`.
- The subagent's steps carry their own actor (`agent:claude-code/explore`,
  …) and chain linearly.
- The tool-result step is the merge: `parents: [agent-call, sub-3]`, or
  `[turn-N, sub-3]` when the main conversation moved on while the subagent
  ran.
- A subagent that was stopped or never reported back is a dead end. The RFC
  keeps dead ends.
- Nested subagents are branches off a branch. Nothing more is needed.

### First-parent convention

`step.parents[0]` is the main line. Any further parent is a branch being
merged in. This is git's first-parent rule and it is what makes the main
conversation and each delegation recoverable from structure alone:

- main line: walk `parents[0]` from `head` to a root;
- delegation ending at merge step `m`: start at `m.parents[1..]`, walk
  `parents[0]` until reaching a step on `m`'s own first-parent chain,
  exclusive. The same rule slices a delegation nested inside another.

Derivers must order `parents` accordingly. Readers must not assume
`parents` is unordered.

### Why not one path per subagent

The RFC does not support cross-path step references, and Pathbase drops
cross-path edges on write. A subagent in its own path could not merge back:
the result arriving in the main conversation would be a soft `meta.refs`
annotation, not an edge. The read-side cost of the branch model is small
(next section), so correctness of the join wins.

## Storage and reads

Pathbase already materialises `step.parents[]` as
`step_parents(path_id, child_step_id, parent_step_id)` with `path_id`
denormalised for single-path recursive queries. Two additions:

- `step_parents.ordinal` (smallint): the index of this edge in
  `step.parents`. Lets a query follow first parents without reading the
  JSONB.
- `path_parents(path_id, uri, path, step, relation, parent_graph_id,
  parent_path_id, parent_step_id)`, one row per path with a `parent`: the
  fields as written and the resolved target. Replaces `path_bases` and
  `graph_lineage`. A partial unique index on
  `parent_path_id WHERE relation = 'continuation'` enforces one
  continuation.
  FKs are `RESTRICT`: a graph with children cannot be deleted
  (`graph_has_dependents`).

Three reads over one path, each a recursive CTE clipped by `path_id`:

| Route | Returns | Cost |
| --- | --- | --- |
| `GET …/paths/{path_id}/line` | The main line: first-parent chain from `head`, as a Toolpath `Path`. | O(main line) |
| `GET …/paths/{path_id}/branches/{step_id}` | The branch merged at `step_id` (its non-first parents walked back to the merge's own first-parent chain), as a `Path`. | O(branch) |
| `GET …/paths/{path_id}/structure` | Fork steps (>1 child), merge steps (>1 parent), dead-end tips, with their actors. The UI's badges. | one aggregate |

The existing whole-path read is unchanged. The composed read across
`path.parent` (#469) follows `parent` to the parent path and takes its main
line through the parent step, recursively, depth-bounded, checking read
permission on every graph in the chain.

A derive-time delegation id on subagent steps (`meta.delegation`, the tool
use id) would make the branch slice a single indexed filter. It is an
accelerator, not part of the model; add it when the CTE measurably hurts.

## Writes

A continuation is a new graph in the parent's repo whose single path
carries `parent`. There is no separate continuations route:

- `POST /graphs` with a document whose path carries `parent` creates it
  (small documents).
- `POST /graphs` with `paths: []` followed by `POST /graphs/{id}/paths`
  whose `PathOpen` carries `parent` creates it (streamed). The server
  resolves and records the parent when the path opens; a refused `parent`
  leaves the graph created with no paths, as any refused first chunk does.

Validation runs under the parent graph's lock, then the child's, in UUID
order as the existing lock rule requires.

The server owns freezing (inactivity, `POST /graphs/{id}/freeze`); nothing
here takes `freeze_after` or `expected_generation`.

## Client

- `toolpath`: `PathIdentity.parent: Option<Parent>`; `Parent { uri, path,
  step, relation }`; `Relation { Continuation, Fork }`. `PathOpen` carries
  it. Schema and RFC updated;
  `toolpath:` base form marked deprecated.
- Claude deriver: derive sidechain transcripts into the same path. The
  `tool_use` id links the spawning step to the subagent file; the matching
  `tool_result` line is the merge. Emit subagent steps before the merge
  step. Order `parents` main line first.
- Other derivers: order `parents` main line first wherever a merge is
  emitted.
- Sync: a frozen remote with new local steps opens a continuation with
  `relation: continuation`; `409 continuation_exists` means append to the named
  graph if it is mutable, else open a new one continuing *its* head.

## Changes to #469 and #470

- `StructuralReference` parser, `path_bases`, `graph_lineage`,
  `bases::resolve/attach`, `lineage::record` → `path_parents` and one
  `parents::resolve` taking the four fields.
- `POST /graphs/{id}/continuations` removed; `POST /graphs` and the open
  route accept `parent`.
- `freeze_after` and `expected_generation` removed.
- Error codes renamed as above.
- `GET /graphs/{id}/meta`: `base` → `parent`, `lineage` → `continuation`
  (the `continuation` child, if any). `GET …/composed` unchanged in shape.
- `Idempotency-Key`, gzip bodies: unaffected by this document.

## Open questions

1. Should the whole-path read default to the main line, with branches on
   demand? Today's viewer renders every step in `seq` order; a path with
   many background subagents is several times its main transcript.
2. Token accounting over a path now includes subagent tokens. Correct, but
   visible; decide whether the UI shows main-line and total separately.
3. Is `fork` worth having at launch, or does the server accept
   `continuation` only and reserve `fork` in the schema?
