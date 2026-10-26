# test(goldens): what the site and RFC promise — path query over every session, the core types, both file formats, a schema check on every transform

Status: planned (asked 2026-10-19 on the goldens demo deck: test "Query every session", "do we demonstrate tests over these types", "and both formats", "can we do a schema check on transform") · Base: the goldens stack once merged

## Why
toolpath.net and RFC.md make claims the goldens did not test: `path query` runs one filter across every session whichever
agent wrote it; a document is a Graph of Paths of Steps with base, head, dead ends, forks, merges, actors and
perspectives; `.path.json` and `.path.jsonl` are the same document; and every transform yields a document the
`agent-coding-session` kind accepts. Each is now a pinned test, and where the code falls short the gap is pinned as it
is, with a qualifier record, rather than claimed.

## What changes
- `crates/path-cli/tests/query_every_session.rs`
  - `query@site-example`: the cache holds the nine pinned `document@<set>` (all seven harnesses) and the site's exact
    filter runs through `path query`. It works on v1.1.0 and returns output tokens per model: one `agent:` group per
    harness (plus `human:user`, and `tool:` groups with 0).
  - `query@cache-sync`: `path p cache sync` over real session files placed where Claude Code, Codex, Copilot, pi and
    Gemini CLI keep them, twice (the second sync is all-unchanged: incremental), then the same filter. opencode and
    Cursor keep sessions in SQLite stores this test does not build; their documents are covered by the first pin.
- `crates/path-cli/tests/core_types.rs`
  - `core_types@dag`: on `examples/path-04-exploration`, dead ends equal all steps minus `ancestors(head)` (the RFC's
    definition): `step-002a`; a fork at `step-001` (three children); a merge at `step-004` (`step-003b`, `step-003c`).
  - `core_types@validate`: `path p validate` on each valid document and its minimal invalid twin, one row per rule.
    Rejected as the RFC says: a step or a path as the document root, an actor prefix outside human/agent/tool/ci.
    **Accepted although the RFC forbids them** (`validator_does_not_enforce`): a duplicated step id, a duplicated path
    id, a parent naming no step, a head naming no step, a `$ref` to a `.path.jsonl` (qualifier concerns on RFC.md 122,
    127).
- `crates/toolpath-goldens/tests/formats.rs`
  - `formats@<set>`: each golden document streamed to `.path.jsonl` and sealed back is identical in canonical form (all
    nine).
  - `formats@examples`: the committed `examples/*.path.jsonl` against their `.path.json` twins. They differ only in
    `graph.id` (a stream carries no graph identity; sealing names the graph after its path, the twins use
    `graph-<path id>`), pinned and recorded (qualifier comment on RFC.md 116).
  - The two refusals: a multi-path graph and a `$ref`-only graph have no JSONL form.
  - `examples/path-02-local-session.path.jsonl` had drifted from its twin (no `base.ref`); fixed.
- `crates/toolpath-goldens/tests/transforms_schema.rs` and `pipeline::writer_with_document`
  - `validate_transforms@<set>`: the derived document, every round trip derived again (all 54 pairs) and the sealed
    JSONL, each validated against the base schema and the v1.1.0 kind schema, with the `meta.kind` each path claims; a
    path claiming no kind fails (it would otherwise skip the kind schema silently).
  - `validate_transforms@writers`: the generated full document through each writer alone, derived again.
  - All valid, all claiming v1.1.0.
  - `bundled_kind_schema_is_the_published_one`: the bundled and site copies are byte-identical. Compared with the live
    https://toolpath.net/kinds/agent-coding-session/v1.1.0/schema.json by hand on 2026-10-19: identical, sha256
    `0557c0c06fe285704a672ac7f86abbd78f2e6e17e44aed0abbbdcc524ad523da`.

## Verification
New tests pass; gates as `scripts/quality_gates.sh` with `--features path-cli/goldens` (see the commit). Snapshots are new
kinds only; no existing snapshot changed.

## Paths
```
crates/path-cli/tests/query_every_session.rs
crates/path-cli/tests/core_types.rs
crates/path-cli/tests/snapshots/
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/tests/formats.rs
crates/toolpath-goldens/tests/transforms_schema.rs
crates/toolpath-goldens/snapshots/
examples/path-02-local-session.path.jsonl
.qual
```
