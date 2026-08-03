# Native ratatui picker — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/path-cli/src/tui/` — a first-party ratatui picker behind the existing `embedded-picker` feature honoring the full `PickOptions` contract, then remove skim. External fzf stays as escape hatch.

**Spec:** `docs/superpowers/specs/2026-08-03-native-picker-design.md`

---

## File map

- **Create** `crates/path-cli/src/tui/{mod,state,render,matcher,preview}.rs`
- **Modify** `crates/path-cli/Cargo.toml` — deps `ratatui` 0.30, `crossterm` 0.29, `nucleo-matcher` 0.3, `ansi-to-tui` 8 (optional, cfg'd like skim); dev-dep `portable-pty` 0.9; drop `skim`/`regex` at the end
- **Modify** `crates/path-cli/src/lib.rs` — register `mod tui`, update `--picker` flag docs, drop `mod skim_picker`
- **Modify** `crates/path-cli/src/fuzzy.rs` — `Picker::Skim` -> `Picker::Native` (+ hidden `skim` alias + one-time note), delegate `pick_embedded` to `crate::tui::pick`, refresh module docs and error text
- **Delete** `crates/path-cli/src/skim_picker.rs`
- **Create** `crates/path-cli/tests/picker_pty.rs` (opt-in `#[ignore]` PTY smoke)
- **Modify** `CLAUDE.md`, `README.md`, version-bump files (0.17.0), `CHANGELOG.md`

## Task 1: Spec + plan docs

- [ ] Commit the design spec and this plan (first commit on the branch).

## Task 2: matcher module

- [ ] Add `ratatui`/`crossterm`/`nucleo-matcher`/`ansi-to-tui` optional deps in the `cfg(not(emscripten))` table; extend `embedded-picker` feature (skim stays for now).
- [ ] `tui/matcher.rs`: `Row`, `MatchEntry`, `FieldRange` + real `parse_field_spec`, `project_fields`, `NucleoMatcher` wrapper (Smart case/normalization, empty query = input order, score-desc/row-asc sort, char indices).
- [ ] Register `mod tui` in `lib.rs` behind the skim cfg.
- [ ] Tests: `parse_field_spec_single_index`, `_open_range_from`, `_open_range_to`, `_bounded_range`, `_comma_list`, `_rejects_negative_and_garbage`, `parse_field_spec_covers_every_in_repo_spec`, `project_fields_skips_out_of_range`.
- [ ] Green: `RUSTFLAGS="-D warnings" cargo test -p path-cli`.

## Task 3: preview module

- [ ] `tui/preview.rs`: `parse_preview_window` (colon-split, order-tolerant, never errors; defaults Right/60%/wrap); `PreviewScheduler` (pending/generation/cache; `on_selection_change`, `poll`, `on_msg`, `cached`); `substitute_placeholders` (shell-quoted `{1}`..`{n}`, `{}`); `spawn_preview_job` (std::thread, sh -c, kill slot, env sizes, ansi-to-tui with de-ANSI fallback, Failed with first stderr line).
- [ ] Tests: `parse_preview_window_right_percent_wrap_word`, `_up_stacked`, `_tolerates_unknown_tokens_with_defaults`, `debounce_coalesces_rapid_selection_changes`, `stale_generation_message_is_dropped`, `cache_hit_skips_spawn`, `substitute_placeholders_shell_quotes_fields`, `ansi_conversion_of_markdown_to_ansi_sample`, `failed_command_yields_failed_state_with_stderr_line`.
- [ ] Green.

## Task 4: state module

- [ ] `tui/state.rs`: `AppState` + pure `handle_event` covering the whole key contract (Enter/Esc/Ctrl-C/Ctrl-D, editing keys, selection moves, Tab/BackTab marks, Ctrl-O, Shift-scroll, dormant Ctrl-R `FilterHook` stub).
- [ ] Tests (event-vector driven): `enter_with_no_query_returns_first_row_original`, `typing_filters_and_enter_returns_top_match_original_line`, `enter_with_zero_matches_returns_no_match`, `esc_returns_cancelled`, `ctrl_c_returns_cancelled`, `ctrl_d_on_empty_query_cancels`, `tab_toggles_mark_and_advances_in_multi_mode`, `tab_is_noop_without_multi`, `enter_returns_marked_rows_in_input_order`, `marks_survive_query_change`, `query_change_resets_selection_to_top`, `up_down_clamp_at_bounds`, `page_down_moves_by_page`, `ctrl_u_clears_query_and_rematches`, `hidden_columns_are_not_searchable`, `resize_below_width_threshold_switches_side_to_stacked`.
- [ ] Green.

## Task 5: render module

- [ ] `tui/render.rs`: `LayoutPref` + `DEFAULT_LAYOUT`, `choose_layout` ladder, `compute_areas`, pane renderers (dim header, marker-column list with bold match spans, right-aligned status, prompt+input, preview Block titled "preview"/"preview (loading…)", placeholder + dim-red error states).
- [ ] Snapshot tests (TestBackend + insta): `snapshot_inline_empty_query`, `snapshot_inline_filtered_highlights`, `snapshot_multi_marked_rows`, `snapshot_fullscreen_side_preview_ready`, `snapshot_fullscreen_stacked_narrow`, `snapshot_no_match_status_line`, `snapshot_preview_loading_placeholder`, `snapshot_preview_error_pane`.
- [ ] Green.

## Task 6: event loop + terminal lifecycle

- [ ] `tui/mod.rs`: `InputEvent` + `From<crossterm KeyEvent>` (Release filtered), `TermGuard` (stderr backend, idempotent restore, inline-region clear, panic hook), resize/mode-change re-setup, 50 ms poll loop, preview mpsc drain + scheduler-driven spawns, `pub(crate) fn pick`.
- [ ] Green.

## Task 7: fuzzy.rs switch

- [ ] `Picker::Skim` -> `Picker::Native` with `#[value(alias = "skim")]`; one-time stderr note when the alias is used; Auto prefers native -> external fzf fallback; `pick_embedded` delegates to `crate::tui::pick`; refresh fuzzy.rs module docs, lib.rs flag docs, no-backend error text; lift `shell_quote` to `pub(crate)`.
- [ ] Green.

## Task 8: skim removal

- [ ] Delete `skim_picker.rs` + its `mod` line; drop `skim`/`regex` deps; `embedded-picker = ["dep:ratatui", "dep:crossterm", "dep:nucleo-matcher", "dep:ansi-to-tui"]`.
- [ ] Verify `cargo tree -p path-cli | grep -E "skim|tui-term|portable-pty|frizbee"` is empty (dev-deps aside) and `cargo build -p path-cli --no-default-features` builds.
- [ ] Green.

## Task 9: PTY smoke tests

- [ ] Dev-dep `portable-pty` 0.9; `tests/picker_pty.rs` with `#[ignore]` tests `pty_smoke_import_picker_accept_first_row`, `pty_smoke_esc_exits_130`.
- [ ] Run the ignored tests once locally; record the result.

## Task 10: docs + version bump

- [ ] CLAUDE.md "Interactive session selection" bullet; README picker section; 4-file bump to 0.17.0; CHANGELOG H2.
- [ ] Final gates: `scripts/quality_gates.sh -site` (or the shellcheck-less subset, noted in the report).

## Self-Review Notes

(Recorded during implementation; deviations from the spec land here.)
