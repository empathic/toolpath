# test(path-cli): hermetic goldens over the committed fixtures (`path goldens capture|check|diff|list`)

Status: opened #360 (draft, 2026-10-16) — was: draft · Base: main · Stack 1/4

Commits, in wip order: `test(path-cli): hermetic goldens for codex<->claude over real fixtures…` · `test(path-cli): claude-origin golden from the compacted fixture…` · `feat(path-cli): path goldens (capture/check/diff/list/init/capture-claude)…` · `feat(goldens): copilot/pi goldens, caller-cwd known defect, sha-pinned manifest…` · `goldens: repo_rev 'code differs' counts only code; toolpath-golden fixtures crate…` · `fix(goldens): only the input and the outputs can fail a set` · its repin.

## Why
Every adapter's projection is a transformation nobody re-checks: a dropped key, a renamed field or lost text ships silently. A golden is a committed input plus the exact bytes each projection produces from it; `check` re-runs the projections in a hermetic child (empty env, temp HOME, cwd `/`) and fails with the key and the target that moved.

## What changes
- `path goldens capture <agent> <fixture>` / `check [set]` / `diff <set>` / `list` in `path-cli`; `scripts/goldens.sh` wraps `cargo run`.
- `goldens/<set>/` per committed fixture (claude, claude-compacted, codex, copilot, pi) with each projection's output; `goldens/manifest.json` pins the command, the input sha and each output sha. The toolchain, lockfiles, goldens source and the captured rev are recorded as `environment` and reported as `info` when they move; only input and output bytes can fail a set.
- `toolpath-golden`: a tiny fixtures crate (dev-dependency) so tests read captured sessions instead of hand-typed JSON.
- Nothing from the developer's machine enters a golden (no home paths; a nix store path is recorded, a dev build's path is not).

## Proves
- Every committed golden set re-runs hermetically and matches its pinned bytes — crates/path-cli/tests/goldens.rs::goldens
- A golden's producer moving (store binary, dev build, rev) is information, never a silent pass or a spurious failure — crates/path-cli/tests/goldens_producer.rs::capture_pins_the_running_binary_and_rev, crates/path-cli/tests/goldens_producer.rs::a_moved_dev_build_is_information_never_a_failure, crates/path-cli/tests/goldens_producer.rs::moved_store_binary_is_a_problem_unless_allowed, crates/path-cli/tests/goldens_producer.rs::a_golden_without_a_producer_is_information_not_failure, crates/path-cli/tests/goldens_producer.rs::a_moved_rev_is_information_whether_or_not_the_code_changed
- A newly dropped key or lost text fails with the key that moved; only NEW losses fail — crates/path-cli/src/goldens.rs::shed_names_a_newly_dropped_path_and_lost_text, crates/path-cli/src/goldens.rs::roundtrip_fails_on_a_newly_gained_key_path, crates/path-cli/src/goldens.rs::roundtrip_row_counts_losses_and_flags_only_new_ones, crates/path-cli/src/goldens.rs::known_rename_gone_is_fixed_only_if_the_source_name_is_written
- Nothing from the developer's machine enters a golden: secrets are refused, identifiers redacted to stable placeholders — crates/path-cli/src/goldens.rs::credential_plain_routes_by_prefix, crates/path-cli/src/goldens.rs::credential_hex_with_encoded_newline_is_decoded, crates/path-cli/src/goldens.rs::credential_bad_prefix_is_refused_without_the_value, crates/path-cli/src/goldens.rs::email_detection, crates/path-cli/src/goldens.rs::username_is_matched_as_a_whole_word, crates/path-cli/src/goldens.rs::credentials_split_into_secrets_and_account_identifiers, crates/path-cli/src/goldens.rs::identifiers_are_redacted_to_stable_placeholders_not_refused, crates/path-cli/src/goldens.rs::leak_check_refuses_secrets_and_other_addresses_but_not_the_attribution_address, crates/path-cli/src/goldens.rs::live_commands_keep_prompt_and_project_but_no_credentials
- Goldens compare in a canonical form (sorted keys, masked session id) — crates/path-cli/src/goldens.rs::canonicalize_sorts_keys_and_masks_the_session_id, crates/path-cli/src/goldens.rs::base64_matches_known_vectors
- Tests read captured sessions, not hand-typed JSON (`toolpath-golden`) — crates/toolpath-golden/src/lib.rs::slices_and_mutates_captured_bytes, crates/path-cli/src/goldens.rs::golden_factory_slices_and_mutates_captured_bytes
- The Claude projector drops an empty streaming-seed assistant line and relinks the chain — crates/toolpath-claude/src/project.rs::test_projector_skips_empty_text_seed_and_relinks_chain

## Verification
`cargo test -p path-cli --test goldens` green on a depth-1 clone (CI's checkout shape, and what a squash merge leaves on main); `check`: 46 ok. Regressions it catches, shown by a throwaway edit then reverted: dropping `user_type` in `toolpath-claude/src/provider.rs` fails with `newly dropped … userType`.

## For the next PR
Round trips, shed and known defects (03) build on the manifest written here.

Note (2026-10-15): the jj branch `pr/02-goldens-all` carries the whole goldens series in wip order (41 commits incl. `fix(claude): drop empty streaming-seed assistant lines on projection`, which the goldens depend on, and `fix(goldens): goldens.sh runs cargo directly when there is no flake`). Splitting it into 02–05 by feature needs `goldens.rs` split into files first; see HOLD.md and the insta spike.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs,
a file touched by several PRs belongs to the earliest):
```
glob:"crates/toolpath-claude/src/**"
glob:"crates/toolpath-golden/**"
glob:"goldens/**"
.gitattributes
CLAUDE.md
Cargo.lock
Cargo.toml
crates/path-cli/Cargo.toml
crates/path-cli/src/.qual
crates/path-cli/src/goldens.rs
crates/path-cli/src/lib.rs
crates/path-cli/tests/goldens.rs
crates/path-cli/tests/goldens_producer.rs
crates/path-cli/tests/roundtrip.rs
crates/toolpath-codex/src/.qual
crates/toolpath-copilot/src/.qual
docs/GOLDENS.md
scripts/goldens.sh
```
