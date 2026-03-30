//! Main application state and event loop for the trace TUI.

use std::time::Instant;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::TuiConfig;
use crate::model::{self, GraphLine, Mode, SelectionMode, StepEntry, TextRedaction};
use crate::redact;
use crate::render;
use toolpath::v1;

/// Tracks an in-progress mouse drag for text selection.
#[derive(Clone)]
struct MouseDrag {
    step_index: usize,
    json_pointer: String,
    start_offset: usize,
}

/// Undo entry.
enum UndoAction {
    ToggleInclude { index: usize, was_included: bool },
    RangeExclude { indices: Vec<usize> },
    RangeInclude { indices: Vec<usize> },
    IncludeAll { previously_excluded: Vec<usize> },
    TextRedact { step_index: usize },
}

/// The main trace TUI application.
pub struct TraceTuiApp {
    /// Configuration (app name, etc.).
    pub config: TuiConfig,
    /// The original path (never mutated).
    pub path: v1::Path,
    /// Display entries.
    pub entries: Vec<StepEntry>,
    /// DAG graph layout.
    pub graph_layout: Vec<GraphLine>,
    /// Cursor position (step index).
    pub cursor: usize,
    /// Sub-line within the current step: 0 = summary line, 1..N = field lines.
    pub sub_line: usize,
    /// Scroll offset (in display lines).
    pub scroll: usize,
    /// Selection mode.
    pub selection: SelectionMode,
    /// UI mode.
    pub mode: Mode,
    /// Whether redaction features are enabled.
    pub redact_mode: bool,
    /// Flash message and timestamp.
    pub flash: Option<(String, Instant)>,
    /// Character position within the current field value (when sub_line > 0).
    pub text_col: usize,
    /// Visible height of the step list area (updated each render).
    pub visible_height: usize,
    /// Visible width of the step list area (updated each render).
    pub visible_width: usize,
    /// In-progress mouse drag state.
    mouse_drag: Option<MouseDrag>,
    /// Undo stack.
    undo_stack: Vec<UndoAction>,
    /// Last search matches (persisted after exiting search mode for n/N navigation).
    last_search_matches: Vec<usize>,
    /// Whether the user exported.
    exported: Option<String>,
}

impl TraceTuiApp {
    pub fn new(path: v1::Path, redact_mode: bool, config: TuiConfig) -> Self {
        let entries = model::build_entries(&path.steps);
        let graph_layout = model::compute_graph_layout(&path.steps, &path.path.head);
        Self {
            config,
            path,
            entries,
            graph_layout,
            cursor: 0,
            sub_line: 0,
            scroll: 0,
            selection: SelectionMode::Normal,
            mode: Mode::Normal,
            redact_mode,
            flash: None,
            text_col: 0,
            visible_height: 24,
            visible_width: 120,
            mouse_drag: None,
            undo_stack: Vec::new(),
            last_search_matches: Vec::new(),
            exported: None,
        }
    }

    /// Run the event loop. Returns the exported JSON string if the user chose to export.
    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<Option<String>>
    where
        B::Error: Send + Sync + 'static,
    {
        loop {
            terminal.draw(|frame| render::render(&mut *self, frame))?;

            // Clear expired flash messages.
            if let Some((_, ts)) = &self.flash
                && ts.elapsed().as_secs() >= 3
            {
                self.flash = None;
            }

            if event::poll(std::time::Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key(key) {
                            return Ok(self.exported.take());
                        }
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Handle a key event. Returns true if the app should exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match &self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return false;
            }
            Mode::Search { .. } => {
                return self.handle_search_key(key);
            }
            Mode::Confirm => {
                return self.handle_confirm_key(key);
            }
            Mode::Normal => {}
        }

        // Handle visual char mode separately — it intercepts most keys.
        if let SelectionMode::Visual { .. } = &self.selection {
            return self.handle_visual_char_key(key);
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let multiplier = if shift { 5 } else { 1 };

        // V (visual line) — redact mode only.
        let is_v_upper = key.code == KeyCode::Char('V');
        let is_v_shifted = key.code == KeyCode::Char('v') && shift;
        if (is_v_upper || is_v_shifted) && self.redact_mode {
            self.selection = SelectionMode::VisualLine {
                anchor: self.cursor,
                cursor: self.cursor,
            };
            return false;
        }

        match key.code {
            // Quit.
            KeyCode::Char('q') | KeyCode::Esc => {
                if matches!(self.selection, SelectionMode::VisualLine { .. }) {
                    self.selection = SelectionMode::Normal;
                    return false;
                }
                return true;
            }

            // Navigation.
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(multiplier as isize),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-(multiplier as isize)),
            KeyCode::Char('J') => self.move_cursor(5),
            KeyCode::Char('K') => self.move_cursor(-5),
            KeyCode::Char('g') => {
                self.cursor = 0;
                self.sub_line = 0;
                self.text_col = 0;
                self.ensure_cursor_visible();
            }
            KeyCode::Char('G') => {
                self.cursor = self.entries.len().saturating_sub(1);
                self.sub_line = 0;
                self.text_col = 0;
                self.ensure_cursor_visible();
            }

            // h/l: text cursor when inside expanded content, expand/collapse on summary line.
            KeyCode::Char('h') | KeyCode::Left if self.sub_line > 0 => {
                self.text_col = self.text_col.saturating_sub(multiplier);
            }
            KeyCode::Char('l') | KeyCode::Right if self.sub_line > 0 => {
                if let Some(len) = self.current_field_value_len() {
                    self.text_col = (self.text_col + multiplier).min(len);
                }
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                // On summary line: expand.
                if let Some(entry) = self.entries.get_mut(self.cursor) {
                    entry.expanded = true;
                }
            }
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                // On summary line: collapse.
                if let Some(entry) = self.entries.get_mut(self.cursor) {
                    entry.expanded = false;
                }
                self.sub_line = 0;
                self.text_col = 0;
            }

            // Visual char mode — v (lowercase, no shift), redact mode only.
            KeyCode::Char('v') if self.redact_mode => {
                self.enter_visual_char_mode();
            }

            // Help.
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
            }

            // Search.
            KeyCode::Char('/') => {
                self.mode = Mode::Search {
                    query: String::new(),
                    cursor: 0,
                    matches: Vec::new(),
                };
            }
            KeyCode::Char('n') => self.next_search_match(false),
            KeyCode::Char('N') => self.next_search_match(true),

            // Redaction action keys — redact mode only.
            _ if self.redact_mode => self.handle_redact_key(key),
            _ => {}
        }
        false
    }

    fn handle_redact_key(&mut self, key: KeyEvent) {
        match key.code {
            // Toggle include/exclude.
            KeyCode::Char(' ') => {
                match &self.selection {
                    SelectionMode::Normal => {
                        self.toggle_include(self.cursor);
                    }
                    SelectionMode::VisualLine { anchor, cursor } => {
                        let lo = (*anchor).min(*cursor);
                        let hi = (*anchor).max(*cursor);
                        let indices: Vec<usize> = (lo..=hi).collect();
                        // Toggle: if any are included, exclude all; otherwise include all.
                        let any_included = indices.iter().any(|&i| self.entries[i].included);
                        for &i in &indices {
                            self.entries[i].included = !any_included;
                        }
                        if any_included {
                            self.undo_stack.push(UndoAction::RangeExclude { indices });
                        } else {
                            self.undo_stack.push(UndoAction::RangeInclude { indices });
                        }
                        self.selection = SelectionMode::Normal;
                    }
                    _ => {}
                }
            }

            // Exclude.
            KeyCode::Char('d') => self.exclude_action(),

            // Include.
            KeyCode::Char('i') => self.include_action(),

            // Exclude everything except selection (visual line).
            KeyCode::Char('D') if matches!(self.selection, SelectionMode::VisualLine { .. }) => {
                if let SelectionMode::VisualLine { anchor, cursor } = &self.selection {
                    let lo = (*anchor).min(*cursor);
                    let hi = (*anchor).max(*cursor);
                    let mut excluded = Vec::new();
                    for i in 0..self.entries.len() {
                        if (i < lo || i > hi) && self.entries[i].included {
                            self.entries[i].included = false;
                            excluded.push(i);
                        }
                    }
                    self.undo_stack
                        .push(UndoAction::RangeExclude { indices: excluded });
                    self.selection = SelectionMode::Normal;
                    self.flash("Kept only selected range");
                }
            }

            // Exclude to end.
            KeyCode::Char('D') => {
                let mut excluded = Vec::new();
                for i in self.cursor..self.entries.len() {
                    if self.entries[i].included {
                        self.entries[i].included = false;
                        excluded.push(i);
                    }
                }
                self.undo_stack
                    .push(UndoAction::RangeExclude { indices: excluded });
                self.flash("Excluded to end");
            }

            // Exclude to beginning.
            KeyCode::Char('B') => {
                let mut excluded = Vec::new();
                for i in 0..=self.cursor {
                    if self.entries[i].included {
                        self.entries[i].included = false;
                        excluded.push(i);
                    }
                }
                self.undo_stack
                    .push(UndoAction::RangeExclude { indices: excluded });
                self.flash("Excluded to beginning");
            }

            // Include all.
            KeyCode::Char('a') => {
                let previously_excluded: Vec<usize> = self
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| !e.included)
                    .map(|(i, _)| i)
                    .collect();
                for entry in &mut self.entries {
                    entry.included = true;
                }
                self.undo_stack.push(UndoAction::IncludeAll {
                    previously_excluded,
                });
                self.flash("Included all steps");
            }

            // Undo.
            KeyCode::Char('u') => self.undo(),

            // Export.
            KeyCode::Char('e') => {
                self.mode = Mode::Confirm;
            }

            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => {
                let doc = redact::build_redacted_document(&self.path, &self.entries);
                match doc.to_json_pretty() {
                    Ok(json) => {
                        self.exported = Some(json);
                        return true;
                    }
                    Err(e) => {
                        self.flash(&format!("Export error: {e}"));
                        self.mode = Mode::Normal;
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        false
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => {
                // Finalize search, jump to first match, persist matches for n/N.
                if let Mode::Search { matches, .. } = &self.mode {
                    self.last_search_matches = matches.clone();
                    if let Some(&first) = matches.first() {
                        self.cursor = first;
                        self.ensure_cursor_visible();
                    }
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Esc => {
                // Persist matches even on Esc so n/N still works.
                if let Mode::Search { matches, .. } = &self.mode {
                    self.last_search_matches = matches.clone();
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                if let Mode::Search { query, .. } = &mut self.mode {
                    query.pop();
                }
                self.update_search_matches();
            }
            KeyCode::Char(c) => {
                if let Mode::Search { query, .. } = &mut self.mode {
                    query.push(c);
                }
                self.update_search_matches();
            }
            _ => {}
        }
        false
    }

    fn update_search_matches(&mut self) {
        if let Mode::Search { query, matches, .. } = &mut self.mode {
            let q = query.to_lowercase();
            *matches = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.summary.to_lowercase().contains(&q)
                        || e.actor_label.to_lowercase().contains(&q)
                        || e.step.step.actor.to_lowercase().contains(&q)
                })
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn next_search_match(&mut self, reverse: bool) {
        let matches = match &self.mode {
            Mode::Search { matches, .. } => matches.clone(),
            _ => self.last_search_matches.clone(),
        };
        if matches.is_empty() {
            return;
        }
        if reverse {
            let prev = matches.iter().rev().find(|&&i| i < self.cursor);
            self.cursor = prev.copied().unwrap_or(*matches.last().unwrap());
        } else {
            let next = matches.iter().find(|&&i| i > self.cursor);
            self.cursor = next.copied().unwrap_or(matches[0]);
        }
        self.ensure_cursor_visible();
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_cursor(-3),
            MouseEventKind::ScrollDown => self.move_cursor(3),
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                let row = mouse.row as usize;
                let col = mouse.column as usize;
                if row < 1 {
                    return;
                }
                let display_line = self.scroll + row - 1;
                let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);

                if let Some((step_idx, sub)) = self.display_line_to_cursor(display_line) {
                    self.cursor = step_idx;
                    self.sub_line = sub;

                    // If clicking on a field line, start text drag.
                    if sub > 0
                        && let Some(fl) = self.get_field_line_at(step_idx, sub)
                        && let Some(ptr) = fl.json_pointer.clone()
                    {
                        let char_offset = self.col_to_char_offset(col, &fl);
                        self.mouse_drag = Some(MouseDrag {
                            step_index: step_idx,
                            json_pointer: ptr.clone(),
                            start_offset: char_offset,
                        });
                        self.selection = SelectionMode::Visual {
                            step_index: step_idx,
                            json_pointer: ptr,
                            anchor: char_offset,
                            cursor: char_offset,
                        };
                        return;
                    }

                    // Step-level click.
                    if shift {
                        match &self.selection {
                            SelectionMode::VisualLine { anchor, .. } => {
                                self.selection = SelectionMode::VisualLine {
                                    anchor: *anchor,
                                    cursor: step_idx,
                                };
                            }
                            _ => {
                                self.selection = SelectionMode::VisualLine {
                                    anchor: step_idx,
                                    cursor: step_idx,
                                };
                            }
                        }
                    } else {
                        self.selection = SelectionMode::Normal;
                    }
                }
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                if let Some(ref drag) = self.mouse_drag.clone() {
                    let row = mouse.row as usize;
                    let col = mouse.column as usize;
                    if row < 1 {
                        return;
                    }
                    let display_line = self.scroll + row - 1;
                    if let Some((step_idx, sub)) = self.display_line_to_cursor(display_line)
                        && step_idx == drag.step_index
                        && sub > 0
                        && let Some(fl) = self.get_field_line_at(step_idx, sub)
                        && fl.json_pointer.as_deref() == Some(&drag.json_pointer)
                    {
                        let char_offset = self.col_to_char_offset(col, &fl);
                        self.selection = SelectionMode::Visual {
                            step_index: drag.step_index,
                            json_pointer: drag.json_pointer.clone(),
                            anchor: drag.start_offset,
                            cursor: char_offset,
                        };
                    }
                }
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                self.mouse_drag = None;
            }
            _ => {}
        }
    }

    /// Get the FieldLine at a given (step_index, sub_line). sub_line must be >= 1.
    fn get_field_line_at(&self, step_idx: usize, sub_line: usize) -> Option<model::FieldLine> {
        if sub_line == 0 {
            return None;
        }
        let field_lines =
            model::build_field_lines(&self.entries[step_idx].step, self.field_wrap_width());
        field_lines.into_iter().nth(sub_line - 1)
    }

    /// Convert a terminal column to a char offset within a field line's value.
    fn col_to_char_offset(&self, col: usize, fl: &model::FieldLine) -> usize {
        let indent_len: usize = 9; // matches render indent
        // fl.text is like "  key: value" or "    value_line".
        // The value starts at the label end.
        let label_end = fl.text.find(": ").map(|p| p + 2).unwrap_or(0);
        let value_col_start = indent_len + label_end;
        if col > value_col_start {
            fl.value_offset + (col - value_col_start).min(fl.value_len)
        } else {
            fl.value_offset
        }
    }

    /// Move cursor by `delta` display lines (positive = down, negative = up).
    fn move_cursor(&mut self, delta: isize) {
        let abs = delta.unsigned_abs();
        let down = delta > 0;
        for _ in 0..abs {
            if down {
                self.move_cursor_down();
            } else {
                self.move_cursor_up();
            }
        }

        // Update visual line cursor if in visual line mode.
        if let SelectionMode::VisualLine { cursor, .. } = &mut self.selection {
            *cursor = self.cursor;
        }

        self.ensure_cursor_visible();
    }

    fn move_cursor_down(&mut self) {
        let entry = &self.entries[self.cursor];
        if entry.expanded {
            let n = self.field_line_count(self.cursor);
            if self.sub_line < n {
                self.sub_line += 1;
                self.text_col = 0;
                return;
            }
        }
        // Move to next step.
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
            self.sub_line = 0;
            self.text_col = 0;
        }
    }

    fn move_cursor_up(&mut self) {
        if self.sub_line > 0 {
            self.sub_line -= 1;
            self.text_col = 0;
            return;
        }
        // Move to previous step.
        if self.cursor > 0 {
            self.cursor -= 1;
            let entry = &self.entries[self.cursor];
            if entry.expanded {
                self.sub_line = self.field_line_count(self.cursor);
            } else {
                self.sub_line = 0;
            }
            self.text_col = 0;
        }
    }

    /// Length of the field value at the current cursor position, if on a field line.
    fn current_field_value_len(&self) -> Option<usize> {
        if self.sub_line == 0 {
            return None;
        }
        let fl = self.get_field_line_at(self.cursor, self.sub_line)?;
        let ptr = fl.json_pointer.as_ref()?;
        self.get_field_value(&self.cursor, ptr).map(|v| v.len())
    }

    /// Max width for value text in field lines (accounts for indent + label space).
    fn field_wrap_width(&self) -> usize {
        self.visible_width.saturating_sub(20).max(40) // indent(9) + label(~10) + margin
    }

    /// Number of field lines for a step (0 if not expanded).
    fn field_line_count(&self, step_idx: usize) -> usize {
        model::build_field_lines(&self.entries[step_idx].step, self.field_wrap_width()).len()
    }

    fn ensure_cursor_visible(&mut self) {
        let cursor_line = self.cursor_display_line();
        if cursor_line < self.scroll {
            self.scroll = cursor_line;
        } else if cursor_line >= self.scroll + self.visible_height {
            self.scroll = cursor_line.saturating_sub(self.visible_height.saturating_sub(1));
        }
    }

    /// Compute the display line index for the current cursor position.
    fn cursor_display_line(&self) -> usize {
        let mut line = 0;
        for (i, entry) in self.entries.iter().enumerate() {
            if i == self.cursor {
                return line + self.sub_line;
            }
            line += 1; // summary line
            if entry.expanded {
                line += self.field_line_count(i);
            }
        }
        line
    }

    /// Map a display line to (step_index, sub_line).
    fn display_line_to_cursor(&self, target: usize) -> Option<(usize, usize)> {
        let mut line = 0;
        for (i, entry) in self.entries.iter().enumerate() {
            if line == target {
                return Some((i, 0));
            }
            line += 1;
            if entry.expanded {
                let n = self.field_line_count(i);
                for sub in 0..n {
                    if line == target {
                        return Some((i, sub + 1));
                    }
                    line += 1;
                }
            }
        }
        None
    }

    fn toggle_include(&mut self, idx: usize) {
        if idx < self.entries.len() {
            let was_included = self.entries[idx].included;
            self.entries[idx].included = !was_included;
            self.undo_stack.push(UndoAction::ToggleInclude {
                index: idx,
                was_included,
            });
        }
    }

    fn exclude_action(&mut self) {
        match &self.selection {
            SelectionMode::VisualLine { anchor, cursor } => {
                let lo = (*anchor).min(*cursor);
                let hi = (*anchor).max(*cursor);
                let indices: Vec<usize> = (lo..=hi).filter(|&i| self.entries[i].included).collect();
                for &i in &indices {
                    self.entries[i].included = false;
                }
                self.undo_stack.push(UndoAction::RangeExclude { indices });
                self.selection = SelectionMode::Normal;
            }
            SelectionMode::Normal => {
                if self.entries[self.cursor].included {
                    self.toggle_include(self.cursor);
                }
            }
            _ => {}
        }
    }

    fn include_action(&mut self) {
        match &self.selection {
            SelectionMode::VisualLine { anchor, cursor } => {
                let lo = (*anchor).min(*cursor);
                let hi = (*anchor).max(*cursor);
                let indices: Vec<usize> =
                    (lo..=hi).filter(|&i| !self.entries[i].included).collect();
                for &i in &indices {
                    self.entries[i].included = true;
                }
                self.undo_stack.push(UndoAction::RangeInclude { indices });
                self.selection = SelectionMode::Normal;
            }
            SelectionMode::Normal => {
                if !self.entries[self.cursor].included {
                    self.toggle_include(self.cursor);
                }
            }
            _ => {}
        }
    }

    /// Enter visual char mode on the field under the cursor (or first field if on summary).
    /// Auto-expands the step if collapsed.
    fn enter_visual_char_mode(&mut self) {
        let idx = self.cursor;
        if idx >= self.entries.len() {
            return;
        }
        // Auto-expand if collapsed.
        if !self.entries[idx].expanded {
            self.entries[idx].expanded = true;
            self.sub_line = 1;
            self.text_col = 0;
        }
        let field_lines =
            model::build_field_lines(&self.entries[idx].step, self.field_wrap_width());

        // If sub_line > 0, try to use that specific field line.
        let target_fl = if self.sub_line > 0 && self.sub_line <= field_lines.len() {
            let fl = &field_lines[self.sub_line - 1];
            if fl.json_pointer.is_some() {
                Some(fl)
            } else {
                // On a structural label — find the next field with a pointer.
                field_lines[self.sub_line..]
                    .iter()
                    .find(|fl| fl.json_pointer.is_some())
            }
        } else {
            // On summary line — use first string field.
            field_lines.iter().find(|fl| fl.json_pointer.is_some())
        };

        if let Some(fl) = target_fl {
            let ptr = fl.json_pointer.as_ref().unwrap().clone();
            self.selection = SelectionMode::Visual {
                step_index: idx,
                json_pointer: ptr,
                anchor: self.text_col,
                cursor: self.text_col,
            };
        } else {
            self.flash("No editable fields");
        }
    }

    /// Handle keys while in visual char mode. All movement extends selection.
    fn handle_visual_char_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Exit visual mode, preserve text_col from cursor position.
                if let SelectionMode::Visual { cursor, .. } = &self.selection {
                    self.text_col = *cursor;
                }
                self.selection = SelectionMode::Normal;
            }
            // Move cursor within the value — always extends selection.
            KeyCode::Char('l') | KeyCode::Right => {
                self.visual_char_move(1);
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.visual_char_move(-1);
            }
            // Move to next/prev field.
            KeyCode::Char('j') | KeyCode::Down => {
                self.visual_char_next_field(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.visual_char_next_field(-1);
            }
            // Word-wise movement.
            KeyCode::Char('w') => {
                self.visual_char_word_forward();
            }
            KeyCode::Char('b') => {
                self.visual_char_word_backward();
            }
            // Select all in current field.
            KeyCode::Char('A') => {
                self.visual_char_select_all();
            }
            // Home / End.
            KeyCode::Char('0') | KeyCode::Home => {
                self.visual_char_set_cursor(0);
            }
            KeyCode::Char('$') | KeyCode::End => {
                if let Some(len) = self.visual_char_field_len() {
                    self.visual_char_set_cursor(len);
                }
            }
            // Apply redaction.
            KeyCode::Char('d') | KeyCode::Char('x') => {
                self.apply_visual_redaction();
            }
            _ => {}
        }
        false
    }

    /// Move visual mode cursor by delta. Anchor stays fixed (always extends).
    /// Skips newline characters so the cursor doesn't land on invisible positions.
    fn visual_char_move(&mut self, delta: isize) {
        if let SelectionMode::Visual {
            step_index,
            json_pointer,
            anchor,
            cursor,
        } = self.selection.clone()
        {
            let value = match self.get_field_value(&step_index, &json_pointer) {
                Some(v) => v,
                None => return,
            };
            let len = value.len();
            let mut new_cursor = (cursor as isize + delta).max(0).min(len as isize) as usize;
            // Skip over newline characters.
            let bytes = value.as_bytes();
            if delta > 0 {
                while new_cursor < len && bytes[new_cursor] == b'\n' {
                    new_cursor += 1;
                }
            } else if delta < 0 {
                while new_cursor > 0 && bytes[new_cursor] == b'\n' {
                    new_cursor -= 1;
                }
            }
            self.selection = SelectionMode::Visual {
                step_index,
                json_pointer,
                anchor,
                cursor: new_cursor,
            };
        }
    }

    /// Set visual mode cursor to a specific position. Anchor stays fixed.
    fn visual_char_set_cursor(&mut self, pos: usize) {
        if let SelectionMode::Visual {
            step_index,
            json_pointer,
            anchor,
            ..
        } = self.selection.clone()
        {
            self.selection = SelectionMode::Visual {
                step_index,
                json_pointer,
                anchor,
                cursor: pos,
            };
        }
    }

    fn visual_char_select_all(&mut self) {
        if let SelectionMode::Visual {
            step_index,
            json_pointer,
            ..
        } = self.selection.clone()
        {
            let len = self.visual_char_field_len_for(&step_index, &json_pointer);
            self.selection = SelectionMode::Visual {
                step_index,
                json_pointer,
                anchor: 0,
                cursor: len,
            };
        }
    }

    fn visual_char_word_forward(&mut self) {
        if let SelectionMode::Visual {
            step_index,
            ref json_pointer,
            cursor,
            ..
        } = self.selection.clone()
            && let Some(value) = self.get_field_value(&step_index, json_pointer)
        {
            // Work in byte offsets to stay consistent with the rest of the system.
            let bytes = value.as_bytes();
            let mut pos = cursor;
            // Skip non-whitespace.
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
                // Advance past any multi-byte char.
                while pos < bytes.len() && !value.is_char_boundary(pos) {
                    pos += 1;
                }
            }
            // Skip whitespace.
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            self.visual_char_set_cursor(pos);
        }
    }

    fn visual_char_word_backward(&mut self) {
        if let SelectionMode::Visual {
            step_index,
            ref json_pointer,
            cursor,
            ..
        } = self.selection.clone()
            && let Some(value) = self.get_field_value(&step_index, json_pointer)
        {
            // Work in byte offsets to stay consistent with the rest of the system.
            let bytes = value.as_bytes();
            let mut pos = cursor;
            // Skip whitespace backward.
            while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
                pos -= 1;
            }
            // Skip non-whitespace backward.
            while pos > 0 && !bytes[pos - 1].is_ascii_whitespace() {
                pos -= 1;
                // Retreat to char boundary.
                while pos > 0 && !value.is_char_boundary(pos) {
                    pos -= 1;
                }
            }
            self.visual_char_set_cursor(pos);
        }
    }

    /// Move visual cursor to the next/prev field. Left/right wraps within a field;
    /// j/k jumps between distinct fields.
    fn visual_char_next_field(&mut self, direction: isize) {
        if let SelectionMode::Visual {
            step_index,
            ref json_pointer,
            anchor,
            ..
        } = self.selection.clone()
        {
            let field_lines =
                model::build_field_lines(&self.entries[step_index].step, self.field_wrap_width());

            // Distinct fields only (first line of each field).
            let fields: Vec<&model::FieldLine> = field_lines
                .iter()
                .filter(|fl| fl.json_pointer.is_some() && fl.value_offset == 0)
                .collect();

            let current_idx = fields
                .iter()
                .position(|fl| fl.json_pointer.as_deref() == Some(json_pointer));

            if let Some(idx) = current_idx {
                let new_idx = (idx as isize + direction)
                    .max(0)
                    .min(fields.len() as isize - 1) as usize;
                if let Some(new_ptr) = &fields[new_idx].json_pointer {
                    self.selection = SelectionMode::Visual {
                        step_index,
                        json_pointer: new_ptr.clone(),
                        anchor,
                        cursor: 0,
                    };
                }
            }
        }
    }

    fn visual_char_field_len(&self) -> Option<usize> {
        if let SelectionMode::Visual {
            step_index,
            ref json_pointer,
            ..
        } = self.selection
        {
            Some(self.visual_char_field_len_for(&step_index, json_pointer))
        } else {
            None
        }
    }

    fn visual_char_field_len_for(&self, step_index: &usize, json_pointer: &str) -> usize {
        self.get_field_value(step_index, json_pointer)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    pub fn get_field_value(&self, step_index: &usize, json_pointer: &str) -> Option<String> {
        let entry = self.entries.get(*step_index)?;
        let value = serde_json::to_value(&entry.step).ok()?;
        let target = value.pointer(json_pointer)?;
        target.as_str().map(|s| s.to_string())
    }

    fn apply_visual_redaction(&mut self) {
        if let SelectionMode::Visual {
            step_index,
            ref json_pointer,
            anchor,
            cursor,
        } = self.selection.clone()
        {
            // Selection is inclusive: [min, max]. Convert to exclusive [start, end) for storage.
            let sel_start = anchor.min(cursor);
            let sel_end = anchor.max(cursor) + 1; // +1: inclusive to exclusive

            // Get the original text for undo.
            let original = match self.get_field_value(&step_index, json_pointer) {
                Some(v) if sel_end <= v.len() => v[sel_start..sel_end].to_string(),
                _ => return,
            };

            self.entries[step_index]
                .text_redactions
                .push(TextRedaction {
                    json_pointer: json_pointer.clone(),
                    start: sel_start,
                    end: sel_end,
                    original,
                });
            self.undo_stack.push(UndoAction::TextRedact { step_index });
            self.selection = SelectionMode::Normal;

            let count = sel_end - sel_start;
            self.flash(&format!("Redacted {count} chars"));
        }
    }

    fn undo(&mut self) {
        if let Some(action) = self.undo_stack.pop() {
            match action {
                UndoAction::ToggleInclude {
                    index,
                    was_included,
                } => {
                    self.entries[index].included = was_included;
                }
                UndoAction::RangeExclude { indices } => {
                    for i in indices {
                        self.entries[i].included = true;
                    }
                }
                UndoAction::RangeInclude { indices } => {
                    for i in indices {
                        self.entries[i].included = false;
                    }
                }
                UndoAction::IncludeAll {
                    previously_excluded,
                } => {
                    for i in previously_excluded {
                        self.entries[i].included = false;
                    }
                }
                UndoAction::TextRedact { step_index } => {
                    if let Some(entry) = self.entries.get_mut(step_index) {
                        entry.text_redactions.pop();
                    }
                }
            }
            self.flash("Undone");
        }
    }

    fn flash(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), Instant::now()));
    }
}
