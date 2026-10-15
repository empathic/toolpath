# fix(opencode): resume writes the database opencode actually reads

Status: ready · Base: pr/02-goldens-core · Independent of the other resume fixes

Commits: `fix(opencode): …` (4b2d71c).

## Why
`path resume --harness opencode` wrote `opencode.db`; opencode 1.18.18 reads `opencode-stable.db` and answered "Session not found". Found by the resume matrix's harness inventory (`review/resume-matrix-v3.md`).

## What changes
- The rule, decoded from the 1.18.18 bundle rather than guessed: opencode resolves `$OPENCODE_DB` first; `opencode.db` only on the latest/beta/prod channels or with `$OPENCODE_DISABLE_CHANNEL_DB`; otherwise `opencode-<channel>.db`, so `opencode-stable.db` for a release build. It never reads the legacy file.
- `toolpath-opencode` `PathResolver` mirrors it: `$OPENCODE_DB` / `with_db_file` pins; else candidates `[opencode-stable.db, opencode.db]`, first existing, else stable. Same path for resume (write) and derive (read). The two files are deliberately not synced. Documented in `paths.rs` and `docs/agents/formats/opencode.md` ("Database file name").
- 8 path-resolution unit tests; README, `cmd_export` and test-support prose updated.

## Verification
`cargo test -p toolpath-opencode -p path-cli` green; fmt and clippy clean; `goldens check` 46 ok (opencode is not a goldens target; manifest unchanged). End to end in a hermetic HOME with no credentials: `opencode session list` created `opencode-stable.db`, the fixed resume wrote 1 session / 6 messages / 11 parts there and no `opencode.db`, and `opencode --session <id>` under understudy rendered the full history (`resume-matrix/screens/v3-codex-to-opencode-fixed.*`).

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
crates/path-cli/src/cmd_export.rs
crates/path-cli/src/providers.rs
crates/path-cli/tests/support/mod.rs
crates/toolpath-opencode/README.md
crates/toolpath-opencode/src/paths.rs
docs/agents/formats/opencode.md
```
