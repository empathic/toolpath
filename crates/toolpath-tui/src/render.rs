//! Rendering logic for the trace TUI.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::TraceTuiApp;
use crate::model::{self, Mode, SelectionMode};

/// Render the full trace TUI.
pub fn render(app: &mut TraceTuiApp, frame: &mut Frame) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Min(3),    // step list
        Constraint::Length(1), // status bar
    ])
    .split(area);

    app.visible_height = chunks[1].height as usize;
    app.visible_width = chunks[1].width as usize;
    render_title(app, frame, chunks[0]);
    render_step_list(app, frame, chunks[1]);
    render_status_bar(app, frame, chunks[2]);

    if matches!(app.mode, Mode::Help) {
        render_help_overlay(app, frame, area);
    }
    if matches!(app.mode, Mode::Confirm) {
        render_confirm_overlay(app, frame, area);
    }
    if matches!(app.mode, Mode::Search { .. }) {
        render_search_bar(app, frame, chunks[2]);
    }
}

fn render_title(app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    let included = app.entries.iter().filter(|e| e.included).count();
    let total = app.entries.len();
    let app_name = &app.config.app_name;
    let session_id = &app.path.path.id;
    let title = if app.redact_mode {
        format!(" {app_name} redact — {session_id} — {total} steps ({included} included)")
    } else {
        format!(" {app_name} view — {session_id} — {total} steps")
    };
    let line = Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// Shared highlight bg for hover, visual line, and visual char selection.
const HIGHLIGHT_BG: Color = Color::Rgb(55, 55, 70);

fn render_step_list(app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    let visible_height = area.height as usize;
    let term_width = area.width as usize;
    if visible_height == 0 || app.entries.is_empty() {
        return;
    }

    // Collect all visible lines (including expanded content).
    let mut lines: Vec<Line> = Vec::new();
    let mut step_line_map: Vec<(usize, bool)> = Vec::new(); // (entry_index, is_summary_line)

    for (i, entry) in app.entries.iter().enumerate() {
        // Build the summary line.
        let is_cursor = i == app.cursor && app.sub_line == 0;
        let is_in_visual_range = match &app.selection {
            SelectionMode::VisualLine { anchor, cursor } => {
                let lo = (*anchor).min(*cursor);
                let hi = (*anchor).max(*cursor);
                i >= lo && i <= hi
            }
            _ => false,
        };

        let mut spans = Vec::new();

        // Cursor / visual-line marker.
        if is_cursor {
            spans.push(Span::styled(
                " > ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if is_in_visual_range {
            spans.push(Span::styled(" > ", Style::default().fg(Color::Cyan)));
        } else {
            spans.push(Span::raw("   "));
        }

        // Redaction gutter.
        if !entry.included {
            spans.push(Span::styled(" - ", Style::default().fg(Color::Red)));
        } else {
            spans.push(Span::raw("   "));
        }

        // DAG graph column.
        if let Some(graph_line) = app.graph_layout.get(i) {
            for seg in &graph_line.segments {
                let style = if seg.ch == "┆" {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                spans.push(Span::styled(seg.ch, style));
            }
            spans.push(Span::raw(" "));
        }

        // Actor label.
        let actor_style = match entry.actor_label.as_str() {
            "user" => Style::default().fg(Color::Green),
            "assistant" => Style::default().fg(Color::Blue),
            "policy" => Style::default().fg(Color::Magenta),
            _ => Style::default().fg(Color::White),
        };
        let dimmed = !entry.included;
        let actor_style = if dimmed {
            actor_style.add_modifier(Modifier::DIM)
        } else {
            actor_style
        };
        spans.push(Span::styled(
            format!("{:<10}", entry.actor_label),
            actor_style,
        ));

        // Summary.
        let summary_style = if dimmed {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        spans.push(Span::styled(&entry.summary, summary_style));

        // Timestamp.
        let ts = &entry.step.step.timestamp;
        let ts_short = ts
            .find('T')
            .map(|i| &ts[i + 1..])
            .unwrap_or(ts)
            .trim_end_matches('Z');
        spans.push(Span::styled(
            format!("  {ts_short}"),
            Style::default().fg(Color::DarkGray),
        ));

        // Pad to terminal width so highlight covers the full row.
        let content_len: usize = spans.iter().map(|s| s.content.len()).sum();
        if content_len < term_width {
            spans.push(Span::raw(" ".repeat(term_width - content_len)));
        }

        let mut line = Line::from(spans);
        if is_in_visual_range || is_cursor {
            line = line.style(Style::default().bg(HIGHLIGHT_BG));
        }

        lines.push(line);
        step_line_map.push((i, true));

        // Expanded content — field-oriented view with json_pointer tracking.
        if entry.expanded {
            let wrap_width = term_width.saturating_sub(20).max(40);
            let field_lines = model::build_field_lines(&entry.step, wrap_width);
            let indent = "         "; // aligns with gutter + cursor marker

            // Collect redaction ranges for this step, grouped by json_pointer.
            let redactions = &entry.text_redactions;

            for (fl_idx, fl) in field_lines.iter().enumerate() {
                let is_field_cursor = i == app.cursor && app.sub_line == fl_idx + 1;
                let is_visual_target = matches!(
                    &app.selection,
                    SelectionMode::Visual { step_index, json_pointer, .. }
                    if *step_index == i && fl.json_pointer.as_deref() == Some(json_pointer.as_str())
                );

                let base_style = if fl.json_pointer.is_some() {
                    if dimmed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::Gray)
                    }
                } else {
                    if dimmed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::DIM)
                    }
                };

                // Get selection range if this line is the visual target.
                let selection = if is_visual_target && fl.json_pointer.is_some() {
                    if let SelectionMode::Visual { anchor, cursor, .. } = &app.selection {
                        Some((*anchor, *cursor))
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Cursor col — show block cursor on the cursor line OR on
                // the visual mode cursor position.
                let cursor_col = if is_field_cursor && selection.is_none() {
                    Some(app.text_col)
                } else if let Some((_, c)) = selection {
                    Some(c) // show cursor at the moving end of visual selection
                } else {
                    None
                };

                let spans =
                    build_field_spans(fl, indent, redactions, base_style, cursor_col, selection);
                let mut line = Line::from(spans);
                if is_field_cursor && (fl.json_pointer.is_none() || fl.value_len == 0) {
                    line = line.style(Style::default().bg(HIGHLIGHT_BG));
                }
                lines.push(line);
                step_line_map.push((i, false));
            }
        }
    }

    // Apply scroll offset.
    let total_lines = lines.len();
    let scroll = app.scroll.min(total_lines.saturating_sub(visible_height));

    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .take(visible_height)
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, area);
}

fn render_status_bar(app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    // Mode indicator with vim-style colored background.
    let (mode_text, mode_style) = match (&app.mode, &app.selection) {
        (Mode::Search { .. }, _) => (
            " SEARCH ",
            Style::default().fg(Color::Black).bg(Color::Green),
        ),
        (Mode::Help, _) => (" HELP ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        (Mode::Confirm, _) => (
            " CONFIRM ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
        (_, SelectionMode::VisualLine { .. }) => (
            " VISUAL LINE ",
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ),
        (_, SelectionMode::Visual { .. }) => (
            " VISUAL ",
            Style::default().fg(Color::Black).bg(Color::Magenta),
        ),
        _ => (
            " NORMAL ",
            Style::default().fg(Color::Black).bg(Color::White),
        ),
    };

    let position_text = match &app.selection {
        SelectionMode::Visual { anchor, cursor, .. } => {
            let sel_len = (*anchor).max(*cursor) - (*anchor).min(*cursor) + 1;
            format!(" col:{cursor} sel:{sel_len}")
        }
        _ if app.sub_line > 0 => format!(" col:{}", app.text_col),
        _ => String::new(),
    };

    let hints = if app.redact_mode {
        "Space:toggle  V:visual-line  v:visual  e:export  ?:help  q:quit"
    } else {
        "Enter:expand  /:search  ?:help  q:quit"
    };

    let flash = app
        .flash
        .as_ref()
        .map(|(msg, _)| format!("  {msg}"))
        .unwrap_or_default();

    let bar_style = Style::default().fg(Color::Black).bg(Color::White);
    let rest = format!("{position_text}  {hints}{flash}");

    let line = Line::from(vec![
        Span::styled(mode_text, mode_style),
        Span::styled(rest, bar_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_help_overlay(_app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    let help_text = vec![
        "Navigation:",
        "  j/↓         Move down",
        "  k/↑         Move up",
        "  J/K         Move by 5",
        "  g/G         Top / Bottom",
        "  Enter       Expand step",
        "  Backspace   Collapse step",
        "  h/l ←/→     Move within text",
        "",
        "Redaction:",
        "  Space       Toggle include/exclude",
        "  V           Visual line mode",
        "  v           Visual char mode (expanded)",
        "  d           Exclude (or redact in visual)",
        "  i           Include",
        "  D           Keep only selection (visual line)",
        "  Shift+D     Exclude to end",
        "  Shift+B     Exclude to beginning",
        "  a           Include all (reset)",
        "  u           Undo",
        "",
        "Actions:",
        "  e           Export",
        "  /           Search",
        "  n/N         Next/prev match",
        "  q/Esc       Quit",
        "  ?           Toggle help",
    ];

    let width = 50.min(area.width.saturating_sub(4));
    let height = (help_text.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    let lines: Vec<Line> = help_text
        .iter()
        .map(|&s| Line::from(Span::raw(s)))
        .collect();

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, popup_area);
    frame.render_widget(paragraph, popup_area);
}

fn render_confirm_overlay(app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    let included = app.entries.iter().filter(|e| e.included).count();
    let excluded = app.entries.len() - included;
    let text_redactions: usize = app.entries.iter().map(|e| e.text_redactions.len()).sum();

    let lines = vec![
        Line::from(""),
        Line::from(format!("  Steps included: {included}")),
        Line::from(format!("  Steps excluded: {excluded}")),
        Line::from(format!("  Text redactions: {text_redactions}")),
        Line::from(""),
        Line::from("  Press Enter to export, Esc to cancel"),
    ];

    let width = 45.min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    let block = Block::default()
        .title(" Export Redacted Trace ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, popup_area);
    frame.render_widget(paragraph, popup_area);
}

/// Build spans for a field line with redaction highlights, block cursor, and selection.
///
/// Handles all three overlapping visual concerns in one pass:
/// - Text redactions: red + strikethrough
/// - Visual selection: HIGHLIGHT_BG
/// - Block cursor: white-on-black
///
/// Priority: cursor > selected+redacted > selected > redacted > normal
fn build_field_spans<'a>(
    fl: &model::FieldLine,
    indent: &str,
    redactions: &[model::TextRedaction],
    base_style: Style,
    cursor_col: Option<usize>,
    selection: Option<(usize, usize)>, // (anchor, cursor) in value coords
) -> Vec<Span<'a>> {
    let full = format!("{indent}{}", fl.text);
    let redact_style = Style::default()
        .fg(Color::Red)
        .add_modifier(Modifier::CROSSED_OUT);
    let sel_style = Style::default().bg(HIGHLIGHT_BG);
    let sel_redact_style = Style::default()
        .fg(Color::Red)
        .bg(HIGHLIGHT_BG)
        .add_modifier(Modifier::CROSSED_OUT);
    let cursor_style = Style::default().bg(Color::White).fg(Color::Black);

    let ptr = match &fl.json_pointer {
        Some(p) => p,
        None => return vec![Span::styled(full, base_style)],
    };
    if fl.value_len == 0 {
        return vec![Span::styled(full, base_style)];
    }

    let value_start_in_display = full.len().saturating_sub(fl.value_len);
    let line_value_end = fl.value_offset + fl.value_len;

    // Redaction ranges clipped to this line.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for r in redactions {
        if r.json_pointer != *ptr {
            continue;
        }
        let r_start = r.start.max(fl.value_offset);
        let r_end = r.end.min(line_value_end);
        if r_start < r_end {
            ranges.push((r_start - fl.value_offset, r_end - fl.value_offset));
        }
    }
    ranges.sort();
    let mut merged_redactions: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        if let Some(last) = merged_redactions.last_mut()
            && s <= last.1
        {
            last.1 = last.1.max(e);
            continue;
        }
        merged_redactions.push((s, e));
    }

    // Selection range clipped to this line (in line-local value coords).
    let sel_range = selection.map(|(a, c)| {
        let s = a.min(c);
        let e = a.max(c);
        let ls = s.max(fl.value_offset).saturating_sub(fl.value_offset);
        let le = e.min(line_value_end).saturating_sub(fl.value_offset);
        (ls, le)
    });

    // Cursor position in display coords.
    let cursor_display = cursor_col.map(|col| {
        let col_in_line = col.saturating_sub(fl.value_offset);
        value_start_in_display + col_in_line.min(fl.value_len)
    });

    // Walk through display string, determine style at each position.
    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut pos = 0;
    while pos < full.len() {
        let style = char_style(
            pos,
            value_start_in_display,
            fl.value_len,
            &merged_redactions,
            sel_range,
            cursor_display,
            base_style,
            redact_style,
            sel_style,
            sel_redact_style,
            cursor_style,
        );

        let start = pos;
        pos += 1;
        while pos < full.len() {
            let next_style = char_style(
                pos,
                value_start_in_display,
                fl.value_len,
                &merged_redactions,
                sel_range,
                cursor_display,
                base_style,
                redact_style,
                sel_style,
                sel_redact_style,
                cursor_style,
            );
            if next_style != style {
                break;
            }
            pos += 1;
        }
        spans.push(Span::styled(full[start..pos].to_string(), style));
    }

    // Cursor at end of value.
    if let Some(cp) = cursor_display
        && cp >= full.len()
    {
        spans.push(Span::styled(" ".to_string(), cursor_style));
    }

    if spans.is_empty() {
        vec![Span::styled(full, base_style)]
    } else {
        spans
    }
}

/// Determine the style for a single character position.
#[allow(clippy::too_many_arguments)]
fn char_style(
    pos: usize,
    value_start: usize,
    value_len: usize,
    redactions: &[(usize, usize)],
    sel_range: Option<(usize, usize)>,
    cursor_display: Option<usize>,
    base: Style,
    redact: Style,
    sel: Style,
    sel_redact: Style,
    cursor: Style,
) -> Style {
    if cursor_display == Some(pos) {
        return cursor;
    }
    let in_value = pos >= value_start && pos < value_start + value_len;
    if !in_value {
        return base;
    }
    let vpos = pos - value_start;
    let is_redacted = redactions.iter().any(|(s, e)| vpos >= *s && vpos < *e);
    let is_selected = sel_range.is_some_and(|(s, e)| vpos >= s && vpos <= e);

    match (is_selected, is_redacted) {
        (true, true) => sel_redact,
        (true, false) => sel,
        (false, true) => redact,
        (false, false) => base,
    }
}

fn render_search_bar(app: &TraceTuiApp, frame: &mut Frame, area: Rect) {
    if let Mode::Search {
        query,
        cursor: _,
        matches,
    } = &app.mode
    {
        let match_info = if matches.is_empty() {
            "no matches".to_string()
        } else {
            format!("{} matches", matches.len())
        };
        let text = format!(" /{query}  ({match_info})");
        let line = Line::from(Span::styled(
            text,
            Style::default().fg(Color::White).bg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), area);
    }
}
