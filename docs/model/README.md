# The goldens work as one object model

`model.json` is every object the goldens work talks about, in one file, with ids and
links: the harnesses, the operations (toolpath.net's words: parse, project, resume,
share), the golden sets, every snapshot, every finding (loss, gain, invented value,
rename, known defect, declared-unpinned), the claims the site makes with their status,
the PR pieces, the qualifier concerns, and the review feedback. It is **generated**,
never edited: `crates/toolpath-model` reads the repositories at a revision and writes
it. The point is that review feedback lands on objects that code and PR records also
point at, so a comment changes the record and the code, not only a slide.

```
cargo run -p toolpath-model -- \
  --pathbase ../pathbase --pathbase-rev origin/wip/bdelanghe \
  --feedback http://clax.localhost/a/hennefgqrc50=docs/model/feedback/goldens-demo.threads.json
```

`--toolpath-rev` defaults to `HEAD`; the model records the full revision of each
repository it read (`sources`), so it names exactly the trees it describes. Commit the
code first, then generate and commit the model (the same "repin after" pair the
goldens manifest uses).

## Borrow, build, or reuse (prior art, 2026-10-19)

| candidate | what it gives | decision | why |
|---|---|---|---|
| Toolpath document format (RFC, `schema/toolpath.schema.json`, kinds) | a path of steps with provenance | **not the carrier** | it models one session's history, not a project's tests, claims and reviews; stretching a kind to hold findings and PRs would blur the format the site sells |
| insta snapshots (`crates/*/snapshots`) | the evidence, per stage per set, already committed | **borrow as input** | every snapshot becomes an object; findings are read out of `round_trip`, `shed`, `known_defects`, `losses` |
| qualifier records (`.qual`, metabox) | review findings at code spans, with supersedes | **borrow as input** | each record becomes a concern; resolve/supersede chains give open/resolved |
| `pr/NN-*.md`, `pr/stack.json` | the virtual PRs and the jj cut | **borrow as input** | pieces, their status, cited commits, jj change ids, GitHub numbers |
| ax-rebuild entity extractor (bdelanghe/scratch.main 8fdb775) | JSON-LD + facto bulk body, path+CAS identity | **borrow the shape later** | the same id-and-provenance idea; this model keeps plain JSON ids so a JSON-LD/facto export is a projection of it, not a second source |
| facto (personal knowledge graph) | storage and query across projects | **not now** | a sink for the model once it settles; the repo must hold its own record first |
| JSON-LD / schema.org | shared vocabularies | **not now** | none of the types (snapshot, known defect, piece) have a schema.org match worth the indirection |
| hand-written JSON | quickest | **rejected** | it drifts the day after it is written; that is the problem this solves |

Built: one small crate (no new dependencies beyond the workspace's), a JSON Schema
(`model.schema.json`), the declared claim list (`claims.json`: quote + operations +
evidence kinds; the status is computed, never declared), and the test.

## The objects

| type | id | read from | links |
|---|---|---|---|
| harness | `harness:<name>` | the seven, in the site's names; versions from `goldens/manifest.json` | |
| operation | `operation:parse\|project\|resume\|share` | fixed; each names the snapshot kinds that evidence it (resume: none) | |
| set | `set:<name>` | `ir@<set>.snap` headers (input file, sha) | harness |
| snapshot | `snapshot:<repo>/<kind>@<set>[@<target>]` | `crates/toolpath-goldens/snapshots`, `crates/pathbase-goldens/snapshots` | set, operation, target |
| finding | `finding:<kind>:…` | `round_trip` lost/gained (one per target and key path), `shed` dropped/renamed, `known_defects`, pathbase `losses`, open concerns tagged declared-limit or naming an unpinned edge | evidence snapshots, sets, pieces, concerns |
| claim | `claim:<id>` | `claims.json`, quote checked against the site source | evidence snapshots, findings against |
| piece | `piece:<repo>/<NN>` | `pr/NN-*.md`, `pr/stack.json` | cited commits (on wip or not), jj ids, GitHub PR, Changeling URL |
| concern | `concern:<id8>` | every `.qual` record | supersedes; status open/resolved/superseded |
| feedback | `feedback:<thread>` | a Clax threads export | about (objects the question names), resolved_by (commits and pieces the replies name) |

**Gains and invented values.** A gain is a key a round trip adds. The generator looks up
the gained key's value in each projection snapshot; when three or more sets get the
same scalar, the gain is an **invented value** (the writer made it up: `permissionMode:
"default"` in every Claude projection, `model_provider: "openai"` in every Codex one).

**Claim status** is the claim's own qualifier record, not computed twice: every claim in
`claims.json` (the home page, the format page, the RFC) has a record on its file, tagged
`claim:<id>`, spanning its text. `pass` (a committed test or snapshot proves it, cited in the
record) reads `pinned`; `concern` (partly or not proven, the gap said) reads `partly`;
`blocker` or `fail` reads `false`; no record reads `no_record`, and `quote_not_found` when
the text is gone. The snapshot reading is kept beside it as `snapshot_status`: `unbacked`
without evidence snapshots, `partly` when a known defect, invented value or
declared-unpinned finding sits on one of its operations or it names an operation nothing
pins (resume), `pinned` otherwise. Change a claim's text and its record drifts (qualifier
review), so wording and status move together.

## The rules (`check`, run by `cargo test -p toolpath-model`)

1. A planned piece (not opened or merged) names only commits on the revision the model read.
2. Every claim has a live qualifier record; one that reads as pinned and declares evidence kinds has snapshot evidence; its quote is still in the file. (`tests/claims.rs` checks the record at the text from the working tree, without regenerating.)
3. Every known defect has a piece or a concern that owns it (names it, names a key it
   renames, or names its kind and its set).

The test also validates `model.json` against `model.schema.json`.

## Limits, said plainly

- Feedback `about` and `resolved_by` are matched from text (harness and defect names in
  the question; commit ids, `pr/NN` and `#NNN` in the replies). Unmatched threads keep
  empty lists rather than guessed links.
- jj change and commit ids in `stack.json` belong to the clone and operation that cut the
  stack (pr/README.md); the model carries them with that note.
- The model is regenerated by hand today; nothing fails when it is stale, only when it
  breaks a rule. `path develop model` is where the command belongs once `path develop`
  lands.
