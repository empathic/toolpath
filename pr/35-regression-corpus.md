# test(goldens): contracts and a regression corpus from the bugs that shipped

Status: planned (asked 2026-10-19: make the goldens drive development; every fidelity bug in CHANGELOG.md becomes
a golden that fails when the bug comes back) · Base: the goldens stack once merged · Independent of pr/19–34

## Why
CHANGELOG.md records the silent losses that shipped before the goldens existed. Each was found late: by a live
resume against the real API, by a store's UNIQUE constraint, or by reading a token report. The goldens claim "a
red test the same day, naming the field"; until now nothing showed that claim holds for the bugs we actually had.
Running each one against the goldens also showed that several would NOT have failed: insta trims trailing
whitespace (a missing final newline passes), the schema does not require unique step ids, and the projection
snapshots pin bytes, not meaning, so a recapture would accept an output the harness rejects.

## What changes
- `crates/toolpath-goldens/src/contracts.rs` (new): what the harness or the schema rejects, as checks over a
  projection or a document, each naming rule, target, line and field. `trailing-newline` (JSONL projections end
  with `\n`; 2026-07-30), `claude-null-signature` (a thinking block's signature is absent, never `null`;
  2026-07-30, API 400), `claude-empty-error-result` (an errored `tool_result` has content; copilot -> claude, API
  400), `parent-chain` (every `parentUuid`/`parentId` names an earlier entry; pi's main thread has one root;
  pi -> claude, pi -> pi), `opencode-session-id` (`ses_` + 12 hex + 14 base62; Zen 403), `unique-step-ids` (RFC
  "ID Uniqueness"; derive 2026-07-01).
- `crates/toolpath-goldens/src/pipeline.rs`, `model.rs`, `lib.rs`: the pipeline runs the contracts over the
  document, every projection and the same-format output, on the raw form (before the copilot/cursor
  canonicalisation), into `Stages::contracts` (not serialised, not pinned).
- `crates/toolpath-goldens/tests/goldens.rs`: every set must have no broken contract. Accepting a snapshot never
  accepts one.
- `crates/toolpath-goldens/tests/regressions.rs` (new): nine shipped bugs re-introduced as a fault in one stage's
  real output (no production code changed), each required to fail with its field and stage:

| bug (source) | stage | caught by |
|---|---|---|
| Claude thinking `"signature": null` (2026-07-30) | project -> claude | `claude-null-signature` + the projection snapshot |
| JSONL export without a final newline (2026-07-30) | project | `trailing-newline` only (insta trims trailing whitespace; the test asserts the snapshot cannot see it) |
| usage repeated on every content block, ~3x output (2026-06-17) | derive | accounting contract, `output_tokens` |
| Codex cumulative `total_token_usage` per turn (2026-06-17) | derive | accounting contract, `output_tokens` |
| duplicate step ids (2026-07-01) | derive | `unique-step-ids` only (the schema does not require uniqueness) |
| actor outside the base schema (actor validation fixes) | validate | the base schema |
| pi parent chain cut (#361) | project -> pi | `parent-chain` (before: only the projection's bytes) |
| copilot -> claude empty errored tool_result (#361) | project -> claude | `claude-empty-error-result` only |
| opencode session-id shape (#365) | project -> opencode | `opencode-session-id` only |

- Not reached by the goldens, recorded as qualifier concerns on `tests/regressions.rs` with the test that does
  cover each: `.orphaned-*` rotation dropping a segment (#236; `toolpath-claude` `chain.rs`
  `test_orphaned_stem_is_not_a_successor`; sets are single files), the export clobber of a richer local original
  (path-cli behaviour; `cmd_export.rs` `export_claude_refuses_existing_session_without_force`), Cursor's
  `state.vscdb` 3.24 table (the cursor set is a JSON dump; `toolpath-cursor` `reads_heads_from_the_3_24_table`).
  Gemini 0.47 `.jsonl` is caught by construction (the gemini set is a 0.47 stream read through the adapter) and by
  `toolpath-gemini/tests/jsonl_sessions.rs`; the opencode database read by `tests/opencode_db.rs`.
- Qualifier: the five records on `pipeline.rs` and `tests/goldens.rs` whose lines moved are re-anchored with
  `--supersedes` (content unchanged). The round-trip concern had been re-anchored twice from the same parent by two
  pieces landing together; one chain is kept, the other resolved `duplicate`. `qualifier review`: 57 fresh, 0 drifted.

## Verification
- `cargo test -p toolpath-goldens` (goldens, regressions 9/9, writers, opencode_db, qualifier): ok; no current set
  breaks a contract.
- Gates as `scripts/quality_gates.sh` with `--features path-cli/goldens`.

## Paths
```
crates/toolpath-goldens/src/contracts.rs
crates/toolpath-goldens/src/lib.rs
crates/toolpath-goldens/src/model.rs
crates/toolpath-goldens/src/pipeline.rs
crates/toolpath-goldens/tests/goldens.rs
crates/toolpath-goldens/tests/regressions.rs
crates/toolpath-goldens/src/.qual
crates/toolpath-goldens/tests/.qual
pr/35-regression-corpus.md
```
