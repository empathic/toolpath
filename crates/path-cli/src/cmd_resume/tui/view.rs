//! The view model: what the screen shows, computed from the [`Model`]
//! with no terminal types. `draw.rs` paints a [`Screen`] with ratatui;
//! another front end can paint the same value.

use super::landing::Page;
use super::model::Model;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub page: Page,
    /// Index into the page's items.
    pub cursor: usize,
    /// The filter text while the filter is open.
    pub filter: Option<String>,
}

pub fn view(m: &Model) -> Screen {
    Screen {
        page: m.page(),
        cursor: m.cursor(),
        filter: m.filter().map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_resume::tui::model::Key;
    use crate::cmd_resume::tui::model::tests::{model, session};

    #[test]
    fn the_screen_follows_the_cursor_and_the_filter() {
        let mut m = model(vec![session("/p", "a", 1), session("/p", "b", 2)]);
        m.update(Key::Down);
        let screen = view(&m);
        assert_eq!((screen.cursor, screen.filter.as_deref()), (1, None));
        assert_eq!(screen.page, m.page());

        m.update(Key::Char('/'));
        m.update(Key::Char('b'));
        let screen = view(&m);
        assert_eq!((screen.cursor, screen.filter.as_deref()), (0, Some("b")));
    }
}
