# test(path-cli): hermetic goldens over the committed fixtures (`path goldens capture|check|diff|list`)

Status: draft · Base: main · Stack 1/4

Commits, in wip order: `test(path-cli): hermetic goldens for codex<->claude over real fixtures…` · `test(path-cli): claude-origin golden from the compacted fixture…` · `feat(path-cli): path goldens (capture/check/diff/list/init/capture-claude)…` · `feat(goldens): copilot/pi goldens, caller-cwd known defect, sha-pinned manifest…` · `goldens: repo_rev 'code differs' counts only code; toolpath-golden fixtures crate…` · `fix(goldens): only the input and the outputs can fail a set` · its repin.

## Why
Every adapter's projection is a transformation nobody re-checks: a dropped key, a renamed field or lost text ships silently. A golden is a committed input plus the exact bytes each projection produces from it; `check` re-runs the projections in a hermetic child (empty env, temp HOME, cwd `/`) and fails with the key and the target that moved.

## What changes
- `path goldens capture <agent> <fixture>` / `check [set]` / `diff <set>` / `list` in `path-cli`; `scripts/goldens.sh` wraps `cargo run`.
- `goldens/<set>/` per committed fixture (claude, claude-compacted, codex, copilot, pi) with each projection's output; `goldens/manifest.json` pins the command, the input sha and each output sha. The toolchain, lockfiles, goldens source and the captured rev are recorded as `environment` and reported as `info` when they move; only input and output bytes can fail a set.
- `toolpath-golden`: a tiny fixtures crate (dev-dependency) so tests read captured sessions instead of hand-typed JSON.
- Nothing from the developer's machine enters a golden (no home paths; a nix store path is recorded, a dev build's path is not).

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
