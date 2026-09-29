//! `path resume` with no input: a terminal UI over the agent sessions
//! in the document cache. It syncs the cache, shows the sessions by
//! project over a time window, and hands the chosen one to the same
//! projection and exec as `path resume <input>`.
//!
//! The core is pure: [`model`] holds the state and the transitions,
//! [`landing`] lays the page out, and [`view`] turns the state into a
//! [`view::Screen`]. This module, [`rows`], and `draw` are the
//! imperative shell: the cache, the terminal, the key mapping, and the
//! resume itself.
//!
//! The UI sits behind [`Chooser`]: it takes the model and returns the
//! user's [`Selection`]. [`TerminalChooser`] runs the event loop. In
//! a test build, `FixedChooser` answers without a terminal, so a test
//! drives the whole pipeline (sync, rows, projection, exec) headless.

mod draw;
mod landing;
mod model;
mod rows;
mod view;

use std::io::IsTerminal;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{ExecStrategy, ResumeArgs};
use crate::artifact::ArtifactType;
use model::{Effect, Key, Model, Selection};

/// How the session gets chosen.
pub(crate) trait Chooser {
    /// `None` means the user quit.
    fn choose(&self, model: Model) -> Result<Option<Selection>>;
}

/// The ratatui screen.
struct TerminalChooser;

impl Chooser for TerminalChooser {
    fn choose(&self, mut model: Model) -> Result<Option<Selection>> {
        let mut terminal = ratatui::init();
        let outcome = event_loop(&mut terminal, &mut model);
        ratatui::restore();
        outcome
    }
}

/// A fixed answer: the session with this title.
#[cfg(test)]
pub(crate) struct FixedChooser {
    pub(crate) title: String,
}

#[cfg(test)]
impl Chooser for FixedChooser {
    fn choose(&self, model: Model) -> Result<Option<Selection>> {
        let session = model
            .sessions()
            .iter()
            .find(|session| session.title == self.title)
            .cloned()
            .with_context(|| format!("no session with the title {:?}", self.title))?;
        Ok(Some(model.selection(session)))
    }
}

pub(crate) fn run(args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "`path resume` with no input opens a terminal UI and needs a TTY on stdin and \
             stdout; pass an input (a Pathbase URL, a file, or a cache id)"
        );
    }
    run_with_chooser(args, exec, &TerminalChooser)
}

/// The pipeline around the chooser: sync, rows, choose, resume.
pub(crate) fn run_with_chooser(
    args: ResumeArgs,
    exec: &dyn ExecStrategy,
    chooser: &dyn Chooser,
) -> Result<()> {
    let here = match args.cwd.as_ref() {
        Some(p) => {
            std::fs::canonicalize(p).with_context(|| format!("resolve cwd path {}", p.display()))?
        }
        None => std::env::current_dir()?,
    };
    let config = crate::config::Config::load()?;
    let config_dir = config.config_dir()?;

    let harnesses: Vec<ArtifactType> = ArtifactType::ALL
        .into_iter()
        .filter(|artifact_type| artifact_type.harness().is_some())
        .collect();
    let bundle = crate::providers::harness_bundle(&config);
    if let Err(e) = crate::sync::sync_bundle(&config_dir, &bundle, &harnesses, None, &mut ()) {
        eprintln!("warning: cache sync skipped: {e:#}");
    }
    let sessions = rows::load_sessions(&config_dir)?;
    if sessions.is_empty() {
        anyhow::bail!("the document cache holds no agent session to resume");
    }

    let model = Model::new(
        sessions,
        here.to_string_lossy().into_owned(),
        config
            .home_dir()
            .map(|home| home.to_string_lossy().into_owned()),
        chrono::Utc::now(),
    )
    .with_here_pinned(args.cwd.is_some())
    .with_offset(*chrono::Local::now().offset());
    match chooser.choose(model)? {
        None => std::process::exit(130),
        Some(selection) => resume(selection, args, exec),
    }
}

/// Resumes the chosen session through `path resume <cache id>`, in its
/// source harness when that is installed.
fn resume(selection: Selection, args: ResumeArgs, exec: &dyn ExecStrategy) -> Result<()> {
    let Selection { session, dir } = selection;
    let source = session
        .harness
        .harness()
        .filter(|harness| super::harness_available(*harness, None));
    super::run_with_strategy(
        ResumeArgs {
            input: Some(session.cache_id),
            cwd: Some(dir.into()),
            harness: args.harness.or(source),
            ..args
        },
        exec,
    )
}

/// Runs the UI until the user picks a session (`Some`) or quits
/// (`None`).
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    model: &mut Model,
) -> Result<Option<Selection>> {
    let mut list = ratatui::widgets::ListState::default();
    loop {
        let screen = view::view(model);
        terminal.draw(|frame| draw::draw(frame, &screen, &mut list))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match map_key(key).and_then(|key| model.update(key)) {
            Some(Effect::Quit) => return Ok(None),
            Some(Effect::Resume(selection)) => return Ok(Some(selection)),
            None => {}
        }
    }
}

fn map_key(key: KeyEvent) -> Option<Key> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Char('c') if ctrl => Key::Quit,
        KeyCode::Char('p') if ctrl => Key::Up,
        KeyCode::Char('n') if ctrl => Key::Down,
        KeyCode::Char(c) if !ctrl => Key::Char(c),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_map_to_the_core_vocabulary() {
        let plain = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(map_key(press(KeyCode::Up, plain)), Some(Key::Up));
        assert_eq!(map_key(press(KeyCode::Char('n'), ctrl)), Some(Key::Down));
        assert_eq!(map_key(press(KeyCode::Char('c'), ctrl)), Some(Key::Quit));
        assert_eq!(
            map_key(press(KeyCode::Char('t'), plain)),
            Some(Key::Char('t'))
        );
        assert_eq!(map_key(press(KeyCode::Char('x'), ctrl)), None);
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..press(KeyCode::Enter, plain)
        };
        assert_eq!(map_key(release), None);
    }
}
