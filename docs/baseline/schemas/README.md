# Baseline schemas

The baseline is the set of recordings (real sessions, one per harness and
version) and the snapshots each stage of the pipeline pins for them. Every
snapshot kind has a schema here, or a stated reason it has none.
`crates/toolpath-goldens/tests/schemas.rs` checks every committed snapshot
body against its kind's schema.

A snapshot file is `crates/toolpath-goldens/snapshots/<kind>@<recording>[@<harness>].snap`:
an insta header (`source`, `expression`, `info` with `adapter`, `agent`,
`input_sha256`, and `input_file`) between two `---` lines, then the body
the schema describes.

| kind | schema | how |
|---|---|---|
| `known_defects` | [known_defects.schema.json](known_defects.schema.json) | generated from `model::KnownDefect` |
| `round_trip` | [round_trip.schema.json](round_trip.schema.json) | generated from `model::RoundTrip` |
| `shed` | [shed.schema.json](shed.schema.json) | generated from `model::Shed` |
| `validate` | [validate.schema.json](validate.schema.json) | generated from `model::Validate` |
| `writer` | [writer.schema.json](writer.schema.json) | generated from `model::RoundTrip` |
| `document` | [document.schema.json](document.schema.json) | the format's own: `schema/toolpath.schema.json` plus the agent-coding-session v1.1.0 kind; checked the way `path p validate` checks it |
| `formats` | [formats.schema.json](formats.schema.json) | hand-written (body built with `json!`) |
| `validate_transforms` | [validate_transforms.schema.json](validate_transforms.schema.json) | hand-written (body built with `json!`) |
| `opencode_db` | [opencode_db.schema.json](opencode_db.schema.json) | hand-written (body built with `json!`) |
| `projection` | none | harness-native files; see [docs/agents/formats/](../../agents/formats/README.md) |
| writer contract (`fixtures/contract/<agent>.json`) | [transform.schema.json](transform.schema.json) | generated from `transform::Contract`, the writer transform IR (`tests/contract.rs` keeps it current) |
| `ir` | none | a `toolpath-convo` `ConversationView`, which has no JSON Schema yet |

Recording provenance (`crates/toolpath-webmock/cassettes/<harness>/provenance.json`)
is checked against [provenance.schema.json](provenance.schema.json).

Not covered here: `goldens/manifest.json` and the `goldens/roundtrip/` and
`goldens/shed/` files, which `path goldens` in `path-cli` writes with
`json!`; and the `path-cli` snapshots (`query@`, `core_types@`, `help@`,
`behaviour@`), which belong to that crate.

Regenerate the generated schemas after changing a model type:

```sh
UPDATE_SCHEMAS=1 cargo test -p toolpath-goldens --test schemas
```
