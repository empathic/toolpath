# feat(model): the goldens work as one generated object model

Status: planned (asked 2026-10-19 on the goldens demo deck: "build out all the context as a json one cohesive object model based on the actual repo and the jj patches … the feedback changes not just this slide but how our code and prs are written") · Base: the goldens stack once merged · Independent of pr/17–22

## Why
The goldens work is spread over snapshots, known-defect pins, qualifier records, `pr/` records,
the site's claims and review threads, and nothing ties them together. A review comment lands on a
slide and stops there. One model, generated from the repositories, gives every object an id and a
source, so feedback, code and PR records point at the same things and the model can check them
against each other.

## What changes
- `crates/toolpath-model` (new, `publish = false`): reads toolpath (and pathbase) at a revision
  through `git` and emits `docs/model/model.json`: 7 harnesses, 4 operations, 9 sets, 194
  snapshots, 2817 findings (2638 losses, 143 gains, 10 invented values, 3 renames, 11 share-side
  changes, 5 known defects, 7 declared-unpinned), 6 claims, 32 pieces, 104 concerns, 29 feedback
  threads (read at 79003bc and pathbase 1d5bc39). Gains whose value is the same constant in three or more projections are marked
  invented values (`permissionMode: "default"`, `model_provider: "openai"`, …).
- `docs/model/claims.json`: the site's claims as quotes plus the operations and snapshot kinds that
  evidence them. Status is computed (today: 4 partly, 2 unbacked, 0 pinned).
- `docs/model/model.schema.json`, `docs/model/README.md` (prior-art table, object types, rules,
  limits), `docs/model/feedback/goldens-demo.threads.json` (the deck's threads, exported from Clax).
- `check()`: a planned piece names only commits on the branch; a pinned claim has snapshot evidence
  and its quote is still on the site; every known defect has an owning piece or concern. The test
  runs it, plus the schema, over the committed model.

## Verification
- `cargo test -p toolpath-model`: 4 unit tests (snapshot names, headers, commit ids and values,
  one case per rule) and the committed-model test (schema + rules), green.
- The first generation broke rule 3 for three defects (claude-roundtrip-renames,
  copilot-session-totals-not-on-steps, opencode-total-usage-drops-breakdowns) because ownership was
  matched by name only; the concerns that own them name the renamed key or the defect's kind and
  set, and the match now accepts those. All are owned (five after pr/22 retired caller-cwd).

## For the next piece
`path develop model` once `path develop` lands; a deck generated from the model; regenerate in the
piece that changes what it describes.

## Paths
```
Cargo.toml
Cargo.lock
crates/toolpath-model/
docs/model/
```
