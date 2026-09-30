//! Paint a [`Screen`] with ratatui. The only module that names
//! ratatui types besides the event loop.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs};

use super::landing::{self, Action, Age, Detail, LANES_MAX, Lanes, Style as LineStyle};
use super::view::Screen;

/// Terminals shorter than this show the page without the detail pane.
const PANE_MIN_HEIGHT: u16 = 20;
/// The lanes show when the page without the detail pane is this tall.
const LANES_MIN_HEIGHT: u16 = 24;
/// The detail pane's lines: the name, the facts, a blank, and the
/// project.
const PANE_HEIGHT: u16 = 4;
/// The label column of the detail pane.
const LABEL_WIDTH: usize = 9;

/// Brand tokens (site/BRAND.md, dark theme).
const ACCENT: Color = Color::Rgb(201, 126, 63);
const BG: Color = Color::Rgb(20, 17, 13);
const ELEVATED: Color = Color::Rgb(37, 31, 24);
const TEXT: Color = Color::Rgb(204, 198, 187);
const TEXT2: Color = Color::Rgb(138, 125, 110);
const DIM: Color = Color::Rgb(92, 83, 70);
const CHIP_ON: Style = Style::new().fg(BG).bg(ACCENT);

/// The session columns right of the name: header, width.
const COLS: [(&str, usize); 2] = [("Updated", 8), ("Duration", 8)];
const COL_GAP: usize = 2;

/// Braille dot bits by column (left, right) and row (top first).
const BRAILLE: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
/// Tracks a lane shows; later tracks share the last.
const TRACKS: usize = 4;

fn cols_width() -> usize {
    COLS.iter().map(|(_, width)| width + COL_GAP).sum()
}

fn age_style(age: Age) -> Style {
    match age {
        Age::Hour => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        Age::Today => Style::new().fg(TEXT),
        Age::Week => Style::new().fg(TEXT2),
        Age::Older => Style::new().fg(DIM),
    }
}

/// Tabs over `options`, the current one filled.
fn tabs(options: &[(String, bool)]) -> Tabs<'_> {
    Tabs::new(options.iter().map(|(label, _)| label.as_str()))
        .select(options.iter().position(|(_, on)| *on))
        .style(Style::new().fg(TEXT2))
        .highlight_style(CHIP_ON.bold())
        .divider(Span::styled("│", Style::new().fg(DIM)))
}

/// The keys line: in each `key action` group, the key stands out.
fn key_hints(keys: &str) -> Line<'_> {
    let mut spans = vec![Span::raw(" ")];
    for group in keys.split("   ").filter(|group| !group.is_empty()) {
        let (key, action) = group.split_once(' ').unwrap_or((group, ""));
        spans.push(Span::styled(key, Style::new().fg(TEXT2).bold()));
        spans.push(Span::styled(format!(" {action}  "), Style::new().fg(DIM)));
    }
    Line::from(spans)
}

/// `text` padded or clipped to `width` columns, right-aligned.
fn right(text: &str, width: usize) -> String {
    format!("{:>width$}", crate::fuzzy::clip_chars(text, width))
}

fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), Style::new().fg(DIM)))
}

/// The column header over the session rows.
fn column_header(width: usize) -> Line<'static> {
    let mut text = format!(
        "{:<name$}",
        "   Session",
        name = width.saturating_sub(cols_width())
    );
    for (label, width) in COLS {
        text.push_str(&" ".repeat(COL_GAP));
        text.push_str(&right(label, width));
    }
    Line::from(Span::styled(text, Style::new().fg(DIM)))
}

/// One line of the page as a row `width` wide. `path_width`: the column a
/// project heading's count starts at.
fn page_row<'a>(
    line: &'a landing::Line,
    on_cursor: bool,
    width: usize,
    path_width: usize,
) -> Line<'a> {
    let marker = if on_cursor {
        Span::styled("›", Style::new().fg(ACCENT).bold())
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![marker, Span::raw(" ".repeat(line.indent))];
    match (&line.cols, line.style) {
        (Some(cols), _) => {
            let room = width
                .saturating_sub(1 + line.indent + cols_width() + 1)
                .max(8);
            let name = crate::fuzzy::clip_chars(&line.text, room);
            let gap = room.saturating_sub(name.chars().count()) + 1;
            let name_style = if on_cursor {
                Style::new().fg(TEXT).bold()
            } else {
                Style::new().fg(TEXT)
            };
            spans.push(Span::styled(name, name_style));
            spans.push(Span::raw(" ".repeat(gap)));
            let cells = [
                (cols.ago.as_str(), age_style(cols.age)),
                (cols.duration.as_str(), Style::new().fg(TEXT2)),
            ];
            for ((text, style), (_, width)) in cells.into_iter().zip(COLS) {
                spans.push(Span::raw(" ".repeat(COL_GAP)));
                spans.push(Span::styled(right(text, width), style));
            }
        }
        (None, LineStyle::Project) => {
            spans.push(Span::styled(
                format!("{:<path_width$}", line.text),
                Style::new().fg(TEXT2),
            ));
            if let Some(count) = line.count {
                spans.push(Span::styled(format!("  {count:>3}"), Style::new().fg(DIM)));
            }
        }
        (None, _) => {
            let color = if on_cursor { TEXT2 } else { DIM };
            spans.push(Span::styled(
                line.text.as_str(),
                Style::new().fg(color).italic(),
            ));
        }
    }
    let row = Line::from(spans);
    if on_cursor {
        row.style(Style::new().bg(ELEVATED))
    } else {
        row
    }
}

/// Each span's track in its lane: the first track free at its start.
/// Past [`TRACKS`] tracks, spans share the last. Returns the tracks
/// and how many the lane needs.
fn tracks(spans: &[(DateTime<Utc>, DateTime<Utc>, usize)]) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&k| spans[k].0);
    let mut ends: Vec<DateTime<Utc>> = Vec::new();
    let mut track = vec![0; spans.len()];
    for k in order {
        let (start, end, _) = spans[k];
        let free = match ends.iter().position(|busy_until| *busy_until < start) {
            Some(free) => free,
            None => {
                ends.push(end);
                ends.len() - 1
            }
        };
        ends[free] = ends[free].max(end);
        track[k] = free.min(TRACKS - 1);
    }
    (track, ends.len())
}

/// The lanes: one row per project, its sessions as bars over the time
/// window, then a time axis. A cell is a braille glyph: two time slots
/// wide and four dot rows tall, split among the lane's tracks, so
/// sessions at once show one above the other. A cell lightens with the
/// sessions in it; the selected session's cells are copper.
fn lane_lines(lanes: &Lanes, width: usize, selected: Option<usize>) -> Vec<Line<'static>> {
    let label_width = lanes
        .rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(8, 32);
    let lane_width = width.saturating_sub(1 + label_width + 2 + 1).max(10);
    let slots = lane_width * 2;
    let total = (lanes.end - lanes.start).num_seconds().max(1) as f64;
    let at = |slot: usize| {
        lanes.start + chrono::Duration::seconds((total * slot as f64 / slots as f64) as i64)
    };
    let column = |t: DateTime<Utc>| {
        ((t - lanes.start).num_seconds() as f64 / total * lane_width as f64) as usize
    };
    let local = |t: DateTime<Utc>| t.with_timezone(&lanes.tz);
    let first_midnight = local(lanes.start)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_local_timezone(lanes.tz)
        .single()
        .expect("a fixed offset has one midnight")
        .with_timezone(&Utc);

    let mut day_breaks = std::collections::HashSet::new();
    let mut midnight = first_midnight + chrono::Duration::days(1);
    while midnight < lanes.end {
        day_breaks.insert(column(midnight));
        midnight += chrono::Duration::days(1);
    }

    let mut out = Vec::new();
    for row in &lanes.rows {
        let mine = selected.is_some_and(|s| row.spans.iter().any(|(_, _, i)| *i == s));
        let label_style = if mine {
            Style::new().fg(TEXT).bold()
        } else {
            Style::new().fg(TEXT2)
        };
        let mut spans = vec![Span::styled(
            format!(
                " {:<label_width$}  ",
                crate::fuzzy::clip_chars(&row.label, label_width)
            ),
            label_style,
        )];
        let (track, track_count) = tracks(&row.spans);
        let dots_per_track = 4 / track_count.clamp(1, TRACKS);
        for cell in 0..lane_width {
            let mut bits = 0u32;
            let mut has_selected = false;
            let mut here = std::collections::HashSet::new();
            for (half, dots) in BRAILLE.iter().enumerate() {
                let (from, to) = (at(2 * cell + half), at(2 * cell + half + 1));
                for (k, (start, end, i)) in row.spans.iter().enumerate() {
                    if *start < to && *end >= from {
                        for dot in track[k] * dots_per_track..(track[k] + 1) * dots_per_track {
                            bits |= dots[dot.min(3)];
                        }
                        has_selected |= Some(*i) == selected;
                        here.insert(*i);
                    }
                }
            }
            let color = match (has_selected, here.len()) {
                (true, _) => ACCENT,
                (false, 1) => DIM,
                (false, 2) => TEXT2,
                (false, _) => TEXT,
            };
            let glyph = char::from_u32(0x2800 + bits).unwrap_or(' ');
            let (glyph, style) = match (bits, day_breaks.contains(&cell)) {
                (0, true) => ('│', Style::new().fg(DIM)),
                (0, false) => ('⠒', Style::new().fg(ELEVATED)),
                (_, true) => (glyph, Style::new().fg(color).bg(ELEVATED)),
                (_, false) => (glyph, Style::new().fg(color)),
            };
            spans.push(Span::styled(glyph.to_string(), style));
        }
        out.push(Line::from(spans));
    }
    out.resize(LANES_MAX, Line::default());

    let mut axis: Vec<char> = vec![' '; lane_width];
    let short = total <= 2.0 * 86_400.0;
    let step = chrono::Duration::hours(if short { 6 } else { 24 });
    let mut tick = first_midnight;
    let mut free_from = 0;
    while tick <= lanes.end {
        if tick >= lanes.start {
            let hour = local(tick).format("%H").to_string();
            let label = if hour == "00" {
                local(tick).format("%a").to_string()
            } else {
                hour
            };
            let at = column(tick);
            if at >= free_from && at + label.len() <= lane_width {
                for (k, c) in label.chars().enumerate() {
                    axis[at + k] = c;
                }
                free_from = at + label.len() + 1;
            }
        }
        tick += step;
    }
    let now = "now";
    if lane_width >= free_from + now.len() {
        for (k, c) in now.chars().enumerate() {
            axis[lane_width - now.len() + k] = c;
        }
    }
    let more = if lanes.more > 0 {
        format!("+{} more", lanes.more)
    } else {
        String::new()
    };
    out.push(Line::from(vec![
        Span::styled(
            format!(
                " {:<label_width$}  ",
                crate::fuzzy::clip_chars(&more, label_width)
            ),
            Style::new().fg(DIM).italic(),
        ),
        Span::styled(axis.into_iter().collect::<String>(), Style::new().fg(DIM)),
    ]));
    out
}

/// The detail pane's lines for `detail`.
fn detail_lines(detail: &Detail) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!("{text:<LABEL_WIDTH$}"), Style::new().fg(DIM));
    let value = |text: &str| Span::styled(text.to_string(), Style::new().fg(TEXT));
    vec![
        Line::from(Span::styled(
            detail.name.clone(),
            Style::new().fg(ACCENT).bold(),
        )),
        Line::from(Span::styled(detail.facts.clone(), Style::new().fg(TEXT2))),
        Line::default(),
        Line::from(vec![label("Project"), value(&detail.project)]),
    ]
}

/// The session page over the whole frame: the title, the time tabs, the
/// lanes, the sessions by project, the detail pane, the filter, and the
/// keys line. `state` is the session list's scroll state, kept across
/// frames: ratatui scrolls it the minimum to show the cursor. With the
/// cursor on the first item, the list scrolls to its top, so the
/// heading over that item shows.
pub fn draw(frame: &mut Frame<'_>, screen: &Screen, state: &mut ListState) {
    let area = frame.area();
    let width = area.width as usize;
    let page = &screen.page;
    frame.render_widget(Block::new().style(Style::new().bg(BG).fg(TEXT)), area);

    let path_width = page
        .sections
        .iter()
        .flat_map(|section| &section.lines)
        .filter(|line| line.style == LineStyle::Project)
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or(0);
    let mut items: Vec<ListItem<'_>> = Vec::new();
    let mut selected = None;
    for (n, section) in page.sections.iter().enumerate() {
        if n > 0 {
            items.push(ListItem::new(Line::default()));
        }
        for line in &section.lines {
            let on_cursor = line.item == Some(screen.cursor);
            if on_cursor {
                selected = Some(items.len());
            }
            items.push(ListItem::new(page_row(line, on_cursor, width, path_width)));
        }
    }

    let selected_session = match page.items.get(screen.cursor) {
        Some(Action::Session(i)) => Some(*i),
        _ => None,
    };
    // The pane's rule and the pane itself.
    let (pane_rule_height, pane_height) = if area.height >= PANE_MIN_HEIGHT {
        (1, PANE_HEIGHT)
    } else {
        (0, 0)
    };
    let lanes = if area.height - pane_rule_height - pane_height >= LANES_MIN_HEIGHT {
        lane_lines(&page.lanes, width, selected_session)
    } else {
        Vec::new()
    };
    let lanes_height = if lanes.is_empty() {
        0
    } else {
        lanes.len() as u16 + 1
    };
    let [
        title_area,
        tabs_area,
        lanes_area,
        rule_area,
        columns_area,
        body,
        pane_rule_area,
        pane_area,
        filter_area,
        keys_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(lanes_height),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(pane_rule_height),
        Constraint::Length(pane_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let title = Line::from(vec![
        Span::styled(" path", Style::new().fg(ACCENT).bold()),
        Span::styled(" resume", Style::new().fg(TEXT).bold()),
    ]);
    frame.render_widget(Paragraph::new(title), title_area);
    frame.render_widget(
        tabs(&page.windows),
        Rect {
            x: tabs_area.x + 1,
            y: tabs_area.y + 1,
            width: tabs_area.width.saturating_sub(1),
            height: 1,
        },
    );
    if !lanes.is_empty() {
        let mut lines = vec![Line::default()];
        lines.extend(lanes);
        frame.render_widget(Paragraph::new(Text::from(lines)), lanes_area);
    }
    frame.render_widget(Paragraph::new(rule(width)), rule_area);
    frame.render_widget(Paragraph::new(column_header(width)), columns_area);

    state.select(selected);
    if screen.cursor == 0 {
        *state.offset_mut() = 0;
    }
    frame.render_stateful_widget(List::new(items), body, state);

    if pane_height > 0 {
        frame.render_widget(Paragraph::new(rule(width)), pane_rule_area);
        if let Some(detail) = &page.detail {
            frame.render_widget(
                Paragraph::new(Text::from(detail_lines(detail))),
                Rect {
                    x: pane_area.x + 1,
                    width: pane_area.width.saturating_sub(2),
                    ..pane_area
                },
            );
        }
    }

    if let Some(filter) = &screen.filter {
        let line = Line::from(vec![
            Span::styled(" / ", Style::new().fg(ACCENT).bold()),
            Span::styled(filter.as_str(), Style::new().fg(TEXT)),
            Span::styled("_", Style::new().fg(DIM)),
        ]);
        frame.render_widget(Paragraph::new(line), filter_area);
    }
    frame.render_widget(Paragraph::new(key_hints(&page.keys)), keys_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_resume::tui::model::Key;
    use crate::cmd_resume::tui::model::tests::{model, session};
    use crate::cmd_resume::tui::view::view;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The screen of `m` as text, one string per terminal row.
    fn paint(m: &crate::cmd_resume::tui::model::Model, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let screen = view(m);
        terminal
            .draw(|frame| draw(frame, &screen, &mut ListState::default()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_page_paints_the_tabs_the_lanes_and_the_rows() {
        let m = model(vec![
            session("/home/u/toolpath", "fix the parser", 1),
            session("/home/u/pathbase", "write the docs", 30),
        ]);
        let rows = paint(&m, 80, 30);
        assert_eq!(rows[0], " path resume");
        assert!(rows[2].contains("today") && rows[2].contains("7 days"));
        assert!(rows[4].starts_with(" ~/toolpath"), "{:?}", rows[4]);
        assert!(rows[10].trim_end().ends_with("now"), "{:?}", rows[10]);
        assert!(rows[12].starts_with("   Session"), "{:?}", rows[12]);
        assert!(rows[12].ends_with("Updated  Duration"), "{:?}", rows[12]);
        assert!(rows[13].starts_with(" ~/toolpath"));
        assert!(rows[14].starts_with("›  fix the parser"), "{:?}", rows[14]);
        assert!(rows[14].ends_with("1h ago      1h00"), "{:?}", rows[14]);
        assert!(rows[17].starts_with("   write the docs"), "{:?}", rows[17]);
        assert_eq!(rows[29], " enter resume  t time  / filter  q quit");
    }

    #[test]
    fn a_short_terminal_drops_the_lanes_and_the_open_filter_shows() {
        let mut m = model(vec![session("/home/u/toolpath", "fix the parser", 1)]);
        m.update(Key::Char('/'));
        m.update(Key::Char('p'));
        let rows = paint(&m, 60, 12);
        assert!(rows[5].starts_with(" ~/toolpath"), "{:?}", rows[5]);
        assert_eq!(rows[10], " / p_");
    }

    #[test]
    fn the_pane_shows_the_session_under_the_cursor() {
        let m = model(vec![session("/home/u/toolpath", "Parser fix", 3)]);
        let rows = paint(&m, 60, 30);
        let pane = rows.iter().position(|row| row == " Parser fix").unwrap();
        assert_eq!(rows[pane - 1], "─".repeat(60));
        assert_eq!(
            rows[pane..pane + 4],
            [" Parser fix", " 09:00 · 1h00", "", " Project  ~/toolpath",]
        );
        assert_eq!(rows[pane + 4], "");
        assert!(rows[4].starts_with(" ~/toolpath"), "lanes: {:?}", rows[4]);
    }

    #[test]
    fn a_terminal_of_20_rows_shows_the_pane_and_no_lanes() {
        let m = model(vec![session("/home/u/toolpath", "fix the parser", 1)]);
        let rows = paint(&m, 60, 20);
        assert!(rows[3].starts_with("─"), "{:?}", rows[3]);
        assert_eq!(rows[14], " fix the parser");
        assert_eq!(rows[17], " Project  ~/toolpath");
        assert_eq!(paint(&m, 60, 19)[13], "");
    }

    #[test]
    fn home_scrolls_the_list_back_to_the_first_heading() {
        let sessions = (1..=5)
            .map(|n| session(&format!("/home/u/p{n}"), &format!("s{n}"), n))
            .collect();
        let mut m = model(sessions);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let mut state = ListState::default();
        let mut first_list_row = |m: &crate::cmd_resume::tui::model::Model| {
            let screen = view(m);
            terminal
                .draw(|frame| draw(frame, &screen, &mut state))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..60)
                .map(|x| buffer[(x, 5)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert!(first_list_row(&m).starts_with(" ~/p1"));
        m.update(Key::End);
        assert!(!first_list_row(&m).starts_with(" ~/p1"));
        m.update(Key::Home);
        assert!(first_list_row(&m).starts_with(" ~/p1"));
    }

    #[test]
    fn sessions_at_once_take_separate_tracks() {
        let at = |h: u32| -> DateTime<Utc> { format!("2026-09-23T{h:02}:00:00Z").parse().unwrap() };
        let (track, count) = tracks(&[(at(1), at(3), 0), (at(2), at(4), 1), (at(5), at(6), 2)]);
        assert_eq!((track, count), (vec![0, 1, 0], 2));
    }
}
