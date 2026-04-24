//! System tray / menu-bar mode.
//!
//! Sets up a macOS menu-bar icon with a small popover window and a 30-second
//! background poller that walks every agent-conversation provider
//! (`toolpath-claude`, `-gemini`, `-codex`, `-opencode`, `-pi`) and reports
//! how many sessions have been active recently.
//!
//! The popover is a second Tauri window (`label = "popover"`) configured as
//! undecorated and hidden by default in `tauri.conf.json`. Left-clicking the
//! tray icon toggles it; clicking the menu's "Open Toolpath" brings up the
//! main window.
//!
//! The poller emits a `tray:stats` event with a [`TrayStats`] payload. The
//! popover frontend subscribes to that event and also invokes
//! `tray_stats_now` for an immediate value on open.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_positioner::{Position, WindowExt};

/// How often the background poller re-scans providers.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Sessions touched within this window count as "active now".
const ACTIVE_WINDOW_SECS: i64 = 120;

/// Sessions touched within this window count as "recent" (shown in list).
const RECENT_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Maximum number of recent sessions to include in the popover payload.
const MAX_RECENT_SESSIONS: usize = 20;

/// Per-provider counts, emitted in [`TrayStats`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderCounts {
    pub provider: &'static str,
    /// Sessions with `last_activity` within [`ACTIVE_WINDOW_SECS`].
    pub active: usize,
    /// Sessions with `last_activity` within [`RECENT_WINDOW_SECS`].
    pub recent: usize,
}

/// One entry in the popover's "recent sessions" list.
#[derive(Debug, Clone, Serialize)]
pub struct RecentSession {
    pub provider: &'static str,
    /// Project key (empty string for codex/opencode which are project-less).
    pub project: String,
    pub session_id: String,
    pub last_activity: String,
}

/// Payload for the `tray:stats` event (and `tray_stats_now` response).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrayStats {
    pub counts: Vec<ProviderCounts>,
    pub recent: Vec<RecentSession>,
    pub total_active: usize,
    pub total_recent: usize,
    /// RFC3339 timestamp of the poll that produced this snapshot.
    pub polled_at: String,
}

/// Compute a fresh stats snapshot by walking every provider.
///
/// Errors from individual providers are swallowed — a broken pi install
/// shouldn't hide claude activity. This is the same defensive posture the
/// existing `list_claude_projects` command takes.
pub fn collect_stats() -> TrayStats {
    let now = Utc::now();
    let mut counts: Vec<ProviderCounts> = Vec::new();
    let mut recent: Vec<RecentSession> = Vec::new();

    collect_claude(&now, &mut counts, &mut recent);
    collect_gemini(&now, &mut counts, &mut recent);
    collect_codex(&now, &mut counts, &mut recent);
    collect_opencode(&now, &mut counts, &mut recent);
    collect_pi(&now, &mut counts, &mut recent);

    recent.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    recent.truncate(MAX_RECENT_SESSIONS);

    let total_active = counts.iter().map(|c| c.active).sum();
    let total_recent = counts.iter().map(|c| c.recent).sum();

    TrayStats {
        counts,
        recent,
        total_active,
        total_recent,
        polled_at: now.to_rfc3339(),
    }
}

fn bucket(
    now: &DateTime<Utc>,
    last: Option<DateTime<Utc>>,
) -> (bool /* active */, bool /* recent */) {
    let Some(ts) = last else {
        return (false, false);
    };
    let delta = now.signed_duration_since(ts).num_seconds();
    if delta < 0 {
        // Future timestamps (clock skew) — treat as active.
        return (true, true);
    }
    (delta <= ACTIVE_WINDOW_SECS, delta <= RECENT_WINDOW_SECS)
}

fn collect_claude(
    now: &DateTime<Utc>,
    counts: &mut Vec<ProviderCounts>,
    recent: &mut Vec<RecentSession>,
) {
    let mgr = toolpath_claude::ClaudeConvo::new();
    let mut c = ProviderCounts {
        provider: "claude",
        ..Default::default()
    };
    if !mgr.exists() {
        counts.push(c);
        return;
    }
    let projects = mgr.list_projects().unwrap_or_default();
    for project in projects {
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for meta in metas {
            let (active, is_recent) = bucket(now, meta.last_activity);
            if active {
                c.active += 1;
            }
            if is_recent {
                c.recent += 1;
                if let Some(ts) = meta.last_activity {
                    recent.push(RecentSession {
                        provider: "claude",
                        project: project.clone(),
                        session_id: meta.session_id,
                        last_activity: ts.to_rfc3339(),
                    });
                }
            }
        }
    }
    counts.push(c);
}

fn collect_gemini(
    now: &DateTime<Utc>,
    counts: &mut Vec<ProviderCounts>,
    recent: &mut Vec<RecentSession>,
) {
    let mgr = toolpath_gemini::GeminiConvo::new();
    let mut c = ProviderCounts {
        provider: "gemini",
        ..Default::default()
    };
    if !mgr.exists() {
        counts.push(c);
        return;
    }
    let projects = mgr.list_projects().unwrap_or_default();
    for project in projects {
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for meta in metas {
            let (active, is_recent) = bucket(now, meta.last_activity);
            if active {
                c.active += 1;
            }
            if is_recent {
                c.recent += 1;
                if let Some(ts) = meta.last_activity {
                    recent.push(RecentSession {
                        provider: "gemini",
                        project: project.clone(),
                        session_id: meta.session_uuid.clone(),
                        last_activity: ts.to_rfc3339(),
                    });
                }
            }
        }
    }
    counts.push(c);
}

fn collect_codex(
    now: &DateTime<Utc>,
    counts: &mut Vec<ProviderCounts>,
    recent: &mut Vec<RecentSession>,
) {
    let mgr = toolpath_codex::CodexConvo::new();
    let mut c = ProviderCounts {
        provider: "codex",
        ..Default::default()
    };
    let sessions = mgr.list_sessions().unwrap_or_default();
    for s in sessions {
        let (active, is_recent) = bucket(now, s.last_activity);
        if active {
            c.active += 1;
        }
        if is_recent {
            c.recent += 1;
            if let Some(ts) = s.last_activity {
                recent.push(RecentSession {
                    provider: "codex",
                    project: s
                        .cwd
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    session_id: s.id,
                    last_activity: ts.to_rfc3339(),
                });
            }
        }
    }
    counts.push(c);
}

fn collect_opencode(
    now: &DateTime<Utc>,
    counts: &mut Vec<ProviderCounts>,
    recent: &mut Vec<RecentSession>,
) {
    let mgr = toolpath_opencode::OpencodeConvo::new();
    let mut c = ProviderCounts {
        provider: "opencode",
        ..Default::default()
    };
    let sessions = mgr.list_sessions().unwrap_or_default();
    for s in sessions {
        let (active, is_recent) = bucket(now, s.last_activity);
        if active {
            c.active += 1;
        }
        if is_recent {
            c.recent += 1;
            if let Some(ts) = s.last_activity {
                recent.push(RecentSession {
                    provider: "opencode",
                    project: s.project_id.clone(),
                    session_id: s.id,
                    last_activity: ts.to_rfc3339(),
                });
            }
        }
    }
    counts.push(c);
}

fn collect_pi(
    now: &DateTime<Utc>,
    counts: &mut Vec<ProviderCounts>,
    recent: &mut Vec<RecentSession>,
) {
    let mgr = toolpath_pi::PiConvo::new();
    let mut c = ProviderCounts {
        provider: "pi",
        ..Default::default()
    };
    if !mgr.exists() {
        counts.push(c);
        return;
    }
    let projects = mgr.list_projects().unwrap_or_default();
    for project in projects {
        let sessions = match mgr.list_sessions(&project) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for s in sessions {
            let ts = DateTime::parse_from_rfc3339(&s.timestamp)
                .ok()
                .map(|t| t.with_timezone(&Utc));
            let (active, is_recent) = bucket(now, ts);
            if active {
                c.active += 1;
            }
            if is_recent {
                c.recent += 1;
                if let Some(ts) = ts {
                    recent.push(RecentSession {
                        provider: "pi",
                        project: project.clone(),
                        session_id: s.id,
                        last_activity: ts.to_rfc3339(),
                    });
                }
            }
        }
    }
    counts.push(c);
}

/// IPC command — returns the current stats without waiting for the next poll.
///
/// Called by the popover on open so it doesn't display stale data for up to
/// 30 seconds after being shown.
#[tauri::command]
pub fn tray_stats_now() -> TrayStats {
    collect_stats()
}

/// IPC command — show + focus the main window.
///
/// Called by the popover's "Open Toolpath" button. Mirrors the tray menu's
/// `tray:open` action so the popover doesn't need a separate permission for
/// addressing another window by label from JS.
#[tauri::command]
pub fn tray_open_main(app: AppHandle) {
    show_main(&app);
    hide_popover(&app);
}

/// Payload pushed to the main window after a trace is derived on the tray
/// side. The main window's reducer consumes this as a `DeriveSucceeded` msg.
#[derive(Debug, Clone, Serialize)]
pub struct TraceOpenedPayload {
    pub doc: serde_json::Value,
    pub source: String,
    pub filename: String,
}

/// IPC command — derive the trace for a single session and surface it in the
/// main window's preview.
///
/// Callable from the popover when the user clicks a recent session. Only the
/// providers with a desktop-side derive command are supported (claude, pi).
/// For gemini/codex/opencode the popover should disable the row instead of
/// calling this.
#[tauri::command]
pub fn tray_open_trace(
    app: AppHandle,
    provider: String,
    project: String,
    session_id: String,
) -> Result<(), String> {
    let (doc, source, filename) = match provider.as_str() {
        "claude" => {
            let value = crate::commands::derive::derive_claude(
                project.clone(),
                vec![session_id.clone()],
                /* include_thinking */ false,
            )
            .map_err(|e| e.to_string())?;
            let filename = format!(
                "claude-{}-{}.path.json",
                basename_slug(&project),
                short(&session_id)
            );
            (value, format!("Claude: {}", basename(&project)), filename)
        }
        "pi" => {
            let value = crate::commands::derive::derive_pi(
                project.clone(),
                vec![session_id.clone()],
                /* include_thinking */ false,
            )
            .map_err(|e| e.to_string())?;
            let filename = format!(
                "pi-{}-{}.path.json",
                basename_slug(&project),
                short(&session_id)
            );
            (value, format!("pi.dev: {}", basename(&project)), filename)
        }
        // Not wired up in the desktop backend yet. The popover disables
        // rows for these, but we still reject politely if one slips through.
        "gemini" | "codex" | "opencode" => {
            return Err(format!(
                "Opening {provider} traces from Quick View isn't wired up yet."
            ));
        }
        other => return Err(format!("unknown provider: {other}")),
    };

    let payload = TraceOpenedPayload {
        doc,
        source,
        filename,
    };
    show_main(&app);
    hide_popover(&app);
    app.emit_to("main", "trace:opened", payload)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn basename(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

fn basename_slug(path: &str) -> String {
    basename(path)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Install the tray icon, menu, event handlers, and the background poller.
///
/// Called from `setup` in `main.rs`. Safe to call once per app lifetime.
pub fn install(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle().clone();

    let menu_open = MenuItem::with_id(app, "tray:open", "Open Toolpath", true, None::<&str>)?;
    let menu_refresh = MenuItem::with_id(app, "tray:refresh", "Refresh now", true, None::<&str>)?;
    let menu_quit = MenuItem::with_id(app, "tray:quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&menu_open, &menu_refresh, &menu_quit])?;

    let _tray = TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("bundle provides a default window icon"),
        )
        .icon_as_template(true)
        .title("·")
        .tooltip("Toolpath — no activity")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "tray:quit" => app.exit(0),
            "tray:open" => show_main(app),
            "tray:refresh" => publish_stats(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            tauri_plugin_positioner::on_tray_event(app, &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(app);
            }
        })
        .build(app)?;

    // Kick off the poller. One initial publish so the tray title reflects
    // reality without a 30s wait.
    let poll_handle = Arc::new(handle.clone());
    thread::spawn(move || {
        publish_stats(&poll_handle);
        loop {
            thread::sleep(POLL_INTERVAL);
            publish_stats(&poll_handle);
        }
    });

    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_popover(app: &AppHandle) {
    let Some(w) = app.get_webview_window("popover") else {
        return;
    };
    let visible = w.is_visible().unwrap_or(false);
    if visible {
        let _ = w.hide();
        return;
    }
    let _ = w.move_window(Position::TrayBottomCenter);
    let _ = w.show();
    let _ = w.set_focus();
}

fn hide_popover(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("popover") {
        let _ = w.hide();
    }
}

fn publish_stats(app: &AppHandle) {
    let stats = collect_stats();

    // Update the tray title with a compact activity indicator.
    if let Some(tray) = app.tray_by_id("main") {
        let title = match stats.total_active {
            0 => "·".to_string(),
            n => format!("● {n}"),
        };
        let tooltip = format!(
            "Toolpath — {} active, {} recent",
            stats.total_active, stats.total_recent
        );
        let _ = tray.set_title(Some(&title));
        let _ = tray.set_tooltip(Some(&tooltip));
    }

    let _ = app.emit("tray:stats", stats);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn bucket_classifies_activity_windows() {
        let now = Utc::now();

        // None → neither active nor recent.
        assert_eq!(bucket(&now, None), (false, false));

        // 10s ago → both active and recent.
        assert_eq!(
            bucket(&now, Some(now - Duration::seconds(10))),
            (true, true)
        );

        // 5 minutes ago → recent but not active.
        assert_eq!(
            bucket(&now, Some(now - Duration::minutes(5))),
            (false, true)
        );

        // 2 days ago → neither.
        assert_eq!(bucket(&now, Some(now - Duration::days(2))), (false, false));

        // Future timestamp (clock skew) → both (optimistic).
        assert_eq!(
            bucket(&now, Some(now + Duration::seconds(30))),
            (true, true)
        );
    }

    #[test]
    fn collect_stats_runs_without_panic() {
        // With no provider data on this machine the call should still
        // produce a well-formed snapshot with all five provider slots.
        let s = collect_stats();
        let providers: Vec<_> = s.counts.iter().map(|c| c.provider).collect();
        assert_eq!(
            providers,
            vec!["claude", "gemini", "codex", "opencode", "pi"]
        );
    }

    #[test]
    fn basename_slug_handles_paths_and_empty() {
        assert_eq!(basename_slug("/Users/alex/proj"), "proj");
        assert_eq!(basename_slug("my project!"), "my-project-");
        assert_eq!(basename_slug(""), "");
    }

    #[test]
    fn short_truncates_session_ids() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
    }
}
