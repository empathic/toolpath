# docs: development starts from the baseline; the unreleased changelog entry

Status: planned (asked 2026-10-19: "I want the goldens to drive development") · Base: the goldens stack once merged ·
Docs only; independent of pr/19–37 (it describes what they landed)

## Why
DEVELOPMENT.md described the gates and the format notes but not how a change is made: nothing said to state a change
as a baseline difference first, how to accept a loss on purpose, or what a piece record cites. CHANGELOG.md had no
entry for any of the goldens work. The baseline also builds on Ben Barber's spring work (the elicited recordings,
7a51195; the translation matrix and real-fixture round trips, #81; `ConversationView`, #86; schema-conformant actors,
#87; `meta.kind`, #85), which the docs never credited.

## What changes
- `DEVELOPMENT.md`: rewritten around a baseline-first loop (state the change as a difference: a new recording, an
  expected loss or gain going away, or a regression case; run the baseline and read the named diff; fix, or accept on
  purpose with the snapshot, the declared loss and a qualifier record in one change; the piece record cites the
  snapshots; `just ci`). Words: recording, snapshot, baseline (`goldens` as the older name), stage, contract. "Where
  this comes from" credits the foundation above and says what the baseline adds. Recipes for adding a harness,
  changing a reader or writer, changing the kind (v1.1.0 only; a new version is a new URI), examples, the object
  model, qualifier records; the three things the baseline does not reach; the gate list now includes `quickstart`
  and `secrets`. Commands are today's (`cargo test -p toolpath-goldens`, `scripts/goldens.sh check`); `path d
  baseline` / `just baseline` are named as landing, not used.
- `CHANGELOG.md`: one unreleased section, "The baseline: every recording through every stage", in the file's style,
  listing what is on wip/bdelanghe as of this piece (pr/01–37, the pi provider and the Cursor history pieces): the baseline crate and its stages, the `goldens`
  feature and the coming `path d baseline` rename, the fixes the baseline found, webmock and `no_secrets`, the mock
  harness, the signature algorithm behind `verify`, the tests of the docs' promises, the qualifier gate in CI and the
  claims records, the object model, kind v1.1.0 only.

## Not done here, and why
- `scripts/goldens.sh` stays: it is the only short form until the `develop` group (`path d`) and its `just` recipes
  land; that piece removes it or points it at the recipe.
- The `examples` gate stays: `examples/` is still hand-written (generating it from producer runs is in progress); the
  gate is what keeps them valid until then.

## Verification
- Docs only. Every command and file named exists at this tip (`scripts/quality_gates.sh` `_all_gates`, the
  snapshot kinds under `crates/toolpath-goldens/snapshots/`, `crates/path-cli/kinds/agent-coding-session/`,
  `toolpath/Cargo.toml` `verify`); the PR numbers and commits cited resolve (`gh pr view 81 85 86 87`, `git log
  7a51195`).

## Paths
```
DEVELOPMENT.md
CHANGELOG.md
pr/39-baseline-first-development.md
```
