//! Functional core of the resume UI: the state, the keys that change
//! it, the effects it asks the shell to perform, and `update`. No IO
//! and no terminal types; every transition is a plain function call
//! the tests drive directly.

use std::collections::HashSet;

use chrono::{DateTime, FixedOffset, Utc};

use super::landing::{self, Action, Page};
use crate::artifact::ArtifactType;

/// One session in the document cache: the facts a row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub harness: ArtifactType,
    /// The cached document a resume reads.
    pub cache_id: String,
    /// The directory the session ran in; empty when the document names
    /// none.
    pub dir: String,
    /// `dir` is a directory on this machine, so a resume can run there.
    pub dir_exists: bool,
    pub title: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

impl Session {
    fn matches(&self, filter: &str) -> bool {
        let filter = filter.to_lowercase();
        self.title.to_lowercase().contains(&filter) || self.dir.to_lowercase().contains(&filter)
    }
}

/// A key press, already mapped from the terminal's event type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    Char(char),
    Quit,
}

/// What the shell must do after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Resume(Selection),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub session: Session,
    /// The directory the resume runs in: the session's own, or `here`.
    pub dir: String,
}

/// The span of time the page shows; `t` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    Today,
    #[default]
    Yesterday,
    SinceMonday,
    Week,
}

impl Window {
    pub const ALL: [Window; 4] = [
        Window::Today,
        Window::Yesterday,
        Window::SinceMonday,
        Window::Week,
    ];

    pub fn next(self) -> Self {
        match self {
            Window::Today => Window::Yesterday,
            Window::Yesterday => Window::SinceMonday,
            Window::SinceMonday => Window::Week,
            Window::Week => Window::Today,
        }
    }

    /// The window's tab.
    pub fn tab(self) -> &'static str {
        match self {
            Window::Today => "today",
            Window::Yesterday => "yesterday",
            Window::SinceMonday => "this week",
            Window::Week => "7 days",
        }
    }

    /// The window as the end of "nothing ...".
    pub fn phrase(self) -> &'static str {
        match self {
            Window::Today => "today",
            Window::Yesterday => "since yesterday",
            Window::SinceMonday => "since Monday",
            Window::Week => "in the last 7 days",
        }
    }

    /// The window's start for `now` in `tz`. Every window runs to `now`.
    pub fn start(self, now: DateTime<Utc>, tz: FixedOffset) -> DateTime<Utc> {
        use chrono::Datelike;
        let today = now.with_timezone(&tz).date_naive();
        let midnight = |day: chrono::NaiveDate| {
            day.and_hms_opt(0, 0, 0)
                .expect("midnight exists")
                .and_local_timezone(tz)
                .single()
                .expect("a fixed offset has one midnight")
                .with_timezone(&Utc)
        };
        match self {
            Window::Today => midnight(today),
            Window::Yesterday => midnight(today - chrono::Duration::days(1)),
            Window::SinceMonday => {
                let back = today.weekday().num_days_from_monday() as i64;
                midnight(today - chrono::Duration::days(back))
            }
            Window::Week => now - chrono::Duration::days(7),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    sessions: Vec<Session>,
    /// The directory `path resume` runs from (`-C`, else the shell cwd).
    here: String,
    /// `-C` was given: `here` is the resume directory, not the session's.
    here_pinned: bool,
    /// The user's home directory; the page shows it as `~`.
    home: Option<String>,
    /// The moment the page is computed against (today, this week).
    now: DateTime<Utc>,
    /// The local UTC offset for the windows and the time axis.
    tz: FixedOffset,
    window: Window,
    /// Projects that show every row, not the newest few.
    expanded: HashSet<String>,
    /// The filter text while the filter is open. An open filter shows
    /// the matching sessions of every window.
    filter: Option<String>,
    /// Index into the page's items.
    cursor: usize,
}

impl Model {
    /// `sessions` in any order; `here` the directory `path resume` runs
    /// from; `now` the moment the page counts against.
    pub fn new(
        sessions: Vec<Session>,
        here: String,
        home: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            sessions,
            here,
            here_pinned: false,
            home,
            now,
            tz: FixedOffset::east_opt(0).expect("UTC"),
            window: Window::default(),
            expanded: HashSet::new(),
            filter: None,
            cursor: 0,
        }
    }

    pub fn with_here_pinned(mut self, pinned: bool) -> Self {
        self.here_pinned = pinned;
        self
    }

    /// The local UTC offset; the windows and the time axis use it.
    pub fn with_offset(mut self, tz: FixedOffset) -> Self {
        self.tz = tz;
        self
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }
    pub fn here(&self) -> &str {
        &self.here
    }
    pub fn home(&self) -> Option<&str> {
        self.home.as_deref()
    }
    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }
    pub fn tz(&self) -> FixedOffset {
        self.tz
    }
    pub fn window(&self) -> Window {
        self.window
    }
    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn is_expanded(&self, dir: &str) -> bool {
        self.expanded.contains(dir)
    }

    /// `session` was active in the window.
    pub fn in_window(&self, session: &Session) -> bool {
        session
            .last_activity
            .is_some_and(|last| last >= self.window.start(self.now, self.tz))
    }

    /// `session` is on the page: a match of the open filter, else a
    /// session in the window.
    pub fn shows(&self, session: &Session) -> bool {
        match &self.filter {
            Some(text) if !text.is_empty() => session.matches(text),
            _ => self.in_window(session),
        }
    }

    pub fn page(&self) -> Page {
        landing::page(self)
    }

    /// The resume of `session`: in its own directory when that exists
    /// and `-C` named none, else `here`.
    pub fn selection(&self, session: Session) -> Selection {
        let dir = if session.dir_exists && !self.here_pinned {
            session.dir.clone()
        } else {
            self.here.clone()
        };
        Selection { session, dir }
    }

    pub fn update(&mut self, key: Key) -> Option<Effect> {
        let items = self.page().items;
        let last = items.len().saturating_sub(1);
        match key {
            Key::Quit => return Some(Effect::Quit),
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(last),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = last,
            Key::Enter => match items.get(self.cursor) {
                Some(Action::Session(i)) => {
                    return Some(Effect::Resume(self.selection(self.sessions[*i].clone())));
                }
                Some(Action::More(dir)) => {
                    self.expanded.insert(dir.clone());
                }
                None => {}
            },
            Key::Esc | Key::Backspace | Key::Char(_) => return self.edit(key),
        }
        None
    }

    /// A key that types into the open filter, or a command key when the
    /// filter is closed.
    fn edit(&mut self, key: Key) -> Option<Effect> {
        match (&mut self.filter, key) {
            (Some(text), Key::Char(c)) => text.push(c),
            (Some(text), Key::Backspace) => {
                text.pop();
            }
            (Some(_), _) => self.filter = None,
            (None, Key::Char('/')) => self.filter = Some(String::new()),
            (None, Key::Char('t')) => self.window = self.window.next(),
            (None, Key::Char('q') | Key::Esc) => return Some(Effect::Quit),
            (None, _) => return None,
        }
        self.cursor = 0;
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn now() -> DateTime<Utc> {
        "2026-09-23T12:00:00Z".parse().unwrap()
    }

    /// A session in `dir` that ran for 1 hour and ended `hours_ago`.
    pub(crate) fn session(dir: &str, title: &str, hours_ago: i64) -> Session {
        let last = now() - chrono::Duration::hours(hours_ago);
        Session {
            harness: ArtifactType::Claude,
            cache_id: format!("claude-{title}"),
            dir: dir.to_string(),
            dir_exists: true,
            title: title.to_string(),
            started_at: Some(last - chrono::Duration::hours(1)),
            last_activity: Some(last),
        }
    }

    pub(crate) fn model(sessions: Vec<Session>) -> Model {
        Model::new(
            sessions,
            "/work/here".to_string(),
            Some("/home/u".to_string()),
            now(),
        )
    }

    fn keys(m: &mut Model, text: &str) {
        for c in text.chars() {
            assert_eq!(m.update(Key::Char(c)), None);
        }
    }

    #[test]
    fn windows_start_at_local_midnight() {
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let start = |w: Window| w.start(now(), tz).to_rfc3339();
        assert_eq!(start(Window::Today), "2026-09-22T22:00:00+00:00");
        assert_eq!(start(Window::Yesterday), "2026-09-21T22:00:00+00:00");
        assert_eq!(start(Window::SinceMonday), "2026-09-20T22:00:00+00:00");
        assert_eq!(start(Window::Week), "2026-09-16T12:00:00+00:00");
    }

    #[test]
    fn t_cycles_the_window_and_returns_the_cursor_to_the_top() {
        let mut m = model(vec![session("/p", "a", 1), session("/p", "b", 2)]);
        m.update(Key::Down);
        assert_eq!(m.cursor(), 1);
        keys(&mut m, "t");
        assert_eq!(m.window(), Window::SinceMonday);
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn the_cursor_stays_on_the_page() {
        let mut m = model(vec![session("/p", "a", 1), session("/p", "b", 2)]);
        m.update(Key::Up);
        assert_eq!(m.cursor(), 0);
        m.update(Key::End);
        assert_eq!(m.cursor(), 1);
        m.update(Key::Down);
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn enter_resumes_the_session_under_the_cursor_in_its_own_directory() {
        let mut m = model(vec![session("/p", "a", 1), session("/p", "b", 2)]);
        m.update(Key::Down);
        let Some(Effect::Resume(selection)) = m.update(Key::Enter) else {
            panic!("enter on a session resumes it");
        };
        assert_eq!(selection.session.title, "b");
        assert_eq!(selection.dir, "/p");
    }

    #[test]
    fn a_resume_runs_here_when_the_directory_is_gone_or_c_names_one() {
        let gone = Session {
            dir_exists: false,
            ..session("/gone", "a", 1)
        };
        assert_eq!(model(vec![]).selection(gone).dir, "/work/here");
        let pinned = model(vec![]).with_here_pinned(true);
        assert_eq!(pinned.selection(session("/p", "a", 1)).dir, "/work/here");
    }

    #[test]
    fn the_filter_shows_matches_from_every_window() {
        let mut m = model(vec![
            session("/p", "fix the parser", 1),
            session("/p", "old Parser work", 24 * 30),
            session("/q", "write the docs", 1),
        ]);
        let shown = |m: &Model| {
            m.sessions()
                .iter()
                .filter(|s| m.shows(s))
                .map(|s| s.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&m), ["fix the parser", "write the docs"]);
        keys(&mut m, "/parser");
        assert_eq!(m.filter(), Some("parser"));
        assert_eq!(shown(&m), ["fix the parser", "old Parser work"]);
        m.update(Key::Backspace);
        assert_eq!(m.filter(), Some("parse"));
        m.update(Key::Esc);
        assert_eq!(m.filter(), None);
        assert_eq!(shown(&m), ["fix the parser", "write the docs"]);
    }

    #[test]
    fn q_and_esc_quit_only_when_the_filter_is_closed() {
        let mut m = model(vec![session("/p", "a", 1)]);
        keys(&mut m, "/q");
        assert_eq!(m.filter(), Some("q"));
        assert_eq!(m.update(Key::Esc), None);
        assert_eq!(m.update(Key::Char('q')), Some(Effect::Quit));
        assert_eq!(model(vec![]).update(Key::Esc), Some(Effect::Quit));
        keys(&mut m, "/");
        assert_eq!(m.update(Key::Quit), Some(Effect::Quit));
    }
}
