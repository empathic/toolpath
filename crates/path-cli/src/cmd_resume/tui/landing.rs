//! The session page: the sessions by project, the same sessions as
//! lanes over the time window, and the detail of the session under
//! the cursor. Pure: takes the model, returns a [`Page`] of sections
//! and the actions its lines trigger.

use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Utc};

use super::model::{Model, Window};
use crate::cache::SessionSummary;

/// What a selectable line does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A session (index into the model's sessions).
    Session(usize),
    /// Show every row of a project (the project's directory).
    More(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub sections: Vec<Section>,
    /// Actions in page order; a [`Line::item`] indexes this.
    pub items: Vec<Action>,
    /// The time windows, the current one marked.
    pub windows: Vec<(String, bool)>,
    pub lanes: Lanes,
    /// The session under the cursor.
    pub detail: Option<Detail>,
    pub keys: String,
}

/// A session in the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub name: String,
    /// When the session was last active, how long it ran, how many
    /// turns it has, and its model.
    pub facts: String,
    pub project: String,
}

/// The sessions on the page as spans over the time window, one lane
/// per project in page order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lanes {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tz: FixedOffset,
    pub rows: Vec<Lane>,
    /// Projects on the page past the last lane.
    pub more: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub label: String,
    /// Each session's start, last activity, and index into the model's
    /// sessions.
    pub spans: Vec<(DateTime<Utc>, DateTime<Utc>, usize)>,
}

/// Lanes the graph shows at most.
pub const LANES_MAX: usize = 6;

/// Session rows a project shows before its "more" line.
const ROWS_PER_PROJECT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub lines: Vec<Line>,
}

/// How the draw shows a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Project,
    Session,
    /// "More" rows and other quiet lines.
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Leading spaces.
    pub indent: usize,
    pub text: String,
    /// A project heading's count of sessions on the page.
    pub count: Option<usize>,
    /// A session row's columns.
    pub cols: Option<Cols>,
    /// Index into [`Page::items`] when the line is selectable.
    pub item: Option<usize>,
    pub style: Style,
}

/// How long ago, for the time column's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    Hour,
    Today,
    Week,
    Older,
}

/// A session row's columns, drawn after the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cols {
    /// `15s ago`, `3h ago`, `8d ago`.
    pub ago: String,
    pub age: Age,
    pub duration: String,
}

/// One project directory's sessions on the page, newest first.
struct Project {
    dir: String,
    sessions: Vec<usize>,
}

/// The projects with a session on the page: `here` first, then by
/// newest activity.
fn projects(m: &Model) -> Vec<Project> {
    let sessions = m.sessions();
    let mut newest: Vec<usize> = (0..sessions.len())
        .filter(|&i| m.shows(&sessions[i]))
        .collect();
    newest.sort_by_key(|&i| std::cmp::Reverse(sessions[i].last_activity));

    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut projects: Vec<Project> = Vec::new();
    for i in newest {
        let dir = sessions[i].dir.as_str();
        let at = *index.entry(dir).or_insert_with(|| {
            projects.push(Project {
                dir: dir.to_string(),
                sessions: Vec::new(),
            });
            projects.len() - 1
        });
        projects[at].sessions.push(i);
    }
    // Stable: projects stay in order of their newest session.
    projects.sort_by_key(|p| p.dir != m.here());
    projects
}

/// Days between `t` and today, local time.
fn age_days(m: &Model, t: DateTime<Utc>) -> i64 {
    let day = |t: DateTime<Utc>| t.with_timezone(&m.tz()).date_naive();
    (day(m.now()) - day(t)).num_days()
}

/// `Www HH:MM` within the week, `MM-DD` within the year, else
/// `YYYY-MM-DD`; `-` when unknown. Local time.
fn when(m: &Model, t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else {
        return "-".to_string();
    };
    let local = t.with_timezone(&m.tz());
    match age_days(m, t) {
        i64::MIN..=6 => local.format("%a %H:%M").to_string(),
        7..=364 => local.format("%m-%d").to_string(),
        _ => local.format("%Y-%m-%d").to_string(),
    }
}

fn detail(m: &Model, s: &SessionSummary) -> Detail {
    let mut facts = vec![
        when(m, s.last_activity),
        duration(s),
        plural(s.turn_count, "turn", "turns"),
    ];
    facts.extend(s.model.clone());
    Detail {
        name: one_line(&s.title),
        facts: facts.join(" · "),
        project: tilde(&s.dir, m.home()),
    }
}

fn session_line(m: &Model, i: usize, item: usize) -> Line {
    let s = &m.sessions()[i];
    let age = match s.last_activity {
        Some(t) if m.now() - t < chrono::Duration::hours(1) => Age::Hour,
        Some(t) if age_days(m, t) <= 0 => Age::Today,
        Some(t) if age_days(m, t) < 7 => Age::Week,
        _ => Age::Older,
    };
    Line {
        indent: 2,
        text: one_line(&s.title),
        count: None,
        cols: Some(Cols {
            ago: ago(s.last_activity, m.now()),
            age,
            duration: duration(s),
        }),
        item: Some(item),
        style: Style::Session,
    }
}

fn dim_line(indent: usize, text: String, item: Option<usize>) -> Line {
    Line {
        indent,
        text,
        count: None,
        cols: None,
        item,
        style: Style::Dim,
    }
}

/// `title` on one line, runs of whitespace as one space; `(no title)`
/// when it has no text.
fn one_line(title: &str) -> String {
    let line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        "(no title)".to_string()
    } else {
        line
    }
}

/// `dir` with the home directory as `~`.
fn tilde(dir: &str, home: Option<&str>) -> String {
    if dir.is_empty() {
        return "(no directory recorded)".to_string();
    }
    let under_home = home
        .filter(|home| !home.is_empty())
        .and_then(|home| dir.strip_prefix(home))
        .filter(|rest| rest.is_empty() || rest.starts_with('/'));
    match under_home {
        Some(rest) => format!("~{rest}"),
        None => dir.to_string(),
    }
}

/// `15s ago`, `12m ago`, `3h ago`, `8d ago`; `-` when unknown.
fn ago(t: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(t) = t else {
        return "-".to_string();
    };
    let secs = (now - t).num_seconds().max(0);
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `45s`, `12m`, `2h10`, `3d`; `-` when either end is unknown.
fn duration(s: &SessionSummary) -> String {
    let (Some(start), Some(end)) = (s.started_at, s.last_activity) else {
        return "-".to_string();
    };
    let secs = (end - start).num_seconds().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h{:02}", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d", secs / 86_400),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The session page: a section per project with a session on the page
/// ([`Model::shows`]), `here` first, then newest first. A section is
/// the project heading, its newest sessions, and a line for the rest.
/// An expanded project and an open filter show every session.
pub fn page(m: &Model) -> Page {
    let sessions = m.sessions();
    let filtering = m.filter().is_some_and(|text| !text.is_empty());
    let projects = projects(m);

    let mut items = Vec::new();
    let mut sections = Vec::new();
    for p in &projects {
        let mut lines = vec![Line {
            indent: 0,
            text: tilde(&p.dir, m.home()),
            count: Some(p.sessions.len()),
            cols: None,
            item: None,
            style: Style::Project,
        }];
        let rows = if filtering || m.is_expanded(&p.dir) {
            p.sessions.len()
        } else {
            ROWS_PER_PROJECT
        };
        for &i in p.sessions.iter().take(rows) {
            items.push(Action::Session(i));
            lines.push(session_line(m, i, items.len() - 1));
        }
        let more = p.sessions.len().saturating_sub(rows);
        if more > 0 {
            items.push(Action::More(p.dir.clone()));
            lines.push(dim_line(2, format!("… {more} more"), Some(items.len() - 1)));
        }
        sections.push(Section { lines });
    }

    let window = m.window();
    if sections.is_empty() {
        let text = if filtering {
            "no session matches the filter".to_string()
        } else {
            format!("nothing {} · t widens the window", window.phrase())
        };
        sections.push(Section {
            lines: vec![dim_line(0, text, None)],
        });
    } else if !filtering {
        let mut all: Vec<&str> = sessions.iter().map(|s| s.dir.as_str()).collect();
        all.sort_unstable();
        all.dedup();
        let quiet = all.len() - projects.len();
        if quiet > 0 {
            let text = format!(
                "… {} with nothing {}",
                plural(quiet, "project", "projects"),
                window.phrase()
            );
            sections.push(Section {
                lines: vec![dim_line(0, text, None)],
            });
        }
    }

    let start = window.start(m.now(), m.tz());
    let lanes = Lanes {
        start,
        end: m.now(),
        tz: m.tz(),
        rows: projects
            .iter()
            .take(LANES_MAX)
            .map(|p| Lane {
                label: tilde(&p.dir, m.home()),
                spans: p
                    .sessions
                    .iter()
                    .filter_map(|&i| {
                        let last = sessions[i].last_activity.filter(|last| *last >= start)?;
                        Some((sessions[i].started_at.unwrap_or(last), last, i))
                    })
                    .collect(),
            })
            .collect(),
        more: projects.len().saturating_sub(LANES_MAX),
    };

    let detail = match items.get(m.cursor()) {
        Some(Action::Session(i)) => Some(detail(m, &sessions[*i])),
        _ => None,
    };

    Page {
        sections,
        detail,
        items,
        windows: Window::ALL
            .iter()
            .map(|w| (w.tab().to_string(), *w == window))
            .collect(),
        lanes,
        keys: if m.filter().is_some() {
            "type to filter   enter resume   esc close the filter".to_string()
        } else {
            "enter resume   t time   / filter   q quit".to_string()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_resume::tui::model::Key;
    use crate::cmd_resume::tui::model::tests::{model, now, session};

    /// Each section's lines as text; a session row as `> <name>`.
    fn texts(page: &Page) -> Vec<Vec<String>> {
        page.sections
            .iter()
            .map(|section| {
                section
                    .lines
                    .iter()
                    .map(|line| match (line.style, line.count) {
                        (Style::Session, _) => format!("> {}", line.text),
                        (_, Some(n)) => format!("{} {n}", line.text),
                        _ => line.text.clone(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_page_lists_here_first_then_the_newest_project() {
        let m = model(vec![
            session("/home/u/old", "a", 5),
            session("/work/here", "b", 9),
            session("/home/u/new", "c", 1),
            session("/home/u/new", "d", 2),
        ]);
        assert_eq!(
            texts(&m.page()),
            [
                vec!["/work/here 1", "> b"],
                vec!["~/new 2", "> c", "> d"],
                vec!["~/old 1", "> a"],
            ]
        );
    }

    #[test]
    fn the_detail_is_the_session_under_the_cursor() {
        let first = SessionSummary {
            turn_count: 12,
            model: Some("claude-opus-5".to_string()),
            ..session("/home/u/p", "Parser fix", 3)
        };
        let second = SessionSummary {
            turn_count: 1,
            ..session("/home/u/p", "Greeting", 30)
        };
        let mut m = model(vec![first, second]);
        assert_eq!(
            m.page().detail,
            Some(Detail {
                name: "Parser fix".to_string(),
                facts: "Wed 09:00 · 1h00 · 12 turns · claude-opus-5".to_string(),
                project: "~/p".to_string(),
            })
        );

        m.update(Key::Down);
        let detail = m.page().detail.unwrap();
        assert_eq!(detail.facts, "Tue 06:00 · 1h00 · 1 turn");
    }

    #[test]
    fn a_line_that_is_no_session_has_no_detail() {
        let rows: Vec<SessionSummary> = (1..=6)
            .map(|n| session("/p", &format!("s{n}"), n))
            .collect();
        let mut m = model(rows);
        m.update(Key::End);
        assert_eq!(m.page().detail, None);
        assert_eq!(model(vec![]).page().detail, None);
    }

    #[test]
    fn tilde_stands_for_the_home_directory_and_no_other() {
        let home = Some("/home/u");
        assert_eq!(tilde("/home/u", home), "~");
        assert_eq!(tilde("/home/u/p", home), "~/p");
        assert_eq!(tilde("/home/user2/p", home), "/home/user2/p");
        assert_eq!(tilde("/p", None), "/p");
        assert_eq!(tilde("", home), "(no directory recorded)");
    }

    #[test]
    fn a_session_outside_the_window_leaves_the_page() {
        let mut m = model(vec![
            session("/p", "recent", 1),
            session("/q", "last week", 24 * 5),
        ]);
        assert_eq!(
            texts(&m.page()),
            [
                vec!["/p 1", "> recent"],
                vec!["… 1 project with nothing since yesterday"]
            ]
        );
        m.update(Key::Char('t'));
        m.update(Key::Char('t'));
        assert_eq!(
            texts(&m.page()),
            [vec!["/p 1", "> recent"], vec!["/q 1", "> last week"]]
        );
    }

    #[test]
    fn an_empty_window_says_how_to_widen_it() {
        let m = model(vec![session("/p", "old", 24 * 30)]);
        assert_eq!(
            texts(&m.page()),
            [vec!["nothing since yesterday · t widens the window"]]
        );
        assert!(m.page().items.is_empty());
    }

    #[test]
    fn a_project_shows_its_newest_rows_until_enter_on_the_more_line() {
        let rows: Vec<SessionSummary> = (1..=7)
            .map(|n| session("/p", &format!("s{n}"), n))
            .collect();
        let mut m = model(rows);
        let page = m.page();
        assert_eq!(
            texts(&page),
            [vec![
                "/p 7",
                "> s1",
                "> s2",
                "> s3",
                "> s4",
                "> s5",
                "… 2 more"
            ]]
        );
        assert_eq!(page.items[5], Action::More("/p".to_string()));

        m.update(Key::End);
        assert_eq!(m.update(Key::Enter), None);
        assert_eq!(m.page().items.len(), 7);
    }

    #[test]
    fn the_filter_lifts_the_row_limit() {
        let rows: Vec<SessionSummary> = (1..=7)
            .map(|n| session("/p", &format!("s{n}"), n))
            .collect();
        let mut m = model(rows);
        for c in "/s".chars() {
            m.update(Key::Char(c));
        }
        assert_eq!(m.page().items.len(), 7);
        m.update(Key::Char('x'));
        assert_eq!(texts(&m.page()), [vec!["no session matches the filter"]]);
    }

    #[test]
    fn a_title_shows_on_one_line() {
        let m = model(vec![
            session("/p", "fix\n  the   parser", 1),
            session("/p", "  ", 2),
        ]);
        assert_eq!(
            texts(&m.page()),
            [vec!["/p 2", "> fix the parser", "> (no title)"]]
        );
    }

    #[test]
    fn the_columns_show_the_age_and_the_duration() {
        let m = model(vec![session("/p", "a", 3)]);
        let page = m.page();
        let cols = page.sections[0].lines[1].cols.clone().unwrap();
        assert_eq!(
            cols,
            Cols {
                ago: "3h ago".to_string(),
                age: Age::Today,
                duration: "1h00".to_string(),
            }
        );
    }

    #[test]
    fn the_lanes_hold_the_sessions_of_the_window_by_project() {
        let m = model(vec![
            session("/home/u/a", "one", 1),
            session("/home/u/a", "two", 3),
            session("/home/u/b", "three", 2),
        ]);
        let lanes = m.page().lanes;
        assert_eq!(lanes.end, now());
        assert_eq!(lanes.start, Window::Yesterday.start(now(), m.tz()));
        let rows: Vec<(String, Vec<usize>)> = lanes
            .rows
            .iter()
            .map(|lane| {
                (
                    lane.label.clone(),
                    lane.spans.iter().map(|(_, _, i)| *i).collect(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("~/a".to_string(), vec![0, 1]),
                ("~/b".to_string(), vec![2])
            ]
        );
        assert_eq!(lanes.more, 0);
    }
}
