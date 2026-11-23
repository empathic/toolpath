# test(agents): hash-verified agent binaries from agents.lock and a hermetic runner (`toolpath-agents fetch|path|run`)

Status: draft (not opened) · Base: main · Stack 0 of the stack: every slice after it builds on it · Source: Bobby 2026-10-28; patch: pr/patches/00-agents-hermetic.patch

## Why
Bobby, 2026-10-28, on the open stack (#359…#370): "give me the first one. I think the first thing of the entire stack should be
downloading binary and setting up that hermetic stuff". The goldens (#360…), resume fixes (#361, #363, #365), webmock (#368, #370) and
replay (pr/117) all run real agent CLIs, and today each finds them its own way (PATH, a developer install, Homebrew: a Homebrew codex raised
a Gatekeeper prompt on Bobby's screen during replay, pr/117 *Why*). What a test proves depends on which binary ran and what it could see.
This slice makes both explicit before anything else lands:
- **which binary**: only what `agents.lock` names, by url and the vendor's published sha256, downloaded and verified, never from PATH;
- **what it sees**: an empty environment, a fresh HOME/TMPDIR, cwd `/`, plus only what the test adds.

## What changes
- `agents.lock` (repo root, new): claude 2.1.308 and codex 0.165.0 for `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`; each sha256
  is the vendor's own checksum (claude: the release `manifest.json`; codex: GitHub's release-asset digest), re-checked by download on 2026-10-28.
- `crates/toolpath-agents` (new, `publish = false`, a dev crate: nothing in the shipped `path` binary changes):
  - `lock`: parse + validate (64-hex sha256, https only, a tar-gz names its member, no duplicate agent/target);
  - `cache`: `fetch` downloads and refuses on a hash mismatch, keeps the blob under its sha256 (no execute bit), re-hashes on every lookup;
    `install` unpacks a verified blob and makes only the named member executable;
  - `hermetic`: `Hermetic::new(exe).env(k, v).command(args)`: empty env + `HOME`/`TMPDIR` (temp, removed on drop) + `PATH=/usr/bin:/bin`
    + `LANG=C.UTF-8`, cwd `/`;
  - bin `toolpath-agents list|fetch|path|run`.
- Workspace: `crates/toolpath-agents` added to `members`; deps already in the lockfile except `tar` (0.4).

## Proves
- A download is kept only if it hashes to the lock's sha256 — `toolpath-agents::cache::tests::a_blob_is_kept_only_under_its_own_hash_and_a_mismatch_is_refused`
- A cache entry that no longer matches its hash is never used — `cache::tests::a_corrupted_cache_entry_is_not_trusted`
- Only the named member of a verified archive becomes runnable — `cache::tests::install_unpacks_the_named_member_of_a_verified_tar_gz_and_makes_only_it_runnable`
- The lock refuses placeholders, uppercase/short hashes, http and member-less archives — `lock::tests::a_lock_without_a_real_hash_an_https_url_or_a_member_is_refused`
- The committed lock covers both CI platforms for every agent — `lock::tests::the_committed_lock_parses_and_covers_this_repos_ci_platforms`
- Lookup by agent and platform, with named errors — `lock::tests::the_lock_names_agents_by_platform`
- The child sees exactly HOME, LANG, PATH=/usr/bin:/bin, TMPDIR and what the test adds, in cwd `/` — `hermetic::tests::the_child_sees_only_a_temp_home_system_path_and_what_the_test_adds`
- HOME/TMPDIR are fresh per run and removed after — `hermetic::tests::home_and_tmpdir_are_fresh_per_run_and_removed_afterwards`

## Verification (2026-10-28, break-glass fork, scratch worktree off origin/main 77dc16a5)
Toolchain: the toolpath flake devShell (cargo 1.97.0; the repo pins 1.94.0, so CI is the authority).
- `scripts/quality_gates.sh format shellcheck clippy test doc examples`: **6/6 passed** (workspace tests include the 8 above).
- Real fetch, `XDG_CACHE_HOME` in a scratch dir: `toolpath-agents fetch codex` (32.6 s) and `fetch claude` (57.8 s, ~275 MB), both
  verified against the lock; `list` shows both `fetched` for aarch64-apple-darwin.
- Hermetic run, no credential anywhere in the child's env: `toolpath-agents run claude --version` → `2.1.308 (Claude Code)`;
  `run codex --version` → `codex-cli 0.165.0`.
- Tamper: one byte appended to the cached codex blob → `path codex` refuses ("not in the cache"); restored → resolves again.
- NOT run: the linux entries (hashes are the vendor's published ones; not downloaded here), act/CI.

## Restack
- Every open slice rebases onto this one, in order: #359 (pr/01-convo-breakdowns) first, then #360 (pr/02-goldens-core), #361 … #370.
  #359 does not use it (toolpath-convo only); it moves up so the stack has one base.
- #360 goldens: `check` already runs projections in a hermetic child (empty env, temp HOME, cwd `/`); `capture-live` should take its agent
  from `toolpath-agents` (`Cache::install` + `Hermetic`) instead of PATH. That also gives the copilot `--allow-all-tools` fix one place to land.
- pr/117 shrinks: `path d agents list|fetch|path` becomes a thin wrapper over `toolpath_agents` (or is dropped for `toolpath-agents`), its
  own lock code and tests go; `cargo xtask pins` keeps act/image pins only.
- pr/119 (hermetic harness install, not written): its install half is this slice; what is left is putting the fetched agents into the act
  image / replay job (`WEBMOCK_REQUIRE` then turns green).
- Not locked yet (follow-ups, one row each): opencode (darwin ships a .zip: add a zip archive kind), gemini, copilot, pi (npm packages on
  node: lock the tarball by its registry sha512 plus a pinned node), cursor-agent (no published checksum: lock by download hash).

## Checklist
1. [x] **Independent PR** — no bundled or speculative changes
   - [x] Verified: one new dev crate + agents.lock + one workspace member line; no shipped crate changes
2. [x] **Changed codepaths verified** — targeted unit and full integration tests
   - [x] Verified: the 8 tests under *Proves*; real fetch + hermetic `--version` of claude and codex (see *Verification*); repo gates
3. [x] **Root cause identified** — every failure traced to source
   - [x] agents came from PATH/Homebrew with no pin and inherited the developer's environment
4. [x] **No duplication** — refactoring preferred over copy/paste
   - [x] pr/117's path-cli/src/agents.rs lock+cache code moves here (generalised: archives, install, hermetic); pr/117 then wraps it
5. [x] **No unrelated changes** — housekeeping isolated to its own branch
   - [x] N/A

Paths (jj fileset):
```
agents.lock
crates/toolpath-agents
Cargo.toml
Cargo.lock
pr/00-agents-hermetic.md
pr/patches/00-agents-hermetic.patch
```
