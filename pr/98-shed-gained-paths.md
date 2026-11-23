# feat(goldens): the shed reports list the gained paths, not only the lost ones

Status: draft · Base: pr/101 · Source: deck v52 item 5 (item 4: see below)

## Why
The shed snapshots (and goldens/shed/*.json) named every path a same-format round trip LOST, with an example, but only counted the ones it GAINED (`key_paths.gained`). Reading what a writer invents
(codex/claude `message.content`, pi's `message.content`, ...) needed a local dump patch. The names are already computed; they were thrown away.

## What changes
- `SameFormat.gained: Vec<Dropped>` (toolpath-goldens model.rs, shed.rs): path plus one example taken from the projection, withheld/clipped by the same `example_value` rule as `dropped`.
- The `path d baseline shed` JSON (path-cli goldens.rs) writes the same `gained` array.
- Regenerated: the shed snapshots, docs/baseline/schemas/shed.schema.json, goldens/shed/*.json, goldens/manifest.json. One qualifier concern re-anchored (goldens.rs, lines moved).

## Verification (sandbox, cargo 1.97.1, offline)
- Test first: `a_dropped_field_a_rename_and_a_shed_event_type_are_each_reported` now expects `gained == [message.stopReason: "end"]` and `gained.len() == key_paths.gained` (did not compile before; passes now).
- toolpath-goldens tests (schemas current), `path-cli --features develop --test goldens` (baseline check), qualifier review 238 fresh, fmt, clippy -D warnings (workspace, all targets, nix-store clippy 1.97.1), workspace tests (only the known sandbox port-bind failures).
- The check still compares DROPPED sets only; a newly gained path is not made a failure here (the round-trip check already pins gained paths per set). Making new gained paths a failure is a separate decision.
- NOT run: pinned 1.94, gitleaks, nix.

## Item 4 (not a change)
The qualifier concern on the cut claim `format-agents-majority` (52b17c0a) was already resolved by 6f455f54 (reason fixed: the sentence was reworded). Nothing to retract in the records.
docs/model/model.json still lists the claim because it is generated and was last generated before that resolve; regenerating needs a pathbase checkout and the clax feedback exports, which this seat does not have: scratch-main-82 regenerates it.
