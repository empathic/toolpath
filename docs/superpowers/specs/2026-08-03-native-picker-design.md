# Native ratatui picker — design

**Status:** implemented
**Date:** 2026-08-03

## Intent

Replace the embedded skim backend of the interactive fuzzy picker with a
first-party ratatui implementation (Atuin-inspired) that we fully control:
adaptive inline/fullscreen layouts, debounced async previews, fzf-style
query operators, and a testable pure-state core. The external `fzf`
backend stays untouched as the escape hatch. All ~26 existing call sites
(`cmd_import`, `cmd_share`, `cmd_resume`) keep working unchanged because
the new picker honors the full `PickOptions` contract.

## Decisions Locked In

| Decision | Choice |
| --- | --- |
| Default layout | `const DEFAULT_LAYOUT: LayoutPref = LayoutPref::Adaptive` — inline viewport without a preview, fullscreen alt-screen with one |
| Matcher | `nucleo-matcher` 0.3, `Pattern::parse(query, CaseMatching::Smart, Normalization::Smart)` — space=AND, `'exact`, `^prefix`, `!negate` are a deliberate upgrade over skim |
| Search scope | `Row.display` (the `with_nth` projection) ONLY — hidden columns never match |
| Tiebreak | score-desc then row-asc (`tiebreak=index` — the only value used in-repo) |
| Marks | `BTreeSet<usize>` keyed by ROW index; survive query changes; returned in input order |
| Preview runtime | NO tokio — `std::thread` + `mpsc`, pure `PreviewScheduler` debounce state machine (100 ms), generation counter drops stale results, kill-slot supersede |
| Terminal | ratatui 0.30 `CrosstermBackend` on **stderr** (stdout stays clean for piped results); no `ratatui::init()` |
| Side-by-side threshold | `right:`/`left:` previews go side-by-side at term width >= 100, stacked below |
| Inline height | header(0/1) + min(rows,12) + status(1) + input(1), clamped to min(…, 15, term_h-1); promote to fullscreen when fewer than ~5 usable list rows fit |
| `--picker` flag | `skim` variant renamed `native` with hidden alias `skim` + one-time stderr note; `auto` prefers native, falls back to external fzf |
| Skim | removed entirely at the end (deps `skim`, `regex` dropped); `embedded-picker` feature repoints to `ratatui`/`crossterm`/`nucleo-matcher`/`ansi-to-tui` |
| Ctrl-R | reserved: dormant `FilterHook` stub field in `AppState`, no behavior yet (future bare-resume session picker) |
| Mouse | none in v1 |

## Surface

`crates/path-cli/src/tui/` behind the existing
`cfg(all(not(target_os = "emscripten"), feature = "embedded-picker"))`:

- `mod.rs` — event loop, terminal lifecycle (`TermGuard`, panic hook,
  resize/mode-change handling), `pub(crate) fn pick(lines, opts) ->
  Result<PickResult>`, `InputEvent` + `From<crossterm KeyEvent>`.
- `state.rs` — `AppState` + pure `handle_event(&mut AppState, InputEvent)
  -> Option<PickResult>` (no IO, event-vector testable).
- `render.rs` — layout ladder (`LayoutPref`, `choose_layout`), panes
  (header/list/status/input/preview), bold match-span highlighting.
- `matcher.rs` — nucleo wrapper, REAL `parse_field_spec` for fzf
  `--with-nth` notation (`3`, `1..`, `2..4`, `..2`, `1,3`; rejects
  garbage), `project_fields`.
- `preview.rs` — `PreviewScheduler` (pure debounce), `spawn_preview_job`
  (`sh -c`, `{1}`..`{n}`/`{}` shell-quoted substitution, COLUMNS /
  FZF_PREVIEW_COLUMNS / FZF_PREVIEW_LINES env, ansi-to-tui conversion),
  `parse_preview_window` (order-tolerant, never errors).

## Semantics (contract)

- `Row { original, fields, display }`; `Selected` returns `original`
  (full TSV line). `MatchEntry { row, score, indices }`.
- Enter: marked rows in input order if any, else highlighted row, else
  `NoMatch`. Esc / Ctrl-C -> `Cancelled`. Ctrl-D on empty query ->
  `Cancelled` (fzf parity), else delete-forward.
- Editing: Backspace, Ctrl-U (clear), Ctrl-W (word),
  Left/Right/Home/End/Ctrl-A/Ctrl-E (char-boundary safe). Query change
  rematches and resets selection to top.
- Selection: Up/Down/Ctrl-P/Ctrl-N clamped; PgUp/PgDn by page. Tab
  toggles mark + advances, BackTab toggles + retreats (multi only).
- Ctrl-O toggles the preview pane (fullscreen only). Shift-Up/Down
  scroll the preview.
- Preview UX: cache hit renders instantly; miss with prior text keeps
  the text with title "preview (loading…)"; no prior text shows a dim
  "deriving preview…" placeholder; failures render the first stderr
  line dim-red in the pane and never crash the picker.
- `KeyEventKind::Release` events are filtered (Windows double-fire).

## Out of scope

- `cmd_resume.rs` / `cmd_share.rs` changes beyond doc references (a
  sibling PR owns those).
- The external fzf backend (`pick_external`) — untouched.
- Ctrl-R behavior (stub only).
