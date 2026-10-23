# test(goldens): pin the opencode database read and codex's absolute rollout_path; declare the rest

Status: planned (asked 2026-10-19 on the goldens demo deck: "pin the ones we haven't pinned yet") · Base: the goldens stack once merged · Pairs with pathbase pr/09-document-uploads

## Why
The transformation graph (review/graph/transformations.md) drew six edges grey (unpinned). Three can be pinned
hermetically; three cannot today, and each now says why where the code is.

## What changes
- **derive opencode, the database read** (was: no test opens a real `opencode-stable.db`).
  `crates/toolpath-goldens/tests/opencode_db.rs` stores `test-fixtures/opencode/convo.json` as opencode 1.18.18
  stores a session (`test-fixtures/opencode/schema-1.18.18.sql`: the four tables the adapter reads, schema only, from a
  real `opencode-stable.db`), resolves the file through `PathResolver`'s channel rule, reads it with `ConvoIO`, and
  compares the IR to the export read. `snapshots/opencode_db@opencode.snap`: resolved `opencode-stable.db`, 12
  messages, 45 parts, no IR difference.
- **append after a real turn** (was: nothing; drawn for codex). `crates/path-cli/tests/codex_home_copy.rs` resumes the
  `codex` golden into a temp home whose `state_5.sqlite` has codex-cli 0.162.1's threads table
  (`test-fixtures/codex/state_5-threads-0.162.1.sql`), copies `CODEX_HOME`, and pins that the copy's
  `threads.rollout_path` still names the original rollout (`tests/snapshots/codex_home_copy__rollout_path.snap`): a
  known defect, with a qualifier concern on `register_codex_thread`. The other agents have no such indirection
  (their resume targets are self-contained under their home), so there is no append check to add for them.
- **upload for gemini, opencode, cursor inputs**: pathbase's side, pinned there (pathbase pr/09) as the documents
  `path share` uploads.
- **Declared unpinned**, qualifier concerns tagged `declared-limit`, `unpinned`: resume into gemini-cli (no answered
  model turn: Google login refused, API key runs 429 / stream cut), resume into Cursor.app (body never shown without
  a deeplink), the live pathbase stored row (webmock layer 10).
- `crates/toolpath-goldens` gains `rusqlite` as a dev-dependency.

## Verification
`cargo test --workspace --features path-cli/goldens` with the new tests; fmt, clippy (`--all-targets -D warnings`,
with the feature), doc, shellcheck as `scripts/quality_gates.sh` runs them.

## Paths
```
crates/toolpath-goldens/Cargo.toml
crates/toolpath-goldens/tests/opencode_db.rs
crates/toolpath-goldens/snapshots/opencode_db@opencode.snap
test-fixtures/opencode/schema-1.18.18.sql
crates/path-cli/tests/codex_home_copy.rs
crates/path-cli/tests/snapshots/codex_home_copy__rollout_path.snap
test-fixtures/codex/state_5-threads-0.162.1.sql
crates/path-cli/src/projection/.qual
crates/toolpath-webmock/cassettes/.qual
Cargo.lock
```
