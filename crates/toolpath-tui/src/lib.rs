//! Interactive TUI for viewing and redacting Toolpath Path documents.
//!
//! Provides a ratatui-based terminal UI for browsing toolpath traces with:
//! - Navigable step list with DAG graph visualization
//! - Expandable/collapsible step detail view
//! - Visual line mode for excluding steps
//! - Visual character mode for redacting text within fields
//! - Export to stdout with redacted steps collapsed into placeholders
//!
//! Uses stderr for TUI rendering so stdout stays clean for piped JSON output.

pub mod app;
pub mod model;
pub mod redact;
pub mod render;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use toolpath::v1;

/// TUI mode.
pub enum TuiMode {
    /// Read-only viewer.
    View,
    /// Interactive redaction.
    Redact,
}

/// Configuration for the TUI title bar.
pub struct TuiConfig {
    /// Application name shown in the title (e.g. "clash trace", "path").
    pub app_name: String,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            app_name: "toolpath".to_string(),
        }
    }
}

/// Launch the trace TUI.
///
/// Returns `Ok(Some(json))` if the user exported a redacted trace, `Ok(None)` if they quit.
pub fn run(path: v1::Path, mode: TuiMode, config: TuiConfig) -> Result<Option<String>> {
    let redact_mode = matches!(mode, TuiMode::Redact);
    let mut tui_app = app::TraceTuiApp::new(path, redact_mode, config);

    // Setup terminal on stderr so stdout stays clean for piped output.
    enable_raw_mode()?;
    let mut stderr = std::io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    // Run the app.
    let result = tui_app.run(&mut terminal);

    // Restore terminal.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
