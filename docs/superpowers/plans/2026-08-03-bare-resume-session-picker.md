# Bare `path resume` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `path resume` with no argument opens a cross-harness session picker (reusing `path share`'s aggregation), derives the picked session (write-through cache, mirroring share), then flows into the existing harness-picker → project → exec pipeline.

**Spec:** `docs/superpowers/specs/2026-08-03-bare-resume-session-picker-design.md`

---

## File map

- **Modify** `crates/path-cli/src/cmd_share.rs` — lift `format_picker_row` + `derive_session` to `pub(crate)`; `bail_no_sessions` gains an adjective param.
- **Modify** `crates/path-cli/src/cmd_resume.rs` — `input: Option<String>`, `--from`/`--project` flags, `Default`, `SessionPicker` seam, `run_bare`, unit tests.
- **Modify** `crates/path-cli/src/lib.rs` — `Commands::Resume` doc comment mentions bare mode.
- **Modify** `crates/path-cli/tests/support/mod.rs` — `args_explicit` via `..Default::default()`; claude/codex fixture writers; `ScopedHome` pins `XDG_DATA_HOME`.
- **Modify** `crates/path-cli/tests/resume.rs` — bare-mode integration tests.
- **Modify** `CLAUDE.md`, `CHANGELOG.md`, `Cargo.toml`, `crates/path-cli/Cargo.toml`, `site/_data/crates.json` — docs + 0.17.0 bump.

---

## Task 1: Lift share helpers

- [ ] `format_picker_row` → `pub(crate)`
- [ ] `derive_session` → `pub(crate)`
- [ ] `bail_no_sessions(bundle, project_filter, adjective)` — share passes "shareable"
- [ ] No behavior changes; `cargo test -p path-cli` green

## Task 2: ResumeArgs surface + Default

- [ ] `input: Option<String>` with bare-mode doc sentence
- [ ] `--from <Harness>` + `--project <PathBuf>` mid-struct after `harness`, both `conflicts_with = "input"`
- [ ] `--url` / `--force` / `--no-cache` doc comments note bare-mode semantics
- [ ] `#[derive(Default)]`; convert struct-literal constructors (cmd_resume unit tests, tests/support `args_explicit`, tests/resume.rs) to `..Default::default()`
- [ ] `resolve_input` uses `args.input.as_deref().expect("resolve_input requires input — bare mode is handled by run_bare")`

## Task 3: Picker seam + run_bare

- [ ] `PickChoice` + `SessionPicker` + `FuzzySessionPicker` + `index_of_selected` + `FixedPicker`
- [ ] `run_with_strategy` early branch to `run_bare(&args, exec, &FuzzySessionPicker)`
- [ ] `run_bare`: availability guard (exact no-TTY string) → target pre-validation → cwd → gather → empty check ("resumable") → pick → load-or-derive (fresh_cache_id fast path; write-through + record_artifact) → ensure_path_with_agent → source → pick_harness → project → exec
- [ ] Unit tests: bare_args_input_is_optional, from_conflicts_with_input, project_conflicts_with_input, index_of_selected_maps_line_back_to_row, index_of_selected_unknown_line_errors, fixed_picker_records_offered_lines, run_bare_offline_picker_errors_with_no_tty_text, run_bare_unavailable_target_harness_errors_before_picking

## Task 4: Integration tests

- [ ] Fixture writers `write_claude_session` / `write_codex_session` in tests/support (modeled on cmd_share unit-test fixtures); `ScopedHome` pins `XDG_DATA_HOME`
- [ ] bare_resume_picks_session_derives_projects_and_execs
- [ ] bare_resume_no_sessions_bails_with_status_table
- [ ] bare_resume_from_filters_the_picker
- [ ] bare_resume_cwd_flag_ranks_matching_sessions_first
- [ ] bare_resume_writes_cache_and_manifest_by_default
- [ ] bare_resume_no_cache_skips_cache_write
- [ ] bare_resume_fresh_cache_fast_path_uses_cached_doc
- [ ] bare_resume_no_match_returns_ok_without_exec

## Task 5: Docs + version bump

- [ ] CLAUDE.md: bare-resume example line in CLI usage; extend the `path resume` Things-to-know bullet (bare mode, --from, --project, cache policy, no-TTY error)
- [ ] lib.rs `Commands::Resume` doc comment mentions bare mode
- [ ] path-cli 0.16.1 → 0.17.0 in crates/path-cli/Cargo.toml, root Cargo.toml, site/_data/crates.json, CHANGELOG.md (H2 at top)
- [ ] `scripts/quality_gates.sh -site` green

## Self-Review Notes

Deviations from the spec found while implementing:

- The cache fast path's stderr line adapts share's verb: share prints
  "…; uploading without re-deriving", resume prints "…; resuming
  without re-deriving". Same shape, resume-appropriate wording.
- Two share-mirroring stderr confirmations were added that the spec's
  implementation shape didn't list: `Picked <harness> session "<title>"`
  after the session pick (the flow continues into a second picker, so
  the user should see what they committed to) and share's
  `Cached <harness> session → <id> (<path>)` line after a write-through.
- Target pre-validation is implemented as a literal
  `pick_harness(Some(h), None, None)` call rather than duplicating the
  error text — same message verbatim, and the later post-derive call
  re-validates for free.
- `bare_resume_fresh_cache_fast_path_uses_cached_doc` passes
  `--project /test/project` on both runs: run 1's projection writes a
  *new* claude session under the scoped `$HOME`, which would outrank
  the fixture by recency in run 2's picker; the project filter pins the
  picker to the fixture row (and exercises `--project` for free).
- `bare_resume_cwd_flag_ranks_matching_sessions_first` records the
  *canonicalized* tempdir path in the codex fixture (macOS tempdirs
  canonicalize `/var/…` → `/private/var/…`), and writes the matching
  fixture first (older mtime) so the ranking win is attributable to the
  cwd match, not recency.
- `tests/support/mod.rs::ScopedHome` now also pins `$XDG_DATA_HOME`
  (PR #142's fix has not landed) so the opencode collector cannot leak
  the developer's real database into gather results.
- Version-slot collision: 0.17.0 is also claimed by in-flight PRs #138
  and #145; whichever lands second rebases its CHANGELOG H2 and takes
  the next slot.

### Review fixes

Adversarial review found five issues; fixed in one follow-up commit:

- **Vacuous ranking test**: `write_codex_session` hardcoded identical
  embedded timestamps into both fixtures, so
  `bare_resume_cwd_flag_ranks_matching_sessions_first` tied on
  `last_activity` and passed by enumeration order. Added
  `write_codex_session_at` (minute-precision stamp param) and gave the
  NON-matching fixture a strictly newer timestamp; verified by
  temporarily sabotaging `matches_cwd: false` in the codex collector
  (test failed) and reverting (test passed).
- **COPILOT_HOME leak**: `ScopedHome` now pins `COPILOT_HOME` under the
  tempdir (and restores it in Drop) — toolpath-copilot honors it as a
  full root override, so a developer's real sessions could leak into
  bare-mode gather results.
- **Misleading empty result with --from**: `run_bare` with a `--from`
  filter and zero rows now bails with
  `no <name> sessions found; drop --from to see sessions from other
  harnesses` instead of the generic all-harness status table; pinned by
  `bare_resume_from_with_no_matching_sessions_mentions_filter`.
- **Raw title in confirmation**: the `Picked … session` line now runs
  the title through `fuzzy::clean_for_picker_display` (already
  pub(crate)), matching what the picker rows show.
- **toolpath-codex fmt hunks**: kept in the branch's style commit as-is
  (standalone landing impractical).
