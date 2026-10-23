# test(baseline): a JSON Schema per snapshot kind; every committed snapshot checked against its kind

Status: planned (asked 2026-10-19: "could we create a json schema for the goldens") · Base: the goldens stack once merged (`crates/toolpath-goldens`) · Independent of the other pieces: it adds a test and docs, and derives `JsonSchema` on the crate's own model types; no snapshot changes

## Why
The baseline (the recordings, and the snapshot each stage pins for them) had no stated shape. A snapshot kind
could change what it records — a field renamed, a variant added, a count turned into a list — and the only signal
was a snapshot diff someone accepted. A schema per kind states the shape once, and checking every committed
snapshot against it means an accepted diff can no longer change the shape silently.

## What changes
- `crates/toolpath-goldens/src/schemas.rs` (new): the generated schemas, from the model types with `schemars`
  (0.8, already in `Cargo.lock`): `known_defects` (`Vec<KnownDefect>`), `round_trip` and `writer` (`RoundTrip`),
  `shed` (`Shed`), `validate` (`Validate`). `src/model.rs` and `src/keypaths.rs` derive `JsonSchema`; the two
  harness-keyed maps serialised with `by_agent_name` declare string keys.
- `docs/baseline/schemas/`: the five generated schemas; hand-written `formats`, `validate_transforms` and
  `opencode_db` (their bodies are built with `json!`); `document` (points at `schema/toolpath.schema.json` plus the
  agent-coding-session v1.1.0 kind); `provenance` for `crates/toolpath-webmock/cassettes/*/provenance.json`; and a
  README listing every kind, how its schema is made, the insta header shape, and what is not covered.
- `crates/toolpath-goldens/tests/schemas.rs` (new):
  - `generated_schemas_are_current`: the committed generated schemas equal what the types produce
    (`UPDATE_SCHEMAS=1 cargo test -p toolpath-goldens --test schemas` regenerates).
  - `every_snapshot_matches_its_kind`: every `snapshots/<kind>@….snap` body validates against its kind's schema;
    `document@` bodies are checked the way `path p validate` checks them (`toolpath_goldens::validate`).
  - `recording_provenance_matches`: every recording's `provenance.json` validates.

## Not covered (stated in the README)
- `projection@`: harness-native files; their shape is `docs/agents/formats/<harness>`.
- `ir@`: a `toolpath-convo` `ConversationView`, which has no JSON Schema — qualifier concern `baa7a7b5` on
  `tests/schemas.rs`. Closing it means deriving `JsonSchema` in the shared crate; its own piece.
- `goldens/manifest.json`, `goldens/roundtrip/`, `goldens/shed/` (written by `path goldens` in path-cli with
  `json!`) and the path-cli snapshots (`query@`, `core_types@`, `help@`, `behaviour@`).

## Findings
Every committed snapshot of a schema'd kind validates against its schema, and all seven recordings' provenance
files validate. No finding.

## Verification
- `cargo test -p toolpath-goldens --test schemas`: 3 passed.
- Gates (`scripts/quality_gates.sh` with `--features path-cli/goldens,toolpath/verify`): see the commit.

## Paths
- `crates/toolpath-goldens/Cargo.toml`, `Cargo.lock`
- `crates/toolpath-goldens/src/{lib.rs,model.rs,keypaths.rs,schemas.rs}`
- `crates/toolpath-goldens/tests/{schemas.rs,.qual}`
- `docs/baseline/schemas/*`
- `pr/41-baseline-schemas.md`
