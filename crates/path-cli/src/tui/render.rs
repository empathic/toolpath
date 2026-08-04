//! Layout ladder and pane rendering for the native picker.
//!
//! The picker adapts to its job: a plain list gets a small *inline*
//! viewport under the shell prompt (Atuin-style); a preview-bearing
//! picker takes over the alternate screen so the preview has room.
//! [`choose_layout`] is the single decision point.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;

use super::preview::{PaneSize, PreviewWindow, Side, WrapMode};
use super::state::AppState;

/// Overall layout preference. `Adaptive` (the default) picks inline
/// for preview-less pickers and fullscreen otherwise; `Inline` and
/// `Fullscreen` force one mode (no in-repo caller forces yet — the
/// knob exists so a future flag can).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutPref {
    Adaptive,
    /// Force the inline viewport. No production caller constructs the
    /// forced variants yet — they exist for a future layout flag (and
    /// the ladder tests exercise them).
    #[allow(dead_code)]
    Inline,
    /// Force the fullscreen alternate screen.
    #[allow(dead_code)]
    Fullscreen,
}

/// LOCKED DECISION: the default layout is adaptive.
pub(super) const DEFAULT_LAYOUT: LayoutPref = LayoutPref::Adaptive;

/// The concrete layout chosen for the current terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutMode {
    /// Inline viewport of `height` rows at the shell cursor.
    Inline { height: u16 },
    /// Alternate screen, no preview pane (none configured, or toggled
    /// off with Ctrl-O).
    Fullscreen,
    /// Alternate screen with a preview pane on `side` sized by `size`.
    /// A `right:`/`left:` spec degrades to a stacked `Up` pane below
    /// the side-by-side width threshold.
    FullscreenSplit { side: Side, size: PaneSize },
}

impl LayoutMode {
    /// True when this mode runs on the alternate screen.
    pub fn is_fullscreen(&self) -> bool {
        !matches!(self, LayoutMode::Inline { .. })
    }
}

/// Minimum terminal width for a side-by-side (left/right) preview
/// split; anything narrower stacks the preview above the list.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 100;

/// Most list rows an inline viewport will show.
const INLINE_MAX_LIST_ROWS: usize = 12;

/// Hard cap on the inline viewport height.
const INLINE_MAX_HEIGHT: u16 = 15;

/// Pick the layout for the current state + terminal size.
pub(super) fn choose_layout(state: &AppState) -> LayoutMode {
    layout_for(
        DEFAULT_LAYOUT,
        state.has_preview,
        state.preview_visible,
        state.preview_window,
        state.header.is_some(),
        state.rows.len(),
        state.term_w,
        state.term_h,
    )
}

/// The pure ladder, parameterized for tests.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_for(
    pref: LayoutPref,
    has_preview: bool,
    preview_visible: bool,
    window: PreviewWindow,
    has_header: bool,
    nrows: usize,
    term_w: u16,
    term_h: u16,
) -> LayoutMode {
    let fullscreen = || {
        if has_preview && preview_visible {
            let side = match window.side {
                Side::Left | Side::Right if term_w < SIDE_BY_SIDE_MIN_WIDTH => Side::Up,
                side => side,
            };
            LayoutMode::FullscreenSplit {
                side,
                size: window.size,
            }
        } else {
            LayoutMode::Fullscreen
        }
    };
    match pref {
        LayoutPref::Fullscreen => fullscreen(),
        LayoutPref::Inline => {
            inline_or_promote(has_header, nrows, term_h).unwrap_or_else(fullscreen)
        }
        LayoutPref::Adaptive => {
            if has_preview {
                fullscreen()
            } else {
                inline_or_promote(has_header, nrows, term_h).unwrap_or_else(fullscreen)
            }
        }
    }
}

/// Inline viewport height: header(0|1) + min(rows, 12) + status(1) +
/// input(1), clamped to min(…, 15, term_h - 1). Returns `None` (promote
/// to fullscreen) when fewer than ~5 usable list rows would fit.
fn inline_or_promote(has_header: bool, nrows: usize, term_h: u16) -> Option<LayoutMode> {
    let chrome: u16 = u16::from(has_header) + 1 /* status */ + 1 /* input */;
    let desired_list = nrows.clamp(1, INLINE_MAX_LIST_ROWS) as u16;
    let height = (chrome + desired_list)
        .min(INLINE_MAX_HEIGHT)
        .min(term_h.saturating_sub(1));
    let usable = height.saturating_sub(chrome);
    if usable < desired_list.min(5) {
        return None;
    }
    Some(LayoutMode::Inline { height })
}

/// The panes of one rendered frame. `preview` is present only in
/// [`LayoutMode::FullscreenSplit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Areas {
    pub header: Option<Rect>,
    pub list: Rect,
    pub status: Rect,
    pub input: Rect,
    pub preview: Option<Rect>,
}

/// Carve `area` into panes for `mode`. Frame anatomy: optional dim
/// header line, list, status line, prompt+input at the bottom; the
/// preview pane (fullscreen split only) takes its side first.
pub(super) fn compute_areas(state: &AppState, mode: LayoutMode, area: Rect) -> Areas {
    let (list_zone, preview) = match mode {
        LayoutMode::FullscreenSplit { side, size } => {
            let (list_zone, preview) = split_preview(area, side, size);
            (list_zone, Some(preview))
        }
        _ => (area, None),
    };
    let has_header = state.header.is_some();
    let chrome: u16 = u16::from(has_header) + 2;
    let list_h = list_zone.height.saturating_sub(chrome);
    let mut y = list_zone.y;
    let header = has_header.then(|| {
        let r = Rect::new(list_zone.x, y, list_zone.width, 1.min(list_zone.height));
        y += 1;
        r
    });
    let list = Rect::new(list_zone.x, y, list_zone.width, list_h);
    y += list_h;
    let status = Rect::new(list_zone.x, y, list_zone.width, 1.min(list_zone.height));
    y += 1;
    let input = Rect::new(
        list_zone.x,
        y.min(list_zone.bottom().saturating_sub(1)),
        list_zone.width,
        1.min(list_zone.height),
    );
    Areas {
        header,
        list,
        status,
        input,
        preview,
    }
}

/// Split `area` into (list zone, preview pane) along `side`.
fn split_preview(area: Rect, side: Side, size: PaneSize) -> (Rect, Rect) {
    match side {
        Side::Left | Side::Right => {
            let w = match size {
                PaneSize::Percent(p) => {
                    (u32::from(area.width) * u32::from(p.min(100)) / 100) as u16
                }
                PaneSize::Lines(n) => n.min(area.width),
            };
            let w = w.min(area.width);
            if side == Side::Right {
                let list = Rect::new(area.x, area.y, area.width - w, area.height);
                let preview = Rect::new(area.x + area.width - w, area.y, w, area.height);
                (list, preview)
            } else {
                let preview = Rect::new(area.x, area.y, w, area.height);
                let list = Rect::new(area.x + w, area.y, area.width - w, area.height);
                (list, preview)
            }
        }
        Side::Up | Side::Down => {
            let h = match size {
                PaneSize::Percent(p) => {
                    (u32::from(area.height) * u32::from(p.min(100)) / 100) as u16
                }
                PaneSize::Lines(n) => n.min(area.height),
            };
            let h = h.min(area.height);
            if side == Side::Up {
                let preview = Rect::new(area.x, area.y, area.width, h);
                let list = Rect::new(area.x, area.y + h, area.width, area.height - h);
                (list, preview)
            } else {
                let list = Rect::new(area.x, area.y, area.width, area.height - h);
                let preview = Rect::new(area.x, area.y + area.height - h, area.width, h);
                (list, preview)
            }
        }
    }
}

/// What the preview pane should show this frame. The event loop
/// derives it from the scheduler cache; render just paints it.
pub(super) struct PreviewView<'a> {
    /// Pane title: `"preview"` or `"preview (loading…)"`.
    pub title: &'a str,
    pub body: PreviewBody<'a>,
}

pub(super) enum PreviewBody<'a> {
    /// A finished preview (possibly kept from the previous selection
    /// while a newer one derives).
    Text(&'a Text<'static>),
    /// Nothing to show yet.
    Placeholder,
    /// The preview command failed; the first stderr line.
    Error(&'a str),
}

/// Paint one frame. `preview` is ignored unless `mode` has a preview
/// pane.
pub(super) fn draw(
    frame: &mut Frame<'_>,
    state: &AppState,
    mode: LayoutMode,
    preview: Option<&PreviewView<'_>>,
) {
    let areas = compute_areas(state, mode, frame.area());
    if let (Some(area), Some(text)) = (areas.header, state.header.as_deref()) {
        frame.render_widget(Paragraph::new(text).style(Style::new().dim()), area);
    }
    render_list(frame, areas.list, state);
    render_status(frame, areas.status, state);
    render_input(frame, areas.input, state);
    if let (Some(area), Some(view)) = (areas.preview, preview) {
        render_preview(frame, area, state, view);
    }
}

/// The match list with its marker gutter: `>` on the highlighted row,
/// `*` on marked rows, matched chars in bold.
fn render_list(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    // Stateless scroll: keep the highlighted row visible.
    let offset = (state.selected + 1).saturating_sub(height);
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(height);
    for (i, entry) in state.matches.iter().enumerate().skip(offset).take(height) {
        let row = &state.rows[entry.row];
        let is_selected = i == state.selected;
        let is_marked = state.marked.contains(&entry.row);
        let mut spans: Vec<Span<'_>> = Vec::new();
        spans.push(if is_selected {
            Span::styled("> ", Style::new().bold())
        } else {
            Span::raw("  ")
        });
        spans.push(if is_marked {
            Span::styled("* ", Style::new().bold())
        } else {
            Span::raw("  ")
        });
        spans.extend(highlight_spans(&row.display, &entry.indices, is_selected));
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Split `display` into styled spans: matched grapheme positions
/// render bold+underlined (on top of the selected-row style).
///
/// `indices` are GRAPHEME positions as reported by nucleo (its
/// haystacks segment by grapheme), so the iteration here must segment
/// identically — enumerating `chars()` would land highlights inside
/// multi-codepoint clusters (emoji ZWJ sequences) instead of on the
/// matched text.
fn highlight_spans<'a>(display: &'a str, indices: &[u32], selected: bool) -> Vec<Span<'a>> {
    let base = if selected {
        Style::new().bold()
    } else {
        Style::new()
    };
    let hilite = base.bold().underlined();
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hilited = false;
    for (gi, grapheme) in display.graphemes(true).enumerate() {
        let hit = indices.binary_search(&(gi as u32)).is_ok();
        if hit != run_hilited && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_hilited { hilite } else { base },
            ));
        }
        run_hilited = hit;
        run.push_str(grapheme);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hilited { hilite } else { base }));
    }
    spans
}

/// Right-aligned dim status: `N/M` match count plus the mark count
/// when any rows are marked.
fn render_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut status = format!("{}/{}", state.matches.len(), state.rows.len());
    if !state.marked.is_empty() {
        status.push_str(&format!(" · {} marked", state.marked.len()));
    }
    frame.render_widget(
        Paragraph::new(status)
            .style(Style::new().dim())
            .right_aligned(),
        area,
    );
}

/// The prompt + query input line, with the terminal cursor parked at
/// the edit position.
fn render_input(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let line = Line::from(vec![
        Span::styled(state.prompt.clone(), Style::new().bold()),
        Span::raw(state.query.clone()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    let prompt_cols = state.prompt.chars().count() as u16;
    let cursor_cols = state.query[..state.cursor].chars().count() as u16;
    let x = (area.x + prompt_cols + cursor_cols).min(area.right().saturating_sub(1));
    frame.set_cursor_position((x, area.y));
}

/// The preview pane: a titled block around the preview text, a dim
/// placeholder, or a dim-red error line.
fn render_preview(frame: &mut Frame<'_>, area: Rect, state: &AppState, view: &PreviewView<'_>) {
    let block = Block::bordered().title(view.title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let paragraph = match view.body {
        PreviewBody::Text(text) => Paragraph::new(text.clone()),
        PreviewBody::Placeholder => Paragraph::new("deriving preview…").style(Style::new().dim()),
        PreviewBody::Error(line) => Paragraph::new(line).style(Style::new().dim().fg(Color::Red)),
    };
    let paragraph = match state.preview_window.wrap {
        WrapMode::NoWrap => paragraph,
        WrapMode::Wrap | WrapMode::WrapWord => paragraph.wrap(Wrap { trim: false }),
    };
    frame.render_widget(paragraph.scroll((state.preview_scroll, 0)), inner);
}

#[cfg(test)]
mod tests {
    use super::super::InputEvent;
    use super::super::matcher::{Row, parse_field_spec};
    use super::super::state::handle_event;
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn window(side: Side) -> PreviewWindow {
        PreviewWindow {
            side,
            size: PaneSize::Percent(60),
            wrap: super::super::preview::WrapMode::WrapWord,
        }
    }

    #[test]
    fn adaptive_without_preview_is_inline() {
        let mode = layout_for(
            LayoutPref::Adaptive,
            false,
            false,
            PreviewWindow::default(),
            false,
            3,
            80,
            24,
        );
        assert_eq!(mode, LayoutMode::Inline { height: 5 });
    }

    #[test]
    fn inline_height_clamps_to_cap_and_terminal() {
        // 50 rows want 12 list rows + 2 chrome = 14.
        let mode = layout_for(
            LayoutPref::Adaptive,
            false,
            false,
            PreviewWindow::default(),
            false,
            50,
            80,
            24,
        );
        assert_eq!(mode, LayoutMode::Inline { height: 14 });
        // A header adds a row but the cap is 15.
        let mode = layout_for(
            LayoutPref::Adaptive,
            false,
            false,
            PreviewWindow::default(),
            true,
            50,
            80,
            24,
        );
        assert_eq!(mode, LayoutMode::Inline { height: 15 });
    }

    #[test]
    fn tiny_terminal_promotes_inline_to_fullscreen() {
        let mode = layout_for(
            LayoutPref::Adaptive,
            false,
            false,
            PreviewWindow::default(),
            false,
            50,
            80,
            6,
        );
        assert_eq!(mode, LayoutMode::Fullscreen);
    }

    #[test]
    fn preview_forces_fullscreen_split() {
        let mode = layout_for(
            LayoutPref::Adaptive,
            true,
            true,
            window(Side::Right),
            false,
            3,
            120,
            30,
        );
        assert_eq!(
            mode,
            LayoutMode::FullscreenSplit {
                side: Side::Right,
                size: PaneSize::Percent(60)
            }
        );
    }

    #[test]
    fn narrow_terminal_stacks_side_preview() {
        let mode = layout_for(
            LayoutPref::Adaptive,
            true,
            true,
            window(Side::Right),
            false,
            3,
            80,
            30,
        );
        assert_eq!(
            mode,
            LayoutMode::FullscreenSplit {
                side: Side::Up,
                size: PaneSize::Percent(60)
            }
        );
    }

    #[test]
    fn ctrl_o_hidden_preview_is_plain_fullscreen() {
        let mode = layout_for(
            LayoutPref::Adaptive,
            true,
            false,
            window(Side::Up),
            false,
            3,
            120,
            30,
        );
        assert_eq!(mode, LayoutMode::Fullscreen);
    }

    // ── Frame snapshots ──────────────────────────────────────────────

    fn plain_state(lines: &[&str], with_nth: &str, multi: bool) -> AppState {
        let spec = parse_field_spec(with_nth).unwrap();
        let rows: Vec<Row> = lines.iter().map(|l| Row::new(l, &spec)).collect();
        AppState::new(rows, multi, "> ", None, None)
    }

    fn preview_state(lines: &[&str], side: Side) -> AppState {
        let spec = parse_field_spec("2..").unwrap();
        let rows: Vec<Row> = lines.iter().map(|l| Row::new(l, &spec)).collect();
        AppState::new(rows, false, "> ", None, Some(window(side)))
    }

    /// Render one frame at (w, h) and snapshot the buffer.
    fn render_frame(
        state: &AppState,
        preview: Option<&PreviewView<'_>>,
        w: u16,
        h: u16,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mode = choose_layout(state);
        terminal.draw(|f| draw(f, state, mode, preview)).unwrap();
        terminal
    }

    #[test]
    fn snapshot_inline_empty_query() {
        let mut state = plain_state(&["first row", "second row", "third row"], "1..", false);
        state.term_w = 40;
        state.term_h = 24;
        // Inline viewport: the backend is exactly the viewport's size.
        let LayoutMode::Inline { height } = choose_layout(&state) else {
            panic!("expected inline layout");
        };
        let terminal = render_frame(&state, None, 40, height);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_inline_filtered_highlights() {
        let mut state = plain_state(&["alpha work", "beta work", "gamma play"], "1..", false);
        state.term_w = 40;
        state.term_h = 24;
        for c in "work".chars() {
            handle_event(&mut state, InputEvent::Char(c));
        }
        let LayoutMode::Inline { height } = choose_layout(&state) else {
            panic!("expected inline layout");
        };
        let terminal = render_frame(&state, None, 40, height);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_multi_marked_rows() {
        let mut state = plain_state(&["one", "two", "three"], "1..", true);
        state.term_w = 40;
        state.term_h = 24;
        handle_event(&mut state, InputEvent::Tab); // mark "one", advance
        handle_event(&mut state, InputEvent::Tab); // mark "two", advance
        let LayoutMode::Inline { height } = choose_layout(&state) else {
            panic!("expected inline layout");
        };
        let terminal = render_frame(&state, None, 40, height);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_no_match_status_line() {
        let mut state = plain_state(&["alpha", "beta"], "1..", false);
        state.term_w = 40;
        state.term_h = 24;
        for c in "zzz".chars() {
            handle_event(&mut state, InputEvent::Char(c));
        }
        let LayoutMode::Inline { height } = choose_layout(&state) else {
            panic!("expected inline layout");
        };
        let terminal = render_frame(&state, None, 40, height);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_fullscreen_side_preview_ready() {
        let mut state = preview_state(
            &[
                "s1\t2026-08-01 10:00 first session",
                "s2\t2026-08-02 11:00 second session",
            ],
            Side::Right,
        );
        state.term_w = 120;
        state.term_h = 16;
        let text = Text::raw("# Session\n\nrendered preview body");
        let view = PreviewView {
            title: "preview",
            body: PreviewBody::Text(&text),
        };
        let terminal = render_frame(&state, Some(&view), 120, 16);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_fullscreen_stacked_narrow() {
        let mut state = preview_state(
            &[
                "s1\t2026-08-01 10:00 first session",
                "s2\t2026-08-02 11:00 second session",
            ],
            Side::Right,
        );
        // Below the side-by-side threshold: the right: spec stacks.
        state.term_w = 60;
        state.term_h = 18;
        let text = Text::raw("stacked preview body");
        let view = PreviewView {
            title: "preview",
            body: PreviewBody::Text(&text),
        };
        let terminal = render_frame(&state, Some(&view), 60, 18);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn snapshot_preview_loading_placeholder() {
        let mut state = preview_state(&["s1\tonly session"], Side::Up);
        state.term_w = 60;
        state.term_h = 14;
        let view = PreviewView {
            title: "preview (loading…)",
            body: PreviewBody::Placeholder,
        };
        let terminal = render_frame(&state, Some(&view), 60, 14);
        insta::assert_snapshot!(terminal.backend());
    }

    // ── Highlight styling ────────────────────────────────────────────

    #[test]
    fn highlight_spans_treats_indices_as_grapheme_positions() {
        use ratatui::style::Modifier;
        // "👩‍👩‍👦 fix parser" segments as [👩‍👩‍👦][ ][f][i][x][ ][p]… — the
        // emoji ZWJ sequence is ONE grapheme (5 codepoints), so nucleo
        // reports "fix" at grapheme positions 2..=4. Codepoint
        // enumeration would place those indices inside the emoji.
        let spans = highlight_spans(
            "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f466} fix parser",
            &[2, 3, 4],
            false,
        );
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            texts,
            vec![
                "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f466} ",
                "fix",
                " parser"
            ]
        );
        assert!(spans[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!spans[2].style.add_modifier.contains(Modifier::UNDERLINED));
    }

    /// Style-level assertion on the rendered buffer (snapshots capture
    /// text only): the highlight modifiers land on exactly the matched
    /// text of an emoji-ZWJ-prefixed row, and not on its neighbors.
    #[test]
    fn emoji_row_highlight_styles_cover_exactly_the_matched_text() {
        use ratatui::style::Modifier;
        let mut state = plain_state(
            &[
                "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f466} fix parser",
                "unrelated row",
            ],
            "1..",
            false,
        );
        state.term_w = 40;
        state.term_h = 24;
        for c in "fix".chars() {
            handle_event(&mut state, InputEvent::Char(c));
        }
        let LayoutMode::Inline { height } = choose_layout(&state) else {
            panic!("expected inline layout");
        };
        let terminal = render_frame(&state, None, 40, height);
        let buffer = terminal.backend().buffer();
        // Row 0 is the (selected) matched row. Collect its cells.
        let cells: Vec<(String, Style)> = (0..buffer.area().width)
            .map(|x| {
                let cell = &buffer[(x, 0)];
                (cell.symbol().to_string(), cell.style())
            })
            .collect();
        // The underlined cells spell exactly the matched text.
        let underlined: String = cells
            .iter()
            .filter(|(_, style)| style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(underlined, "fix");
        // Matched cells carry the full highlight (bold + underline)…
        let f_style = cells
            .iter()
            .find(|(symbol, _)| symbol == "f")
            .map(|(_, style)| *style)
            .expect("matched 'f' cell present");
        assert!(f_style.add_modifier.contains(Modifier::BOLD));
        assert!(f_style.add_modifier.contains(Modifier::UNDERLINED));
        // …while the adjacent unmatched cells (the emoji grapheme and
        // the first char of "parser") do not carry the underline.
        for neighbor in ["\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f466}", "p"] {
            let style = cells
                .iter()
                .find(|(symbol, _)| symbol == neighbor)
                .map(|(_, style)| *style)
                .unwrap_or_else(|| panic!("cell {neighbor:?} present"));
            assert!(
                !style.add_modifier.contains(Modifier::UNDERLINED),
                "unmatched {neighbor:?} must not be underlined"
            );
        }
    }

    #[test]
    fn snapshot_preview_error_pane() {
        let mut state = preview_state(&["s1\tonly session"], Side::Up);
        state.term_w = 60;
        state.term_h = 14;
        let view = PreviewView {
            title: "preview",
            body: PreviewBody::Error("error: session file unreadable"),
        };
        let terminal = render_frame(&state, Some(&view), 60, 14);
        insta::assert_snapshot!(terminal.backend());
    }
}
