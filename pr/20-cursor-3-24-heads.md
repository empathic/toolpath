# fix(cursor): read Cursor 3.24's composerHeaders table; write a row there too

Status: planned (asked 2026-10-19 on the goldens demo deck: "do we have a fix for these two? … separate prs in jj for each") · Base: main (independent of the goldens stack and of pr/19 gemini)

## Why
The transformation graph drew "derive cursor" red (broken, inferred): Cursor 3.24 migrates
`ItemTable.composer.composerHeaders` into a `composerHeaders` table at launch (`cursorDiskKV`
`composer.composerHeaders.migratedToTable = 1`) and deletes the key, and the reader only read the key. Confirmed
2026-10-19 against a copy of a real Cursor 3.24.12 `state.vscdb` (the resume-matrix baseline backup: 2 table rows, no
ItemTable key): `list_composer_ids` returned 0 heads at 45cb679 and 2 with this change (recency order). Both heads in
that DB are empty drafts, so a real 3.24 conversation with bubbles was not available; the synthetic 3.24-shaped
fixture covers that.

## What changes
- `crates/toolpath-cursor/src/reader.rs`: `read_composer_headers` reads the `composerHeaders` table (when present,
  most recent first) and then fills in heads from the legacy key that the table does not hold; `HEADERS_TABLE`
  constant. Tests: a DB in the 3.24 shape (schema exactly as 3.24.12 creates it, key deleted, migration flag set)
  lists and loads the composer; table rows win over a stale blob copy and the blob fills in a head written after the
  migration; neither present is empty. The existing tests cover the ≤ 3.6 shape.
- `crates/path-cli/src/projection/cursor.rs`: when the table exists, the projected head is also upserted as a row
  (`workspaceId` from the head, `recency` = `lastUpdatedAt`); the legacy key is still written, since Cursor 3.24 was
  seen to migrate and list it. Tests: projection into both shapes read back through `DbReader`.
- Qualifier: a `pass` on the reader with the real-DB result; a `concern` (declared-limit) that a row written
  directly has not been checked against a live Cursor.app.
- README harness table, `site/_data/crates.json`, `docs/agents/formats/cursor.md`: the reader reads both layouts.
- Goldens unchanged: the cursor goldens load `test-fixtures/cursor/convo.json`, not a database.

## Verification
fmt (cargo + prettier over site/), clippy `--workspace --all-targets --features path-cli/goldens -D warnings`,
`RUSTFLAGS=-D warnings cargo test --workspace --features path-cli/goldens` (79 suites ok), doc, shellcheck.
Real DB: `HOME=<tmp with a copy of the backup> cargo run -p toolpath-cursor --example dump_fixture` lists nothing
(panics "at least one composer") before, `empty-state-draft` and `394f0cb6-…` after.

## Revset
```
e0415cd fix(cursor): read Cursor 3.24's composerHeaders table; write a row there too
```

## Paths
```
crates/toolpath-cursor/src/reader.rs
crates/toolpath-cursor/src/.qual
crates/path-cli/src/projection/cursor.rs
crates/path-cli/src/projection/.qual
README.md
site/_data/crates.json
docs/agents/formats/cursor.md
```
