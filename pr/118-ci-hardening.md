# ci: gate matrix in parallel, release gated on the full gates, concurrency, timeouts, clippy --all-targets, gitleaks in CI, Node 24 actions pinned by SHA, fixed runners, CI and Site workflows

Status: ready · Base: main · Independent (of the code PRs; three tokens are assigned per hunk, see *Shared hunks*) · Source: Bobby 2026-10-26 (approved: concurrency + timeouts, clippy --all-targets, secrets gate in CI, parallel gates, full CI on release, forked PRs skip the deploy); patch: pr/patches/118-ci-hardening.patch

## Why
- **Red runs that were not about the code.** Last 30 runs of *CI + Deploy* (figures from the caller's
  `gh run list`, not re-measured here): 15 failed, every one on a stacked `pr/*` branch whose slice
  does not build on its own (e.g. `cmd_export.rs` calling `cmd_pathbase::resolve_session_url`, a
  HOLD-era function; see README *Shared files are not file-level*). Pushing a stack starts a run per
  branch and per push, and nothing cancels the superseded ones, so the queue fills with runs whose
  result nobody reads.
- **One serial job.** The quality gates ran as one step, ~314 s, before a ~61 s wasm build and the
  deploy. The gates do not depend on each other; the wall time is the slowest group, not the sum.
- **A tag could publish untested binaries.** `release.yml` built and uploaded on any `v*` tag with no
  gate at all. Bobby's rule: full CI on release.
- **Jobs without a timeout** run to GitHub's 6-hour default when something hangs.
- **clippy only saw library and binary code**; tests, benches and examples were never linted.
- **The secrets gate skipped in CI**: no gitleaks on the runner, so `gate_secrets` printed "skipped"
  and passed. Only `no_secrets.rs` (fixture surfaces) ran there.

## What changes
`.github/workflows/gates.yml` (new, `on: workflow_call`): the gates as a 4-job matrix, `fail-fast: false`,
`timeout-minutes: 20` each, each running `scripts/quality_gates.sh --verbose <group>`:

| job | gates |
|---|---|
| `gates (lint)` | format shellcheck clippy secrets |
| `gates (test)` | test |
| `gates (doc)` | doc examples |
| `gates (plugin-site)` | plugin quickstart site |

The four groups name every gate in `_all_gates` exactly once (`quickstart` rides with plugin+site,
`secrets` with lint). One cargo cache key per group (`cargo-<group>-<Cargo.lock hash>`) so jobs
building different artifacts do not race to save one key. The pinned qualifier install moves here and
runs only in the test group (the only consumer, `crates/path-cli/tests/goldens.rs`); a pinned
gitleaks 8.30.1 (`gitleaks_8.30.1_linux_x64.tar.gz`, sha256 `551f6fc8…2470eb`, checked against the
release's checksums file and a local download; the version the flake dev shell has) is installed in
the lint group the same way.

`.github/workflows/deploy-site.yml`:
- `ci` calls `gates.yml`; `deploy` `needs: ci` (all four groups) with `timeout-minutes: 15`.
- `concurrency`: group `<workflow>-<ref>` for `pull_request`, `cancel-in-progress` for
  `pull_request` only. For `push` the group is `<workflow>-<sha>`: with `cancel-in-progress: false` a
  shared per-ref group would still let a newer pending main run *replace* an older pending one, and
  main must never lose a run. Main deploys can still overlap as they do today (no regression).
- **Forked PRs skip the Cloudflare deploy**, stated in the workflow:
  `if: github.event_name == 'push' || github.event.pull_request.head.repo.full_name == github.repository`.
  A fork gets no `CLOUDFLARE_*` secrets, so the deploy could only fail; the fork's gates still run.

`.github/workflows/release.yml`:
- `gates` (calls `gates.yml`, `permissions: contents: read`) runs on the tagged commit; `build`
  `needs: gates`, so no binary is built, let alone uploaded, from a tree that fails any gate.
- `build` and `release` get `timeout-minutes: 30`; `concurrency` group `<workflow>-<ref>-<sha>`,
  never cancelled.

`scripts/quality_gates.sh`:
- `gate_clippy`: `--all-targets`. It surfaced no new warnings on wip `d8a528b9`, so no Rust code changes.
- `gate_secrets`: under CI (`$CI` set) a missing gitleaks **fails** the gate instead of skipping
  (a broken install must not pass silently); locally it still skips with the message. The allowlist is
  pr/15's `.gitleaks.toml` unchanged (it already covers `openapi_sha256` and `_vN`, pr/117's fix).

`just ci` is unchanged: it still runs every gate in one process.

### Added 2026-10-26: Node 24 actions pinned by SHA, fixed runner images, CI and Site split
Source: Bobby 2026-10-26, on the run annotations ("just fix them"; "should we split up the workflow
files"; "it's an odd name for it too"). Every run warned that `actions/cache@v4`, `checkout@v4`,
`setup-node@v4`, `pnpm/action-setup@v4`, `github-script@v7` and `wrangler-action@v3` target Node 20
and are forced onto Node 24, and that `ubuntu-latest` moves to Ubuntu 26 from 2026-10-19.

- **Actions**: each moves to the lowest major whose `action.yml` declares `using: node24` (checked
  per release tag with `gh api`), pinned by commit SHA with the version as a comment:
  checkout v5.1.0, cache v5.1.0, setup-node v5.0.0, pnpm/action-setup v4.4.0, github-script v8,
  wrangler-action v4.1.3, upload-artifact v6.0.0, download-artifact v7.0.0, action-gh-release
  v3.0.3. Lowest Node 24 major rather than latest, to keep the input surface the workflows use.
- **Wrangler**: wrangler-action v4 installs the latest Wrangler 4 when none is named, so
  `wranglerVersion: "4.153.0"` (npm `latest` on 2026-10-26) is set explicitly.
- **Runners**: `ubuntu-latest` → `ubuntu-24.04` (what it pointed at before the Ubuntu 26
  migration; moving to 26.04 is its own change, since it raises the gnu binary's glibc floor),
  `macos-latest` → `macos-26` (what `macos-latest` is today, per actions/runner-images).
- **Split and rename**: `deploy-site.yml` ("CI + Deploy") becomes `ci.yml` ("CI": triggers,
  concurrency, `gates` then `site`) and `site.yml` ("Site", `on: workflow_call`: wasm, site build,
  Cloudflare deploy, preview comment). The fork condition and the deploy permissions sit on the
  caller's `site` job; `secrets: inherit` passes the Cloudflare secrets. Check names become
  `CI / gates / gates (lint)` … and `CI / site / deploy`. The "Restricted Main" ruleset has no
  required status checks (deletion and signatures only), so the rename blocks nothing.
- `flake.nix` and `gates.yml` comments that named `deploy-site.yml` now name the new files.
- `pr/patches/118-ci-hardening.patch` predates this addition (it is cut against pr/15 and pr/25,
  not plain main). `pr/cut.sh` cuts by the fileset below, so the PR picks the addition up; the
  patch file is refreshed at the next cut.

## Shared hunks (per-hunk assignment, README rule)
- The secrets gate itself is **pr/15** (#370): this PR depends on it and does not duplicate it. The
  `secrets` token in the lint group, the gitleaks install step and the fail-closed hunk go with pr/15
  (or land after it).
- `quickstart` in the plugin-site group goes with **pr/32** (the gate does not exist on main).
- The qualifier install step is **pr/25**'s; this PR moves it into `gates.yml`.
- Cut against plain main the matrix is `format shellcheck clippy | test | doc examples | plugin site`;
  `quality_gates.sh` exits 2 on an unknown gate name, so a token without its gate fails loudly.

## Verification of the 2026-10-26 addition (worktree from `origin/wip/bdelanghe` `6edb678f`)
- `actionlint` 1.7.12 (shellcheck 0.11.0 integration): 0 errors in 5 workflow files.
- Every pinned SHA resolved from its tag with `gh api repos/<action>/commits/<tag>`; each tag's
  `action.yml` read back as `using: node24`.
- Not run: any GitHub Actions run (nothing on main yet); the reusable `site.yml` call is checked by
  actionlint only.

## Verification (worktree from `origin/wip/bdelanghe` `d8a528b9`, `nix develop`)
- `actionlint` 1.7.12 (with shellcheck integration): 0 errors in 4 workflow files.
- `shellcheck scripts/quality_gates.sh`: clean.
- `cargo clippy --workspace --all-targets --features path-cli/develop,toolpath/verify -- -D warnings`: clean.
- The four groups run as CI would, one after another on one machine (shared target dir, so later
  groups reuse earlier builds): lint 4/4 (15.5 s), test 1/1 (196 s), doc+examples 2/2 (60 s),
  plugin+quickstart+site 3/3 (82 s). `just ci`: 10/10 passed, 235 s wall with a warm target.
  On parallel runners the gates' wall time is the slowest group (test), down from the serial sum.
- `CI=true` with no gitleaks on PATH: `secrets: FAIL` ("fails closed"); without `CI`: skip + PASS.
- The four groups name each of the 10 gates in `_all_gates` exactly once (diffed).
- `act` 0.2.89: `-l` shows the graph (`ci` → `deploy`; release `gates` → `build` → `release`);
  `pull_request -n` expanded `ci` into the four `gates (…)` matrix jobs, then stopped at "Set up job":
  no docker daemon (colima VM never created; pr/111: first VM needs Bobby's OK). Not run step by step.
- Toolchain: nixpkgs rustc **1.97.1**, not the `rust-toolchain.toml` 1.94.0 pin CI uses (no rustup
  here). A newer clippy usually has more lints, not fewer, but a 1.94-only lint would first show up in CI.
- Not run: any GitHub Actions run (nothing pushed); the fork condition, concurrency cancellation and
  reusable-workflow permissions are checked by actionlint only.

## For the next PR
- A check that the gates.yml groups and `_all_gates` agree (the diff above as a shellcheck-gate step),
  so a new gate cannot run locally only.
- If overlapping main deploys ever publish out of order, a job-level `concurrency` on `deploy` keyed
  on the Pages branch.
- The test group dominates (~196 s here); splitting `cargo test` by crate or using nextest partitions
  is the next wall-time win.

Paths (jj fileset, one pattern per line; cut with `pr/cut.sh`; disjoint from the other PRs).
`scripts/quality_gates.sh` is not in it: pr/15's fileset owns that file (earliest wins), and this PR's
two hunks there (`--all-targets`, the CI fail-closed branch) are assigned per hunk at the cut:
```
.github/workflows/gates.yml
.github/workflows/ci.yml
.github/workflows/site.yml
.github/workflows/release.yml
```
`replay.yml`'s two changed lines (checkout pin, runner image) and the `flake.nix` comment are
assigned per hunk to the records that carry those files.
