//! Layout ladder and pane rendering for the native picker.
//!
//! The picker adapts to its job: a plain list gets a small *inline*
//! viewport under the shell prompt (Atuin-style); a preview-bearing
//! picker takes over the alternate screen so the preview has room.
//! [`choose_layout`] is the single decision point.

use ratatui::layout::Rect;

use super::preview::{PaneSize, PreviewWindow, Side};
use super::state::AppState;

/// Overall layout preference. `Adaptive` (the default) picks inline
/// for preview-less pickers and fullscreen otherwise; `Inline` and
/// `Fullscreen` force one mode (no in-repo caller forces yet — the
/// knob exists so a future flag can).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutPref {
    Adaptive,
    Inline,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
