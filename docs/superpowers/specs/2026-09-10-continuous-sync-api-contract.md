# Continuous sync: Pathbase API contract

**Status:** Agreed client/server contract for the sync MVP. OpenAPI in the
Pathbase repo is the normative artifact once S4 lands; this document fixes
the shapes both sides build against until then.
**Design:** `2026-09-02-continuous-sync-design.md`.

All routes are relative to `/api/v1/u/{owner}/repos/{repo}` unless noted.
Every non-2xx response is the existing `ApiErrorResponse { code, error }`.
`code` is a closed enum; this contract adds the values listed in "Error codes".

## Graph fields

Every graph representation (`GraphResponse`, `GraphSummaryResponse`,
`GraphDocumentResponse`, `GraphMetaResponse`) carries:

| Field | Type | Meaning |
| --- | --- | --- |
| `state` | `"mutable" \| "frozen"` | One-way. Nothing turns `frozen` back into `mutable`. |
| `generation` | integer (i64) | Compare-and-swap token. Increments on every committed document-content change and on freeze. Not a revision id. |

Visibility and name edits do not change `generation`.

## `GraphMetaResponse`

The lean result of every sync mutation and of `GET /graphs/{id}/meta`. It
never contains step payloads.

```json
{
  "id": "5f1c…",
  "url": "https://pathbase.dev/u/ben/pathstash/graphs/5f1c…",
  "state": "mutable",
  "generation": 7,
  "updated_at": "2026-09-10T18:00:00Z",
  "base": {
    "from": "https://pathbase.dev/u/ben/pathstash/graphs/3a9e…#claude-abc/step-041",
    "source_graph_id": "3a9e…",
    "source_path_id": "claude-abc",
    "source_step_id": "step-041"
  },
  "lineage": {
    "source_graph_id": "3a9e…",
    "source_path": "claude-abc",
    "continuation_graph_id": null
  },
  "paths": [
    { "id": "claude-abc", "server_id": "9d0b…", "head": "step-058", "step_count": 17 }
  ]
}
```

- `base` is present when this graph's single path has a structural base
  (`path.base.from`). `from` is the reference exactly as stored;
  `source_*` are the resolved target on this host.
- `lineage.source_graph_id` / `lineage.source_path` are set on a
  continuation (this graph continues that frozen path).
  `lineage.continuation_graph_id` is set on a frozen graph that already has
  its main-line child. A graph with neither is independent.
- `paths[].id` is the Toolpath path id; `server_id` the Pathbase UUID;
  `step_count` counts **owned** steps only (inherited history is not counted).

## Routes

### `POST /graphs` (existing, extended)

Body `UploadGraphBody` gains one optional field:

```json
{ "name": "…", "document": { … }, "visibility": "unlisted", "freeze_after": false }
```

- Authenticated callers create `mutable` graphs at `generation: 0`, or
  `frozen` at `generation: 1` when `freeze_after` is `true`.
- Anonymous uploads (`/api/v1/u/anon/repos/pathstash/graphs`) are always
  created `frozen`; `freeze_after` is ignored there.
- Response stays `201 GraphDocumentResponse` (flattened graph fields include
  `state` and `generation`). Sync clients follow up with `GET …/meta` only
  when they need path server ids.
- A document whose path carries `base.from` is rejected here with
  `400 invalid_base`; continuations go through `POST /graphs/{id}/continuations`.

### `GET /graphs/{id}/meta`

`200 GraphMetaResponse`. Read permission required (same rule as `GET /graphs/{id}`).
No transcript reconstruction.

### `PUT /graphs/{id}`

Replace the document **owned** by a mutable graph.

```json
{ "document": { … }, "expected_generation": 7, "freeze_after": false }
```

- The document's path set must equal the graph's owned path set (same
  Toolpath ids). Adding, dropping, or renaming a path is rejected with
  `400 invalid_document`.
- Steps are matched to existing rows by `(path id, step id)`; server UUIDs
  are preserved. Changed payloads are updated, new steps inserted in document
  order, absent owned steps deleted, parent edges rebuilt for affected rows.
- A step id stored anywhere along the base chain (inherited or not; frozen
  ids are reserved along the whole chain) is rejected with
  `400 inherited_step_redefined`, with or without any force flag. The path's `base.from` must equal the stored one (`400 base_retargeted`
  otherwise).
- `expected_generation` mismatch → `409 generation_conflict`. Frozen graph →
  `409 frozen`. Both are checked under the graph lock.
- `freeze_after: true` freezes in the same transaction; a failure of either
  half rolls back both.
- An identical document is a no-op: no row writes, `generation` and
  timestamps unchanged, `200` with the current meta.
- Response `200 GraphMetaResponse`.

### `POST /graphs/{id}/freeze`

```json
{ "expected_generation": 7 }
```

- Already frozen → `200` with current meta, generation unchanged (idempotent).
- Mutable and matching → frozen, generation +1, `200 GraphMetaResponse`.
- Mismatch → `409 generation_conflict`.

### `POST /graphs/{id}/continuations`

Create, or discover, the main-line continuation of a frozen path.

```json
{ "document": { … }, "source_path": "claude-abc", "expected_generation": 8, "freeze_after": false }
```

- `{id}` must be `frozen`; a mutable source → `409 source_not_frozen`.
- `source_path` is the Toolpath path id inside the source graph. The
  document must contain exactly one path, with `base.from` equal to
  `<this server's URL for {id}>#<source_path>/<step>` where `<step>` is the
  source path's head or an ancestor of it (a step on the frozen main line;
  derives such as Claude's put trailing sidecar steps after the last turn,
  so resumed work hangs off an ancestor of the head), and at least one
  owned step. `400 invalid_base` when the reference names another document,
  host, path, or step, or a frozen dead end that is not on the head's
  ancestry; `400 empty_continuation` when no owned steps are present;
  `400 invalid_document` when an owned step reuses an id stored anywhere
  in the source path or its own base chain (frozen ids are reserved along
  the whole chain, dead ends included).
- `expected_generation`, when present, must match the source graph's
  generation (protects against racing with a late visibility edit only; it is
  optional because a frozen graph's document cannot change).
- At most one main-line continuation per `(source graph, source path)`,
  enforced by a unique lineage row. A second create for the same source returns
  `200` with the **existing** continuation's meta (its `lineage` shows the
  source) and does not touch its content. `201` when created.
- The continuation is created in the source's repo with the source's
  visibility, `mutable` at `generation: 0` (or `frozen`/`1` with
  `freeze_after`).

### `DELETE /graphs/{id}` (existing, restricted)

`409 graph_has_dependents` when any continuation references this graph as a
structural base. Otherwise unchanged (`204`).

### `GET /graphs/{id}/download` (existing, "stored" view)

Returns the stored document: the graph's own paths and owned steps, with
`base.from` preserved on continuation paths and no inherited steps.

### `GET /graphs/{id}/paths/{path_id}/composed`

Returns a self-contained Toolpath `Path` document: the base's ancestry
through the selected step (recursively, deduplicated, transcript order,
depth-bounded), then this path's owned steps, with explicit parent ids
restored and the root step's `base.from` removed. Read permission is
checked for every graph in the chain at read time; an unreadable base →
`403 base_unavailable` (no partial transcript is served as complete).

### Anything else that mutates document content

Legacy `POST /graphs/{id}/paths/{path_id}/steps` (append), path header
`PATCH`, path `DELETE`: gated by `409 frozen` and by the shared-path rule
(`409 conflict` when a path is a member of more than one graph). They bump
`generation` when they change rows.

## Idempotency

Authenticated sync clients send `Idempotency-Key: <opaque, ≤ 128 chars>` on
`POST /graphs`, `PUT /graphs/{id}`, `POST /graphs/{id}/freeze`, and
`POST /graphs/{id}/continuations`.

- Scope: principal + repo + key. The record binds method, target route, and
  the SHA-256 of the exact decompressed request body.
- Replay with the same key and digest returns the original status and body,
  even if the graph has since been frozen or further updated.
- Same key, different method/target/digest → `409 idempotency_conflict`.
- Two concurrent requests with the same key: the second waits for the first
  and returns its result.
- Original target since deleted → `410 operation_target_deleted`.
- Records are retained; there is no short expiry window.
- Requests without the header behave as today (no replay protection). The
  sync engine never sends a mutation without a key.

## Transport

- Request bodies may be gzip-encoded (`Content-Encoding: gzip`); the server
  applies the configured body limit to the **decompressed** size and answers
  `413` before any graph mutation.
- Clients compress bodies above 256 KiB.

## Error codes

| Code | Status | Meaning |
| --- | --- | --- |
| `frozen` | 409 | Target graph is frozen; document content cannot change. |
| `generation_conflict` | 409 | `expected_generation` does not match. |
| `idempotency_conflict` | 409 | Key reused with a different request. |
| `graph_has_dependents` | 409 | Delete refused; a continuation references this graph. |
| `source_not_frozen` | 409 | Continuation source must be frozen. |
| `invalid_base` | 400 | `base.from` malformed, foreign, unresolvable on this host, or not on the source head's ancestry. |
| `base_retargeted` | 400 | PUT changed a stored `base.from`. |
| `inherited_step_redefined` | 400 | PUT resubmitted an inherited step id as an owned step. |
| `invalid_document` | 400 | Path set, ids, heads, or parents fail scoped validation. |
| `empty_continuation` | 400 | Continuation without owned steps. |
| `base_unavailable` | 403 | Composed read hit a base the viewer cannot read. |
| `operation_target_deleted` | 410 | Idempotent replay of an operation whose target was deleted. |

Existing codes (`not_found`, `unauthorized`, `bad_request`, `conflict`,
`visibility_locked`, `forbidden`, `internal_error`) keep their meanings.
Clients must switch on `code`, never on the status alone.
