# ci: every PR's checks tell its story — the record's claims proved by named tests, new tests red at the base and green at the head, one goldens line per set

Status: ready · Base: pr/118 (needs its `gates.yml` matrix) · Source: Bobby 2026-10-28 ("do these ci checks tell a story about our changes? a TDD BDD clarity in the contract." → "yes do it")

## Why
Run 38840304621 (#360) was green and said nothing about #360: two checks, `ci` (one 385 s "Quality gates" step, ~600 tests in
one log) and `deploy` (a site preview). pr/118 splits the gates into named jobs; that says *which gate* broke, still not *what
this PR claims and which test proves it*, nor whether the new tests prove the change at all (a new test that already passes
without the change proves nothing). The goldens were one test, `goldens ... ok`, for every harness.

## What changes
- `cargo xtask contract` (crates/xtask/src/contract.rs): finds the PR record for the head branch (`pr/<branch>.md` in the tree,
  else on `origin/wip/bdelanghe`, where the records live), reads its `## Proves` section (`- <claim> — <file>::<test>, …`),
  runs each named test in the binary that holds that file, and writes claim → test → result to `$GITHUB_STEP_SUMMARY`. A claim
  with no test, a missing test or a failing test fails the job. No PR, or no record: says so and passes.
- `cargo xtask red-green` (same file): the tests the change adds (head `--list` minus base `--list`), green at the head; for a
  new integration test (`tests/*.rs`), the base built with only that file taken from the head: red (fails or does not compile)
  is the evidence; green there is flagged "proves nothing new" (warning). A new test inline in a changed source file is listed
  as such (it cannot run without the change).
- Both build with the features the tree's own test gate uses (read from `scripts/quality_gates.sh`), so an older base builds
  as its CI did.
- `scripts/quality_gates.sh`: gates `contract` and `redgreen`; `gates.yml`: matrix jobs `gates (contract)` and
  `gates (red-green)` (history for red-green, the records fetched for contract, `PR_BASE_SHA` from the event, qualifier
  installed for both since the goldens may be among the named tests).
- `crates/path-cli/tests/goldens.rs`: one test per golden set (`goldens::claude`, `goldens::codex`, …) via `Goldens::check(Some(set))`,
  plus `every_manifest_set_has_a_test` (fails when goldens/manifest.json gains or loses a set the list does not name).
- `## Proves` backfilled for pr/01 (#359), pr/02 (#360), pr/06 (#361) from the tests their diffs add.

## Proves
- A record's Proves section parses into claims and test refs; a claim may have none — crates/xtask/src/contract.rs::proves_section_parses_claims_tests_and_claims_without_tests
- The record is the head branch without its `pr/` prefix, never a path — crates/xtask/src/contract.rs::record_is_the_branch_without_its_pr_prefix_and_never_a_path
- A missing test, a failing test and a claim without tests each fail the contract — crates/xtask/src/contract.rs::contract_fails_on_a_missing_test_a_failing_test_and_a_claim_without_tests
- A test result matches by full path or last segment only — crates/xtask/src/contract.rs::results_match_the_full_path_or_its_last_segment_only
- A ref resolves to the integration test, else the lib, else the bin — crates/xtask/src/contract.rs::exe_for_picks_the_integration_test_else_the_lib_else_the_bin
- Added tests are head minus base per binary — crates/xtask/src/contract.rs::added_is_head_minus_base_per_binary
- Features come from the tree's own test gate — crates/xtask/src/contract.rs::features_come_from_the_test_gate_line_or_none
- Every golden set is its own test, and none can be missing — crates/path-cli/tests/goldens.rs::every_manifest_set_has_a_test, crates/path-cli/tests/goldens.rs::claude, crates/path-cli/tests/goldens.rs::codex

## Verification
Run locally (`nix develop`) with the built xtask against each PR head, records read from this branch (`RECORDS_REF`), each head
in its own target dir:
- **pr/01 (#359)** contract: 2/2 claims ✅. red-green vs `origin/main`: 2 added (both inline in `toolpath-convo`), 🟢 at head.
  Note: the record's *What changes* never mentions `Turn::is_content_empty`, which the slice adds (the Proves line now names it).
- **pr/02 (#360)** contract: 7 claims, 24 tests, all ✅ — after the first run caught a stale binary (see below). red-green vs
  `origin/pr/01-convo-breakdowns`: the 6 integration tests (`tests/goldens.rs`, `tests/goldens_producer.rs` ×5) are 🔴 "does not
  compile without the change" at the base and 🟢 at the head; the 18 inline tests are listed as inline, 🟢 at head.
- **pr/06 (#361)** contract: 5/5 ✅. red-green vs `origin/pr/02-goldens-core`: 5 added (all inline), 🟢 at head.
- `cargo test -p xtask` (13, of which 7 contract.rs), clippy `-D warnings` clean; `cargo test -p path-cli --features develop --test goldens`
  on this branch: 7 (six sets + `every_manifest_set_has_a_test`), 16 s.

Found while building it: two checkouts of the same workspace must NOT share a target dir — cargo's fingerprints are keyed by
package, so the second checkout silently got the first one's test binaries (pr/02's contract first reported its
`test_projector_skips_empty_text_seed…` as "no such test" because the toolpath-claude test binary was pr/01's). red-green builds
the base in `target/red-green-target` for that reason, and its base worktree path is fixed because build scripts (pathbase-client's)
bake `CARGO_MANIFEST_DIR` in. Limit: a new test inline in a changed source file cannot be shown red without the change; the table
says so instead of claiming it.

## Checklist
1. [x] **Independent PR** — no bundled or speculative changes
   - [x] Verified: CI contract/red-green (xtask + gates), goldens per set, Proves backfill for 01/02/06 only
2. [x] **Changed codepaths verified** — targeted unit and full integration tests
   - [x] Verified: `cargo test -p xtask` (13; 7 in contract.rs); `cargo test -p path-cli --features develop --test goldens` (7: six sets + manifest); contract and red-green run on the pr/01, pr/02, pr/06 heads (tables above)
3. [x] **Root cause identified** — the checks named gates, never the PR's claims or the evidence its tests prove the change
4. [x] **No duplication** — one module for both verbs; the gates matrix reused (caching, qualifier install), not a new workflow
5. [x] **No unrelated changes** — N/A

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`). `scripts/quality_gates.sh` and `gates.yml` hunks are assigned
per hunk (pr/15 and pr/118 own those files):
```
crates/xtask/src/contract.rs
crates/xtask/src/main.rs
crates/xtask/Cargo.toml
crates/path-cli/tests/goldens.rs
pr/120-ci-contract.md
```
