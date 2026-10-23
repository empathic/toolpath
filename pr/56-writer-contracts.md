# test(baseline): each writer's transform declared up front as a small typed IR, checked both ways

Status: planned (ruling 2026-10-19, design consultant: "declared synthesized values + value-aware gains"; refined by Bobby: "can transforms be explicit — is there an IR for transforms? in schemars") · Base: the goldens stack once merged

## Why
`writer@<agent>` pins what each writer, read back by its own adapter, loses and gains — after the fact. Nothing said up
front what a writer is supposed to do, so a pinned loss looked the same whether it was inherent to the target format or a
bug, and an invented value (codex `model_provider: "openai"`, Claude `permissionMode: "default"`) looked like data.
On the deck's map the projection column was red for opencode, gemini and cursor for exactly this reason: losses nobody
had declared.

## What changes
- `crates/toolpath-goldens/src/transform.rs`: a writer contract is a list of ops — `keep`, `drop {why, defect?}`,
  `rename {from, to}`, `move {from, to}`, `synthesize {path, value?, why, defect?}`, `convert {path, how}` (`how` names a
  conversion; it is not code). `serde` + `schemars::JsonSchema`; the JSON Schema is committed at
  `docs/baseline/schemas/transform.schema.json`. schemars only describes the shape; the IR and its check are ours (the
  Cambria idea without the library).
- `fixtures/contract/<agent>.json` for all seven writers (opencode, gemini and cursor first): every measured loss and
  gain on `fixtures/full-view.json` has an op with a reason.
- `tests/contract.rs`: measured lost/gained key paths == what the ops imply, both directions (an op with no effect
  fails); a `synthesize` with a `value` must match every occurrence after the round trip. Negative controls: a changed
  codex default is named with its key and value; an op with no effect and an undeclared loss are named.
- `SYNTHESIZED` in `toolpath-claude`, `toolpath-codex`, `toolpath-pi` `project.rs`: the `(key, value)` defaults each
  writer invents; the test checks each is literally in that writer's projection.
- `contracts.rs` gains the loader rules from `docs/agents/formats/validation.md` (pr/36): codex `timestamp` on every
  line and `session_meta` first; pi version-3 session header first; Copilot `session.start` first. All current
  projections pass.

## Defects found (declared, to fix)
| writer | key path | defect |
|---|---|---|
| claude | `delegations[]` | Claude Code keeps sub-agent work as sidechain sessions; the writer could write them |
| codex | `token_usage.breakdowns.output.reasoning` | codex's token_count has `reasoning_output_tokens`; the writer does not fill it |
| opencode | `token_usage.breakdowns.output.reasoning` | opencode stores `tokens.reasoning` per message; the writer does not fill it |
| opencode | `change.notes.md.raw` | the writer does not write opencode's snapshot repo, so a projected session's file diffs are gone |
| copilot | `environment.working_dir` | Copilot's `session.start` carries the cwd; the writer does not set it |

Everything else lost is inherent to the target format (no stop reason, no cache-write tokens, per-message usage, no
delegation record, toolpath's own producer stamp, …), each with its reason in the contract.

## Verification
- `cargo test -p toolpath-goldens --test contract` (4 tests); the goldens' contract checks over every projection.
- Full gates with `--features path-cli/goldens,toolpath/verify`.

## Paths
```
crates/toolpath-goldens/src/transform.rs
crates/toolpath-goldens/src/contracts.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/src/lib.rs
crates/toolpath-goldens/Cargo.toml
crates/toolpath-goldens/fixtures/contract/
crates/toolpath-goldens/tests/contract.rs
crates/toolpath-{claude,codex,pi}/src/project.rs
docs/baseline/schemas/transform.schema.json
Cargo.lock
```
