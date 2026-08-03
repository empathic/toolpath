//! The picker's state machine. [`handle_event`] is PURE — no IO, no
//! terminal, no clocks — so the whole key contract is testable by
//! feeding event vectors and asserting on the returned [`PickResult`]
//! and state.

use std::collections::BTreeSet;

use crate::fuzzy::PickResult;

use super::InputEvent;
use super::matcher::{MatchEntry, NucleoMatcher, Row};
use super::preview::PreviewWindow;

/// Reserved extension point: a future PR populates this for the
/// bare-resume session picker (Ctrl-R cycles source filters). Shipping
/// the field now keeps the state shape stable; there is deliberately
/// NO behavior behind it yet.
#[derive(Debug, Clone, Copy)]
pub(super) struct FilterHook;

/// Everything the picker knows. Render reads it; `handle_event`
/// mutates it.
pub(super) struct AppState {
    /// All input rows, in input order. Row indices are stable — marks
    /// and match entries key on them.
    pub rows: Vec<Row>,
    /// Current query text.
    pub query: String,
    /// Byte offset of the cursor within `query` (always on a char
    /// boundary).
    pub cursor: usize,
    /// Current matches, sorted score-desc then row-asc.
    pub matches: Vec<MatchEntry>,
    /// Index into `matches` of the highlighted row.
    pub selected: usize,
    /// Marked rows by ROW index — a `BTreeSet` so iteration yields
    /// input order and marks survive query changes untouched.
    pub marked: BTreeSet<usize>,
    /// Multi-select enabled (Tab/BackTab active).
    pub multi: bool,
    pub prompt: String,
    pub header: Option<String>,
    /// A preview command is configured.
    pub has_preview: bool,
    pub preview_window: PreviewWindow,
    /// Preview pane currently shown (Ctrl-O toggles; fullscreen only).
    pub preview_visible: bool,
    /// Preview scroll offset (Shift-Up/Down).
    pub preview_scroll: u16,
    /// Rows per PgUp/PgDn jump; render keeps it in sync with the list
    /// pane height.
    pub page_rows: usize,
    pub term_w: u16,
    pub term_h: u16,
    matcher: NucleoMatcher,
    /// Dormant Ctrl-R hook — see [`FilterHook`].
    #[allow(dead_code)]
    pub filter_hook: Option<FilterHook>,
}

impl AppState {
    pub fn new(
        rows: Vec<Row>,
        multi: bool,
        prompt: &str,
        header: Option<&str>,
        preview_window: Option<PreviewWindow>,
    ) -> Self {
        let mut matcher = NucleoMatcher::new(&rows);
        let matches = matcher.rematch("");
        Self {
            rows,
            query: String::new(),
            cursor: 0,
            matches,
            selected: 0,
            marked: BTreeSet::new(),
            multi,
            prompt: prompt.to_string(),
            header: header.map(str::to_string),
            has_preview: preview_window.is_some(),
            preview_window: preview_window.unwrap_or_default(),
            preview_visible: preview_window.is_some(),
            preview_scroll: 0,
            page_rows: 10,
            term_w: 80,
            term_h: 24,
            matcher,
            filter_hook: None,
        }
    }

    /// ROW index of the highlighted match, if any.
    pub fn current_row(&self) -> Option<usize> {
        self.matches.get(self.selected).map(|m| m.row)
    }

    fn on_query_changed(&mut self) {
        self.matches = self.matcher.rematch(&self.query);
        self.selected = 0;
        self.preview_scroll = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let max = (self.matches.len() - 1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max) as usize;
        if next != self.selected {
            self.selected = next;
            self.preview_scroll = 0;
        }
    }

    fn prev_char_boundary(&self) -> usize {
        self.query[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_char_boundary(&self) -> usize {
        self.query[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.query.len() {
            let end = self.next_char_boundary();
            self.query.replace_range(self.cursor..end, "");
            self.on_query_changed();
        }
    }

    /// Delete the word before the cursor: trailing whitespace, then
    /// the run of non-whitespace.
    fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let head = &self.query[..self.cursor];
        let trimmed = head.trim_end();
        let start = trimmed
            .rfind(char::is_whitespace)
            .map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8))
            .unwrap_or(0);
        self.query.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.on_query_changed();
    }

    /// The rows Enter would return right now.
    fn accepted_rows(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            // BTreeSet iterates ascending row index = input order.
            return self
                .marked
                .iter()
                .map(|&i| self.rows[i].original.clone())
                .collect();
        }
        self.current_row()
            .map(|r| vec![self.rows[r].original.clone()])
            .unwrap_or_default()
    }
}

/// Advance the state by one input event. Returns `Some` when the
/// picker session is over.
pub(super) fn handle_event(state: &mut AppState, ev: InputEvent) -> Option<PickResult> {
    match ev {
        InputEvent::Char(c) => {
            state.query.insert(state.cursor, c);
            state.cursor += c.len_utf8();
            state.on_query_changed();
        }
        InputEvent::Backspace => {
            if state.cursor > 0 {
                let start = state.prev_char_boundary();
                state.query.replace_range(start..state.cursor, "");
                state.cursor = start;
                state.on_query_changed();
            }
        }
        InputEvent::DeleteForward => state.delete_forward(),
        InputEvent::CtrlD => {
            // fzf parity: Ctrl-D on an empty query cancels; otherwise
            // it's forward delete.
            if state.query.is_empty() {
                return Some(PickResult::Cancelled);
            }
            state.delete_forward();
        }
        InputEvent::CtrlU => {
            if !state.query.is_empty() {
                state.query.clear();
                state.cursor = 0;
                state.on_query_changed();
            }
        }
        InputEvent::CtrlW => state.delete_word_back(),
        InputEvent::Left => state.cursor = state.prev_char_boundary(),
        InputEvent::Right => state.cursor = state.next_char_boundary(),
        InputEvent::Home => state.cursor = 0,
        InputEvent::End => state.cursor = state.query.len(),
        InputEvent::Up => state.move_selection(-1),
        InputEvent::Down => state.move_selection(1),
        InputEvent::PageUp => state.move_selection(-(state.page_rows as isize)),
        InputEvent::PageDown => state.move_selection(state.page_rows as isize),
        InputEvent::Tab => {
            if state.multi {
                if let Some(row) = state.current_row() {
                    if !state.marked.remove(&row) {
                        state.marked.insert(row);
                    }
                    state.move_selection(1);
                }
            }
        }
        InputEvent::BackTab => {
            if state.multi {
                if let Some(row) = state.current_row() {
                    if !state.marked.remove(&row) {
                        state.marked.insert(row);
                    }
                    state.move_selection(-1);
                }
            }
        }
        InputEvent::Enter => {
            let rows = state.accepted_rows();
            return Some(if rows.is_empty() {
                PickResult::NoMatch
            } else {
                PickResult::Selected(rows)
            });
        }
        InputEvent::Esc | InputEvent::CtrlC => return Some(PickResult::Cancelled),
        InputEvent::CtrlO => {
            // Only meaningful in fullscreen layouts — which is exactly
            // when a preview is configured.
            if state.has_preview {
                state.preview_visible = !state.preview_visible;
            }
        }
        InputEvent::CtrlR => {
            // Reserved: FilterHook cycling lands in a future PR.
        }
        InputEvent::ShiftUp => state.preview_scroll = state.preview_scroll.saturating_sub(1),
        InputEvent::ShiftDown => state.preview_scroll = state.preview_scroll.saturating_add(1),
        InputEvent::Resize(w, h) => {
            state.term_w = w;
            state.term_h = h;
        }
        InputEvent::Noop => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::matcher::parse_field_spec;
    use super::super::preview::{PaneSize, Side};
    use super::super::render::{LayoutMode, choose_layout};
    use super::*;

    fn state_of(lines: &[&str], with_nth: &str, multi: bool) -> AppState {
        let spec = parse_field_spec(with_nth).unwrap();
        let rows: Vec<Row> = lines.iter().map(|l| Row::new(l, &spec)).collect();
        AppState::new(rows, multi, "> ", None, None)
    }

    fn type_str(state: &mut AppState, s: &str) {
        for c in s.chars() {
            assert!(handle_event(state, InputEvent::Char(c)).is_none());
        }
    }

    #[test]
    fn enter_with_no_query_returns_first_row_original() {
        let mut s = state_of(&["p1\ts1\tfirst row", "p2\ts2\tsecond row"], "3", false);
        let out = handle_event(&mut s, InputEvent::Enter);
        assert_eq!(
            out,
            Some(PickResult::Selected(vec!["p1\ts1\tfirst row".to_string()]))
        );
    }

    #[test]
    fn typing_filters_and_enter_returns_top_match_original_line() {
        let mut s = state_of(&["p1\ts1\talpha work", "p2\ts2\tbeta work"], "3", false);
        type_str(&mut s, "beta");
        let out = handle_event(&mut s, InputEvent::Enter);
        assert_eq!(
            out,
            Some(PickResult::Selected(vec!["p2\ts2\tbeta work".to_string()]))
        );
    }

    #[test]
    fn enter_with_zero_matches_returns_no_match() {
        let mut s = state_of(&["p1\ts1\talpha", "p2\ts2\tbeta"], "3", false);
        type_str(&mut s, "zzzzqqqq");
        assert!(s.matches.is_empty());
        assert_eq!(
            handle_event(&mut s, InputEvent::Enter),
            Some(PickResult::NoMatch)
        );
    }

    #[test]
    fn esc_returns_cancelled() {
        let mut s = state_of(&["a"], "1..", false);
        assert_eq!(
            handle_event(&mut s, InputEvent::Esc),
            Some(PickResult::Cancelled)
        );
    }

    #[test]
    fn ctrl_c_returns_cancelled() {
        let mut s = state_of(&["a"], "1..", false);
        assert_eq!(
            handle_event(&mut s, InputEvent::CtrlC),
            Some(PickResult::Cancelled)
        );
    }

    #[test]
    fn ctrl_d_on_empty_query_cancels() {
        let mut s = state_of(&["a"], "1..", false);
        assert_eq!(
            handle_event(&mut s, InputEvent::CtrlD),
            Some(PickResult::Cancelled)
        );
        // With a query, Ctrl-D is forward delete, not cancel.
        let mut s = state_of(&["abc"], "1..", false);
        type_str(&mut s, "ab");
        assert!(handle_event(&mut s, InputEvent::Home).is_none());
        assert!(handle_event(&mut s, InputEvent::CtrlD).is_none());
        assert_eq!(s.query, "b");
    }

    #[test]
    fn tab_toggles_mark_and_advances_in_multi_mode() {
        let mut s = state_of(&["one", "two", "three"], "1..", true);
        assert!(handle_event(&mut s, InputEvent::Tab).is_none());
        assert!(s.marked.contains(&0));
        assert_eq!(s.selected, 1);
        // BackTab from here toggles row 1 and retreats.
        assert!(handle_event(&mut s, InputEvent::BackTab).is_none());
        assert!(s.marked.contains(&1));
        assert_eq!(s.selected, 0);
        // Tab on an already-marked row unmarks it.
        assert!(handle_event(&mut s, InputEvent::Tab).is_none());
        assert!(!s.marked.contains(&0));
    }

    #[test]
    fn tab_is_noop_without_multi() {
        let mut s = state_of(&["one", "two"], "1..", false);
        assert!(handle_event(&mut s, InputEvent::Tab).is_none());
        assert!(s.marked.is_empty());
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn enter_returns_marked_rows_in_input_order() {
        let mut s = state_of(&["row zero", "row one", "row two"], "1..", true);
        // Mark row 2 first, then row 0 — result must still be input
        // order (0 before 2).
        handle_event(&mut s, InputEvent::Down);
        handle_event(&mut s, InputEvent::Down);
        handle_event(&mut s, InputEvent::Tab); // marks row 2, advance clamps
        handle_event(&mut s, InputEvent::Up);
        handle_event(&mut s, InputEvent::Up);
        handle_event(&mut s, InputEvent::Tab); // marks row 0
        let out = handle_event(&mut s, InputEvent::Enter);
        assert_eq!(
            out,
            Some(PickResult::Selected(vec![
                "row zero".to_string(),
                "row two".to_string()
            ]))
        );
    }

    #[test]
    fn marks_survive_query_change() {
        let mut s = state_of(&["alpha", "beta", "gamma"], "1..", true);
        handle_event(&mut s, InputEvent::Tab); // mark "alpha"
        type_str(&mut s, "gam");
        // The mark on row 0 survived even though row 0 no longer
        // matches; Enter returns the marked set.
        assert!(s.marked.contains(&0));
        let out = handle_event(&mut s, InputEvent::Enter);
        assert_eq!(out, Some(PickResult::Selected(vec!["alpha".to_string()])));
    }

    #[test]
    fn query_change_resets_selection_to_top() {
        let mut s = state_of(&["aa", "ab", "ac"], "1..", false);
        handle_event(&mut s, InputEvent::Down);
        handle_event(&mut s, InputEvent::Down);
        assert_eq!(s.selected, 2);
        type_str(&mut s, "a");
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn up_down_clamp_at_bounds() {
        let mut s = state_of(&["one", "two"], "1..", false);
        handle_event(&mut s, InputEvent::Up);
        assert_eq!(s.selected, 0);
        handle_event(&mut s, InputEvent::Down);
        handle_event(&mut s, InputEvent::Down);
        handle_event(&mut s, InputEvent::Down);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn page_down_moves_by_page() {
        let lines: Vec<String> = (0..30).map(|i| format!("row {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut s = state_of(&refs, "1..", false);
        s.page_rows = 10;
        handle_event(&mut s, InputEvent::PageDown);
        assert_eq!(s.selected, 10);
        handle_event(&mut s, InputEvent::PageUp);
        assert_eq!(s.selected, 0);
        // Clamps at the end.
        handle_event(&mut s, InputEvent::PageDown);
        handle_event(&mut s, InputEvent::PageDown);
        handle_event(&mut s, InputEvent::PageDown);
        assert_eq!(s.selected, 29);
    }

    #[test]
    fn ctrl_u_clears_query_and_rematches() {
        let mut s = state_of(&["alpha", "beta"], "1..", false);
        type_str(&mut s, "beta");
        assert_eq!(s.matches.len(), 1);
        assert!(handle_event(&mut s, InputEvent::CtrlU).is_none());
        assert_eq!(s.query, "");
        assert_eq!(s.cursor, 0);
        assert_eq!(s.matches.len(), 2);
    }

    #[test]
    fn ctrl_w_deletes_word_before_cursor() {
        let mut s = state_of(&["alpha beta"], "1..", false);
        type_str(&mut s, "alpha beta");
        assert!(handle_event(&mut s, InputEvent::CtrlW).is_none());
        assert_eq!(s.query, "alpha ");
        assert!(handle_event(&mut s, InputEvent::CtrlW).is_none());
        assert_eq!(s.query, "");
    }

    #[test]
    fn cursor_moves_are_char_boundary_safe() {
        let mut s = state_of(&["héllo"], "1..", false);
        type_str(&mut s, "hé");
        // Cursor sits after the multibyte é; Left crosses it cleanly.
        handle_event(&mut s, InputEvent::Left);
        assert_eq!(s.cursor, 1);
        handle_event(&mut s, InputEvent::Right);
        assert_eq!(s.cursor, 1 + 'é'.len_utf8());
        handle_event(&mut s, InputEvent::Backspace);
        assert_eq!(s.query, "h");
    }

    #[test]
    fn hidden_columns_are_not_searchable() {
        // The query text exists only in hidden column 1 of a
        // with_nth "3" row: zero matches, Enter -> NoMatch.
        let mut s = state_of(&["needle-project\tsess\tvisible title"], "3", false);
        type_str(&mut s, "needle-project");
        assert!(s.matches.is_empty());
        assert_eq!(
            handle_event(&mut s, InputEvent::Enter),
            Some(PickResult::NoMatch)
        );
    }

    #[test]
    fn resize_below_width_threshold_switches_side_to_stacked() {
        let spec = parse_field_spec("1..").unwrap();
        let rows = vec![Row::new("one", &spec)];
        let mut s = AppState::new(
            rows,
            false,
            "> ",
            None,
            Some(PreviewWindow {
                side: Side::Right,
                size: PaneSize::Percent(60),
                wrap: super::super::preview::WrapMode::WrapWord,
            }),
        );
        handle_event(&mut s, InputEvent::Resize(120, 30));
        assert_eq!(
            choose_layout(&s),
            LayoutMode::FullscreenSplit {
                side: Side::Right,
                size: PaneSize::Percent(60)
            }
        );
        handle_event(&mut s, InputEvent::Resize(80, 30));
        assert_eq!(
            choose_layout(&s),
            LayoutMode::FullscreenSplit {
                side: Side::Up,
                size: PaneSize::Percent(60)
            }
        );
    }

    #[test]
    fn shift_arrows_scroll_preview() {
        let mut s = state_of(&["one"], "1..", false);
        handle_event(&mut s, InputEvent::ShiftDown);
        handle_event(&mut s, InputEvent::ShiftDown);
        assert_eq!(s.preview_scroll, 2);
        handle_event(&mut s, InputEvent::ShiftUp);
        assert_eq!(s.preview_scroll, 1);
        // Saturates at zero.
        handle_event(&mut s, InputEvent::ShiftUp);
        handle_event(&mut s, InputEvent::ShiftUp);
        assert_eq!(s.preview_scroll, 0);
    }
}
