//! Native interactive picker — the embedded backend behind
//! [`crate::fuzzy::pick`] (Atuin-inspired, built on ratatui).
//!
//! Module layout:
//!
//! - [`matcher`] — nucleo wrapper + fzf `--with-nth` field projection.
//! - [`preview`] — debounced async preview pipeline (no tokio).
//! - [`state`] — pure, event-vector-testable picker state machine.
//! - [`render`] — layout ladder + pane rendering.
//! - this file — terminal lifecycle and the event loop.
//!
//! The picker renders on **stderr** so stdout stays clean for piped
//! results, honors the full [`crate::fuzzy::PickOptions`] contract, and
//! returns [`crate::fuzzy::PickResult`] exactly like the external fzf
//! backend.

// Removed when fuzzy.rs switches its embedded backend to this module
// (the module is dark until then, and `-D warnings` would reject it).
#![allow(dead_code)]

mod matcher;
mod preview;
mod render;
mod state;

/// Picker input, decoupled from crossterm so [`state::handle_event`]
/// stays pure and event-vector testable. Chorded editing keys with
/// obvious single equivalents are normalized in the `From` impl
/// (Ctrl-A/Ctrl-E -> Home/End, Ctrl-P/Ctrl-N -> Up/Down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputEvent {
    /// A printable character typed into the query.
    Char(char),
    Backspace,
    /// Forward delete (Delete key, or Ctrl-D with a non-empty query).
    DeleteForward,
    Enter,
    Esc,
    CtrlC,
    /// Ctrl-D: cancel on an empty query (fzf parity), else forward
    /// delete — the split happens in the state machine, which knows
    /// the query.
    CtrlD,
    /// Clear the whole query.
    CtrlU,
    /// Delete the word before the cursor.
    CtrlW,
    /// Toggle the preview pane (fullscreen layouts only).
    CtrlO,
    /// Reserved: cycle the dormant [`state::FilterHook`]. No behavior
    /// yet — a future PR populates it for the bare-resume session
    /// picker.
    CtrlR,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    PageUp,
    PageDown,
    /// Toggle mark + advance (multi mode only).
    Tab,
    /// Toggle mark + retreat (multi mode only).
    BackTab,
    /// Scroll the preview pane up.
    ShiftUp,
    /// Scroll the preview pane down.
    ShiftDown,
    /// Terminal resized to (columns, rows).
    Resize(u16, u16),
    /// Anything we don't handle.
    Noop,
}

impl From<crossterm::event::KeyEvent> for InputEvent {
    fn from(key: crossterm::event::KeyEvent) -> Self {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('c') if ctrl => InputEvent::CtrlC,
            KeyCode::Char('d') if ctrl => InputEvent::CtrlD,
            KeyCode::Char('u') if ctrl => InputEvent::CtrlU,
            KeyCode::Char('w') if ctrl => InputEvent::CtrlW,
            KeyCode::Char('o') if ctrl => InputEvent::CtrlO,
            KeyCode::Char('r') if ctrl => InputEvent::CtrlR,
            KeyCode::Char('a') if ctrl => InputEvent::Home,
            KeyCode::Char('e') if ctrl => InputEvent::End,
            KeyCode::Char('p') if ctrl => InputEvent::Up,
            KeyCode::Char('n') if ctrl => InputEvent::Down,
            KeyCode::Char(c) if !ctrl => InputEvent::Char(c),
            KeyCode::Backspace => InputEvent::Backspace,
            KeyCode::Delete => InputEvent::DeleteForward,
            KeyCode::Enter => InputEvent::Enter,
            KeyCode::Esc => InputEvent::Esc,
            KeyCode::Left => InputEvent::Left,
            KeyCode::Right => InputEvent::Right,
            KeyCode::Home => InputEvent::Home,
            KeyCode::End => InputEvent::End,
            KeyCode::Up if shift => InputEvent::ShiftUp,
            KeyCode::Down if shift => InputEvent::ShiftDown,
            KeyCode::Up => InputEvent::Up,
            KeyCode::Down => InputEvent::Down,
            KeyCode::PageUp => InputEvent::PageUp,
            KeyCode::PageDown => InputEvent::PageDown,
            KeyCode::Tab => InputEvent::Tab,
            KeyCode::BackTab => InputEvent::BackTab,
            _ => InputEvent::Noop,
        }
    }
}
