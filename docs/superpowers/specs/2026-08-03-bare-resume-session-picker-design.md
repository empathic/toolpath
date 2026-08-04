# Bare `path resume` — cross-harness session picker

**Status:** approved design, implementing
**Date:** 2026-08-03
**Issue:** #110

## Intent

`path resume` today requires an `<input>` (Pathbase URL, shorthand,
file, or cache id). But the most common resume wish is local: "pick up
that session I ran earlier — maybe in a different harness". Bare
`path resume` (no argument) opens a cross-harness session picker
(reusing `path share`'s aggregation), derives the picked session, and
flows into the existing harness-picker → project → exec pipeline.

No new TUI is introduced — the existing fzf/skim pickers are reused.
A native ratatui picker is a sibling PR behind the `fuzzy::pick` seam;
this change does not touch `fuzzy.rs`.

## Surface

```
path resume                       # bare: pick any session from any harness
path resume --from codex          # bare: only codex sessions in the picker
path resume --project /p          # bare: only sessions tied to /p
path resume --harness claude      # bare: resume target pinned; harness picker skipped
path resume -C /work/proj         # ranking anchor AND projection/exec cwd
path resume <input> [...]         # explicit mode, unchanged
```

- `ResumeArgs.input` becomes `Option<String>`; omitting it enters bare
  mode.
- `--from <harness>`: bare mode only — narrows the *session picker* to
  one harness. The resume *target* is still `--harness` / the harness
  picker. `conflicts_with = "input"`.
- `--project <path>`: bare mode only — narrows the session picker to
  sessions tied to that project directory (mirrors
  `path share --project`). `conflicts_with = "input"`.
- `--url` is inert in bare mode (bare mode never fetches from
  Pathbase); documented in its help text.
- `--force` in bare mode: skip the cache freshness probe, always
  re-derive the picked session.

## Behavior matrix

| Invocation | Behavior |
| --- | --- |
| bare + TTY | session picker → doc → harness picker → project → exec |
| bare, no TTY | bail exactly: `no input provided and no TTY for interactive selection; pass an <input> (URL, file, or cache id), or rerun in a terminal` — checked BEFORE gathering |
| bare `--harness X` | X validated on PATH BEFORE the session picker (pick_harness's error text); unfiltered session picker; harness picker skipped |
| bare `--from Y` | `harness_filter = Some(Y.artifact_type())` into `gather_artifacts` |
| bare `-C P` | canonicalized P is BOTH the ranking anchor passed to `gather_artifacts` AND the projection/exec cwd |
| 0 sessions gathered | `bail_no_sessions` with "resumable" wording (per-harness status table) |
| picker Cancelled | `std::process::exit(130)` |
| picker NoMatch | quiet `Ok(())` |

## Cache policy (locked decision — deviates from issue #110's sketch)

Issue #110 sketched an ephemeral derive. Locked decision instead:
**write-through by default, mirroring `share`.**

- Read fast-path: if `!no_cache && !force`,
  `sync::fresh_cache_id(bundle, row.artifact_type, project, &row.session_id)`
  → load the cached doc and print share's "Cache is current …" line
  (with a resume-appropriate verb).
- Otherwise `cmd_share::derive_session`, then unless `no_cache`:
  `cache::write_cached(&derived.cache_id, &derived.doc, true)` +
  `sync::record_artifact` (non-fatal warn on failure, like share).
- The `project` argument for `fresh_cache_id`/`derive_session` is
  `row.path.as_deref()` — `Some` for path-keyed providers
  (claude/gemini/pi), `None` for cwd-keyed ones — mirroring exactly how
  `share_explicit` passes it.

## Decisions Locked In

| Decision | Choice |
| --- | --- |
| Picker UI | Existing fzf/skim via `fuzzy::pick`; no new TUI (sibling PR) |
| Session aggregation | Reuse `cmd_share::gather_artifacts` (all 7 providers, cwd-ranked) |
| Picker rows | Reuse `cmd_share::format_picker_row` (5-col TSV), share-identical `PickOptions` with prompt `resume> ` |
| Cache policy | Write-through by default, mirroring share (deviates from issue #110's ephemeral sketch) |
| No-TTY error | Exact string, checked before gathering |
| Target validation | `--harness X` validated on PATH before the session picker fires |
| New flags placement | Mid-struct, immediately after `harness` (keeps hunks disjoint from in-flight PR #145, which appends at the END) |
| Picker seam | `SessionPicker` trait + `PickChoice` enum + `FixedPicker` test double, mirroring `ExecStrategy`/`RecordingExec` |
| `resolve_input` | `args.input.as_deref().expect(…)` — unreachable in bare mode by the `run_with_strategy` guard |
| `project_into_harness` | Reused as-is; never call `cmd_export::project_claude` directly (in-flight #150 changes its return type) |
| `ResumeArgs` construction | Gains `Default` so callers use `..Default::default()` — a deliberate rebase gift to #145 |
| Version | path-cli 0.16.1 → 0.17.0 (0.17.0 is also claimed by in-flight #138/#145 — collision recorded) |

## Implementation shape

1. `run_with_strategy` gains a compact early branch:
   `if args.input.is_none() { return run_bare(&args, exec, &FuzzySessionPicker); }`
   (#145 will insert its `--remote` early-return above it later).
2. `pub fn run_bare(args, exec, picker)`: availability guard → target
   pre-validation → cwd resolve → `HarnessBundle::from_environment()` →
   `gather_artifacts` → empty check → format rows → `picker.pick` →
   `PickChoice::Index(i)` → load-or-derive → `ensure_path_with_agent` →
   `source = row.artifact_type.harness().or_else(|| infer_source_harness(path))`
   → `pick_harness(args.harness, source, None)` →
   `project_into_harness` → `invocation_for` → `exec_harness`.
3. Picker seam: `pub enum PickChoice { Index(usize), Cancelled, NoMatch }`;
   `pub trait SessionPicker { fn available(&self) -> bool { true } fn pick(&self, lines: &[String], header: &str) -> Result<PickChoice>; }`.
   `FuzzySessionPicker` (prod) maps `Selected` → index via a pure
   `index_of_selected(lines, selected_line)` (line equality is safe:
   cols 1–3 are unique per row). `FixedPicker` test double records the
   offered lines.
4. `cmd_share.rs` lifts ONLY `format_picker_row` + `derive_session` to
   `pub(crate)`; `bail_no_sessions` gains an adjective parameter
   (share: "shareable", resume: "resumable"). No behavior changes.

## Addendum (2026-08-04): recency-first hydration

Dogfooding on a 3.9 GB codex tree showed the full cross-harness sweep
costs seconds per launch. The bare picker now hydrates only the newest
`RECENT_LIMIT` (100) sessions: codex rollouts are ranked stat-only by
file mtime and the top slice gets an O(1) head+tail `peek_metadata`
(no `message_count`); the other providers' listings are cheap and run
in full, in parallel. The merged view is ranked exactly like the full
sweep and truncated to the limit, with a tail row ("N older sessions —
load everything") that runs `gather_artifacts` on demand. `--project`
always uses the full sweep (its matches may be arbitrarily old). An
earlier scoped-to-project-first design was built and reverted: it hid
other projects' sessions behind an extra step, which is not how the
picker should rank — recency plus cwd-first sorting already puts the
right rows on top.
