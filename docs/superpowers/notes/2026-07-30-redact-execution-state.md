# `path p redact` execution state

Working state of the parallel implementation on branch `evan/redact`.
Written as a checkpoint so the work is recoverable without the
orchestrator's transcript. Delete this file when the branch merges.

## Committed

| Commit | Contents |
|---|---|
| `34a36c0` | T0 shared vocabulary |
| `1e456df` | T1 span normalisation, T4 transforms, T6 CLI args, T12 docs |
| `98d51c3` | T2 field map, T5 plan machinery, T3 detector |
| `39ce229` | T7 apply, T8 plan generation, T10 sync replay |

At `39ce229`: `toolpath-redact` 140 tests, `path-cli` 379 lib + 56
integration, all green. `toolpath-redact` is clippy-clean.

## Not implemented

- T9 CLI dispatch was `todo!()` at `39ce229`; an agent has since written
  ~860 lines of it, uncommitted.
- T11 end-to-end integration tests.
- `src/exec.rs`, the subprocess detector. Cut deliberately.
- T3 step 3.6 checksum validators. Cut deliberately.

## Standing constraints from the user

1. **Do not fix pre-existing lint issues.** `cargo clippy --workspace --
   -D warnings` and `cargo fmt --check` are red on findings that predate
   this branch (`cmd_list.rs` 370/390/542/563, `cmd_import.rs` 583/774,
   `toolpath-pi/src/reader.rs:359`, rustfmt drift in `toolpath-codex`).
   An earlier commit fixing these was reverted on request.
2. **Do not change behavior outside `toolpath-redact`.** The known
   un-redaction paths are documented, not fixed. See
   `2026-07-30-redaction-known-gaps.md`.
3. Agents summarize and hand off at 256k context, hard stop at 500k.

## Open review findings not yet applied

From the T3 review, being remediated:

- `BASE_SCORE` 0.6 against a default threshold of 0.8 means nothing
  redacts unless a hotword is within the window. All five of the plan's
  own true-positive fixtures score 0.600 and are skipped.
- `secretGroup` is not deserialized, so the wrong span is redacted for
  `sonar-api-token`, `microsoft-teams-webhook`, `jwt-base64`.
- `mask_existing_markers` substitutes NUL, which satisfies `[^\s:@/]`
  and so causes the idempotence break it exists to prevent.
- Diff clipping keeps only the first line of a multi-line secret, so PEM
  key material survives.
- The three excluded rules fail on regex size limit, not dialect. All 221
  compile with `size_limit(64 << 20)`.
- Hotword window is bytes, documented as characters.
- No per-rule keyword gating; all 221 regexes run on any keyword hit.

From the T7 review, being remediated:

- `group_by_field`'s `BTreeMap` orders `/change/X` before pointers
  beneath it, inverting `surfaces()`'s documented contract. A valid plan
  errors with the document already half-rewritten.
- The audit record publishes the pre-redaction artifact key verbatim.
- Signatures are stripped from steps the pass never touched.
- A zero-width span splices a marker into clean text.
- An empty fingerprint key is accepted.
- The record's `at` carries an `extra` segment that does not resolve
  against the serialized step, because `StructuralChange.extra` is
  `#[serde(flatten)]`.

## Loose ends

- An agent wrote a `.env` fixture into the repo root. Values are fake
  (AWS's documented `EXAMPLE` key). It is untracked and **not** in
  `.gitignore`, so `git add -A` would commit it. Stage explicit paths.
- Accept/reject precedence is unsettled between tracks. `sync/engine.rs`
  `policy_decisions` applies `accept` then `reject`. T11's
  `redact_accept_reject_precedence` is the authority; if it disagrees,
  flip `policy_decisions`.
- `path p validate` against redacted output is unverified. T7 could not
  run a real JSON Schema validator, so T11 is the only real schema check
  in the implementation.
