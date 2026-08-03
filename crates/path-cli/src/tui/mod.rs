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

mod matcher;
mod preview;
mod render;
mod state;

use std::io::Stderr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::fuzzy::{PickOptions, PickResult};

use matcher::Row;
use preview::{PreviewContent, PreviewScheduler};
use render::{LayoutMode, PreviewBody, PreviewView};
use state::AppState;

/// How long each event-loop tick waits for input before servicing the
/// preview scheduler and redrawing.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// True while the terminal is in picker mode (raw + maybe alt screen)
/// and needs restoring. Consulted by both the panic hook and the
/// normal drop path so restore runs exactly once whichever fires.
static NEEDS_RESTORE: AtomicBool = AtomicBool::new(false);
/// Whether the emergency restore must also leave the alternate screen.
static RESTORE_FULLSCREEN: AtomicBool = AtomicBool::new(false);

/// Restore-first path for the panic hook: idempotent, best-effort,
/// touches only global terminal state (the `Terminal` value itself is
/// unreachable from a panic hook).
fn emergency_restore() {
    if !NEEDS_RESTORE.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = disable_raw_mode();
    if RESTORE_FULLSCREEN.load(Ordering::SeqCst) {
        let _ = crossterm::execute!(std::io::stderr(), LeaveAlternateScreen);
    }
}

/// Scoped panic hook: `take_hook` -> install a restore-first wrapper,
/// then put the previous hook back on clean drop.
struct PanicHookGuard {
    prev: Option<Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>>,
}

impl PanicHookGuard {
    fn install() -> Self {
        let prev: Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync> =
            Arc::from(std::panic::take_hook());
        let in_hook = prev.clone();
        std::panic::set_hook(Box::new(move |info| {
            emergency_restore();
            in_hook(info);
        }));
        Self { prev: Some(prev) }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::panic::set_hook(Box::new(move |info| prev(info)));
        }
    }
}

/// The live terminal for one layout mode. Renders on **stderr** so
/// stdout stays clean for piped results. Restore is idempotent (the
/// [`NEEDS_RESTORE`] flag arbitrates with the panic hook) and clears
/// the inline viewport region so the shell prompt continues cleanly.
struct TermGuard {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    fullscreen: bool,
}

impl TermGuard {
    fn new(mode: LayoutMode) -> Result<Self> {
        let fullscreen = mode.is_fullscreen();
        enable_raw_mode().context("enable raw mode")?;
        NEEDS_RESTORE.store(true, Ordering::SeqCst);
        RESTORE_FULLSCREEN.store(fullscreen, Ordering::SeqCst);
        let backend = CrosstermBackend::new(std::io::stderr());
        let terminal = match mode {
            LayoutMode::Inline { height } => Terminal::with_options(
                backend,
                TerminalOptions {
                    viewport: Viewport::Inline(height),
                },
            )
            .context("create inline terminal")?,
            _ => {
                crossterm::execute!(std::io::stderr(), EnterAlternateScreen)
                    .context("enter alternate screen")?;
                Terminal::new(backend).context("create fullscreen terminal")?
            }
        };
        Ok(Self {
            terminal,
            fullscreen,
        })
    }

    fn restore(&mut self) {
        if !NEEDS_RESTORE.swap(false, Ordering::SeqCst) {
            return;
        }
        if !self.fullscreen {
            // Clear the inline viewport region so the shell prompt
            // continues where the picker sat, without stale rows.
            let _ = self.terminal.clear();
        }
        let _ = disable_raw_mode();
        if self.fullscreen {
            let _ = crossterm::execute!(std::io::stderr(), LeaveAlternateScreen);
        }
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Run the native picker over `lines` with the fzf-shaped `opts`.
/// Same contract as the external backend: `Selected` carries full
/// original lines (hidden columns included), Esc/Ctrl-C yield
/// `Cancelled`, an accepted empty match set yields `NoMatch`.
pub(crate) fn pick(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    let spec = matcher::parse_field_spec(opts.with_nth)?;
    let rows: Vec<Row> = lines.iter().map(|l| Row::new(l, &spec)).collect();
    let preview_window = opts
        .preview
        .map(|_| preview::parse_preview_window(opts.preview_window));
    let mut state = AppState::new(rows, opts.multi, opts.prompt, opts.header, preview_window);
    if let Ok((w, h)) = crossterm::terminal::size() {
        state.term_w = w;
        state.term_h = h;
    }
    let template = opts.preview.map(crate::fuzzy::substitute_exe_placeholder);

    let _hook = PanicHookGuard::install();
    let mut mode = render::choose_layout(&state);
    let mut guard = TermGuard::new(mode)?;

    let (tx, rx) = mpsc::channel::<preview::PreviewMsg>();
    let mut scheduler = PreviewScheduler::new();
    let kill_slot = preview::new_kill_slot();
    let mut last_row: Option<usize> = None;
    // Row whose Ready text the pane last showed — kept on cache misses
    // so the pane doesn't blank while the next preview derives.
    let mut shown_row: Option<usize> = None;

    if template.is_some()
        && let Some(row) = state.current_row()
    {
        scheduler.on_selection_change(row, Instant::now());
        last_row = Some(row);
    }

    let result = loop {
        // Pane geometry for this tick: page size + preview inner size.
        let size = guard.terminal.size().context("query terminal size")?;
        let frame_area = match mode {
            LayoutMode::Inline { height } => Rect::new(0, 0, size.width, height.min(size.height)),
            _ => Rect::new(0, 0, size.width, size.height),
        };
        let areas = render::compute_areas(&state, mode, frame_area);
        state.page_rows = (areas.list.height as usize).max(1);

        let view = build_preview_view(&state, &scheduler, &mut shown_row);
        guard
            .terminal
            .draw(|f| render::draw(f, &state, mode, view.as_ref()))
            .context("draw picker frame")?;

        if event::poll(POLL_INTERVAL).context("poll terminal events")? {
            match event::read().context("read terminal event")? {
                // Key-release events double-fire on Windows terminals;
                // only act on press/repeat.
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(result) = state::handle_event(&mut state, InputEvent::from(key)) {
                        break result;
                    }
                }
                Event::Resize(w, h) => {
                    state::handle_event(&mut state, InputEvent::Resize(w, h));
                }
                _ => {}
            }
        }

        // Layout may have changed (resize, Ctrl-O). Crossing the
        // inline/fullscreen boundary — or changing the inline height —
        // needs a fresh terminal; staying fullscreen just re-splits on
        // the next draw.
        let new_mode = render::choose_layout(&state);
        if new_mode != mode {
            let recreate =
                mode.is_fullscreen() != new_mode.is_fullscreen() || !new_mode.is_fullscreen();
            mode = new_mode;
            if recreate {
                guard.restore();
                guard = TermGuard::new(mode)?;
            } else {
                guard.terminal.autoresize().context("autoresize terminal")?;
            }
        } else if matches!(mode, LayoutMode::Inline { .. }) {
            guard.terminal.autoresize().context("autoresize terminal")?;
        }

        if let Some(template) = template.as_deref() {
            let now = Instant::now();
            let current = state.current_row();
            if current != last_row {
                if let Some(row) = current {
                    scheduler.on_selection_change(row, now);
                }
                last_row = current;
            }
            while let Ok(msg) = rx.try_recv() {
                scheduler.on_msg(msg, current);
            }
            if let Some(req) = scheduler.poll(now) {
                let pane = areas
                    .preview
                    .map(|r| (r.width.saturating_sub(2), r.height.saturating_sub(2)))
                    .unwrap_or((size.width, size.height));
                let row = &state.rows[req.row];
                let command =
                    preview::substitute_placeholders(template, &row.fields, &row.original);
                preview::spawn_preview_job(req, command, pane, tx.clone(), kill_slot.clone());
            }
        }
    };

    guard.restore();
    // Don't leave a preview command running after the picker exits.
    preview::kill_current(&kill_slot);
    Ok(result)
}

/// Decide what the preview pane shows this frame. Pure derivation
/// from scheduler cache + selection: cache hits render instantly, a
/// miss keeps the previously shown text under a "(loading…)" title,
/// and a miss with nothing to keep shows the dim placeholder.
fn build_preview_view<'a>(
    state: &AppState,
    scheduler: &'a PreviewScheduler,
    shown_row: &mut Option<usize>,
) -> Option<PreviewView<'a>> {
    if !state.has_preview || !state.preview_visible {
        return None;
    }
    let Some(row) = state.current_row() else {
        return Some(PreviewView {
            title: "preview",
            body: PreviewBody::Placeholder,
        });
    };
    match scheduler.cached(row) {
        Some(PreviewContent::Ready(text)) => {
            *shown_row = Some(row);
            Some(PreviewView {
                title: "preview",
                body: PreviewBody::Text(text),
            })
        }
        Some(PreviewContent::Failed(line)) => Some(PreviewView {
            title: "preview",
            body: PreviewBody::Error(line),
        }),
        None => match shown_row.and_then(|r| scheduler.cached(r)) {
            Some(PreviewContent::Ready(text)) => Some(PreviewView {
                title: "preview (loading…)",
                body: PreviewBody::Text(text),
            }),
            _ => Some(PreviewView {
                title: "preview (loading…)",
                body: PreviewBody::Placeholder,
            }),
        },
    }
}

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
