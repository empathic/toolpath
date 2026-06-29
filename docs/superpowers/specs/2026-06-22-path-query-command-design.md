# `path query` — querying the local cache

**Status:** Design proposal
**Date:** 2026-06-22

## Goal

`path query` reads the local cache at `~/.toolpath/documents/` and answers
questions across every cached document:

- "find every turn that mentions `RefCell`"
- "which sessions touched `cmd_resume.rs`?"
- "which turns burned > 50k tokens?"
- "the 10 steps that cost the most tokens"
- "every failed `Bash` call in my Claude sessions"

Toolpath is general (see `RFC.md`): a path can be an agent session, a PR,
a release. A path's step shape comes from its `meta.kind`, and the query
model works on whatever shape a kind defines.

## The model

One command, one idea: **load every cached step into a single JSON array
and transform it with a jaq filter.**

```
path query [scope flags] ['<jq filter>']
```

The filter is jaq — the language LLMs and power users already know — and
it does the matching, projection, sorting, grouping, and top-N. With no
filter, `path query` emits the scoped array; with a filter, it emits what
the filter produces.

The filter receives the whole array, so a per-element match is
`map(select(…))` or `.[] | select(…)`, and ranking and aggregation are
`sort_by(-.tokens) | .[:10]`, `group_by(.path.meta.source)`,
`unique_by(...)`.

`path query 'f'` equals `path query | jq 'f'`: bare `path query` prints
the array, and a filter is the same as piping that array to `jq`. The
filter runs in-process via the `jaq` crate (pure-Rust jq, regex enabled
for `test`/`match`).

## The step object

Each array element is a Toolpath step — `step`, `change`, `meta` verbatim
— wrapped with its source context:

```json
{
  "cache_id": "claude-abc123",
  "path": {
    "id": "session-…",
    "base": { "uri": "file:///Users/ben/empathic/oss/toolpath" },
    "meta": { "kind": "https://toolpath.net/kinds/agent-coding-session/v1.1.0", "source": "claude", "title": "Add path query" }
  },
  "step": { "id": "step-0042", "parents": ["step-0041"], "actor": "agent:claude-code", "timestamp": "2026-06-20T14:03:11Z" },
  "change": {
    "claude://session-…": {
      "structural": {
        "type": "conversation.append", "role": "assistant", "text": "…",
        "token_usage": { "input_tokens": 8123, "output_tokens": 412, … },
        "tool_uses": [ { "name": "Bash", "result": { "is_error": false } } ]
      }
    }
  },
  "meta": { "intent": "…" },
  "dead_end": false
}
```

The wrapper adds three keys: `cache_id`, `path` (the parent path's `id`,
`base`, and `meta`), and `dead_end` (whether the step sits off the head's
ancestry, computed while loading); `path query --help` lists them.
Everything under `step`/`change`/`meta` is verbatim Toolpath.

`change` maps each artifact to its perspectives; the structure inside is
what the **kind** defines — here, an `agent-coding-session`
`conversation.append`. Its fields (`token_usage`, `tool_uses`, `role`,
`text`, …) sit directly under `structural` alongside `type`, reached as
`.change[].structural.token_usage`. A git PR step's `change` holds raw
diffs and structural ops. The field set varies by kind, which is what
`path kind` (below) surfaces.

**Identity.** Step IDs repeat across sessions, so an element's unique
identity is the triple `(cache_id, path.id, step.id)`. Group or dedup on
that triple, or on `cache_id` for session-level rollups.

## Cold start: `path kind`

To learn a step's shape before querying, run `path kind`. A step's shape
is set by its kind, so that is what the command names.

```
path kind
```

It lists the kinds the binary bundles a spec for (today
`agent-coding-session`). `path kind <kind>` prints that kind's bundled
`schema.json`. The schema names every field, its type, and — in its
`description` fields — the semantics behind it (e.g. that `token_usage` is
a group total). `<kind>` is a value-enum of the bundled kinds, so
`path help kind` lists them.

```
path kind agent-coding-session
path kind agent-coding-session/v1.0.0
path query --kind agent-coding-session 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'
```

The bundled schemas live at
`crates/path-cli/kinds/<name>/<version>/schema.json` and publish at
`https://toolpath.net/kinds/<name>/<version>/`. For a doc whose kind the
binary bundles a spec for, `path kind` shows it; for any other doc, read a
sample with `path query --kind … '.[0]'`. `path query --help` points here.

### Which version `path kind` shows

The binary bundles each version of a kind's spec it ships with (today
`agent-coding-session` v1.0.0 and v1.1.0). `path kind <kind>` shows the
newest; a trailing `/<version>` (`path kind <kind>/v1.0.0`) pins one,
matching the same prefix rule as `--kind`.

## Scope flags

The filter expresses *what* to match; scope flags choose *which* documents
load. Two kinds:

**File selection** — which cached files to read:

| Flag | Effect |
| --- | --- |
| `--source <name>` | claude/gemini/codex/opencode/cursor/pi/git/github — selects files by cache-id prefix before parsing (fast) |
| `--id <cache-id>` | one (repeatable) cached document |
| `--input <file>` | an off-cache file (`-` for stdin) |

**Content scoping** — match on a parsed field:

| Flag | Effect |
| --- | --- |
| `--project <path>` | canonicalizes the path and compares it against `base`/cwd |
| `--kind <selector>` | semver-prefix match (see below) |

Everything else is a jaq predicate on the real structure: actor
(`.step.actor | startswith("agent:")`), files touched
(`.change | keys[]`), time (`.step.timestamp >= "2026-06-15"`), dead ends
(`select(.dead_end)`), structural type (`.change[].structural.type`), plus
ranking and aggregation.

### `--kind` matching

`path.meta.kind` is a semver-versioned URI
(`…/kinds/<name>/v<major>.<minor>.<patch>`). `--kind` matches a *prefix*
of `(name, major, minor, patch)`:

| `--kind` | Matches |
| --- | --- |
| `agent-coding-session` | any version |
| `agent-coding-session/v1` | `v1.*.*` |
| `agent-coding-session/v1.0` | `v1.0.*` |
| `agent-coding-session/v1.0.0` | exactly that |

A bare name matches any version; a full URI matches exactly; the `v` is
optional. Matching compares parsed `(name, major, minor, patch)` tuples,
so `v1` matches `v1.9.0` and keeps `v10.0.0` separate.

## Examples

The motivating questions, as commands (the `change` paths come from
`path kind`). The filter runs over the whole array, so selection reads
`map(select(…))`:

| Question | Command |
| --- | --- |
| Steps mentioning `RefCell` | `path query 'map(select(any(.. \| strings; test("RefCell"))))'` |
| Steps that touched `cmd_resume.rs` | `path query 'map(select(any(.change \| keys[]; endswith("cmd_resume.rs"))))'` |
| Turns over 50k input tokens | `path query 'map(select(any(.change[].structural.token_usage; .input_tokens > 50000)))'` |
| Failed `Bash` calls in Claude sessions | `path query --source claude 'map(select(any(.change[].structural.tool_uses[]?; .name == "Bash" and .result.is_error)))'` |

The first walks the step with recursive descent (`.. | strings`) to match
a term anywhere in it — the structure-agnostic search.
`any(generator; condition)` tests each generated value and keeps the step
when one matches.

The whole array is in scope, so rollups run in the same filter:

```bash
# which sessions touched cmd_resume.rs, deduped
path query '[.[] | select(any(.change | keys[]; endswith("cmd_resume.rs"))) | .cache_id] | unique'

# step count per source
path query 'group_by(.path.meta.source) | map({source: .[0].path.meta.source, steps: length})'

# top 10 steps by total tokens (input + output + cache)
path query --kind agent-coding-session '
  map({cache_id, step: .step.id,
       tokens: ([.change[].structural.token_usage // empty
                 | (.input_tokens//0)+(.output_tokens//0)+(.cache_read_tokens//0)+(.cache_write_tokens//0)] | add // 0)})
  | sort_by(-.tokens) | .[:10]'
```

## How it runs

Enumerate via `cmd_cache::list_cached()` (newest-first), then select files
by `--source`/`--id` prefix (a filename match, before parsing). Parse each
survivor with `Graph::from_json` and walk its `paths`. For each inline path
that passes `--project`/`--kind` (matched on the path's `base`/`meta`),
compute the dead-end set once via
`toolpath::v1::query::dead_ends(steps, &path.head)`, and wrap each step in
its envelope (`cache_id` + `path` context + `dead_end`). Assemble the array
in a deterministic order (graph order × path order × step order), run the
jaq filter once over it, and print.

`path kind` prints the requested kind/version's bundled `schema.json`,
or — with no argument — lists the bundled kinds.

A file that fails to parse is skipped with a stderr warning. The code
lives in a small `crates/path-cli/src/query/` module, with `cmd_query.rs`
and `cmd_kind.rs` as thin clap layers over it.

Output mirrors jq: pretty-printed JSON on a TTY, compact when piped (`-c`
to force). A top-level array prints as a JSON array; `… | .[]` yields
JSONL. Slice with `.[:N]` in the filter.

**Memory.** The whole scoped result set stays in memory while the filter
runs; ranking and aggregation read across all of it. The index (below)
carries this to larger caches.

## Future, not in v1

- **Index.** For a large cache, a derived, rebuildable index (e.g.
  SQLite + FTS5) accelerates the same command, flags, and output.
- **Redaction.** A transform stage between projection and output could
  scrub secrets/PII.
- **Remote.** `--remote <owner/repo>` could run the same filters against
  Pathbase once its API exposes filtering.

## Testing

Unit-test the wrapping (a fixture step emerges verbatim under
`step`/`change`/`meta`, with `cache_id`/`path` context attached),
`dead_end` over a small DAG, the walk from a graph's `paths` to their
steps, `path kind` printing a bundled `schema.json`,
`--kind` matching at each specificity (`v1` vs `v10` included), and file
selection by `--source`/`--id`. Integration via `assert_cmd` with a
`$TOOLPATH_CONFIG_DIR` sandbox of fixture docs: the four examples plus a
sort/top-N rollup, `path kind` output, a doc whose kind the binary bundles
a spec for and one it does not (both queryable), a malformed doc skipped
with a warning, deterministic array order across a parallel parse, and
compact JSON when piped.

The work is additive to `path-cli`: new `path query` and `path kind`
porcelain commands plus the `jaq` dependency. **Breaking:** `path query`'s
former subcommands change — `ancestors` moves to `path p query ancestors`,
and `dead-ends`/`filter` become jaq forms (`map(select(.dead_end))`,
`map(select(.step.actor | startswith("agent:")))`). Pre-1.0, so a minor
version bump; the `CHANGELOG.md` entry calls out the change, and the
`CLAUDE.md` CLI docs update.

## Decisions

1. **Embed `jaq`.** `path query` runs jaq in-process (pure-Rust) for the
   one-liner UX; `path query | jq` is the same filter piped out.
2. **Whole-array input.** The filter receives every scoped step as one
   array, so ranking and aggregation are plain jaq.
3. **`path kind` prints the bundled `schema.json`.** The schema is the
   field/type/semantics reference, authored once and shipped with the
   binary.
4. **Empty result exits 0.** `path query` is a stream transformer; exit 0
   means it ran, and exit 1 means an error.
