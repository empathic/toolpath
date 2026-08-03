//! Debounced async preview pipeline for the native picker. NO tokio —
//! plain `std::thread` workers reporting over an `mpsc` channel, driven
//! by a *pure* scheduler state machine that the event loop polls.
//!
//! Flow: selection changes arm a 100 ms debounce; when it fires (and
//! the row isn't cached) the scheduler emits a [`SpawnRequest`] with a
//! bumped generation. The event loop spawns the preview command via
//! [`spawn_preview_job`]; any previously running command is killed
//! best-effort through the shared kill slot. Results come back as
//! [`PreviewMsg`]s; stale generations are dropped on receipt.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use ratatui::text::Text;

/// Debounce window between a selection change and the preview spawn.
/// Long enough to coalesce held-down arrow keys, short enough to feel
/// instant on a deliberate stop.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(100);

/// Where the preview pane sits relative to the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Up,
    Down,
    Left,
    Right,
}

/// Preview pane size: a percentage of the split axis or absolute lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneSize {
    Percent(u16),
    Lines(u16),
}

/// Wrapping mode for preview text. ratatui's `Paragraph` wraps at word
/// boundaries, so `Wrap` and `WrapWord` render identically — both are
/// kept so the parse stays faithful to the fzf notation callers pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WrapMode {
    WrapWord,
    Wrap,
    NoWrap,
}

/// Parsed `--preview-window` spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PreviewWindow {
    pub side: Side,
    pub size: PaneSize,
    pub wrap: WrapMode,
}

impl Default for PreviewWindow {
    fn default() -> Self {
        Self {
            side: Side::Right,
            size: PaneSize::Percent(60),
            wrap: WrapMode::WrapWord,
        }
    }
}

/// Parse fzf `--preview-window` notation: colon-separated tokens in any
/// order — a side (`up`/`down`/`left`/`right`), a size (`60%` or an
/// absolute line count), and a wrap mode (`wrap`/`wrap-word`/`nowrap`).
/// NEVER errors: unknown tokens are ignored and missing ones fall back
/// to the defaults (right / 60% / wrap-word).
pub(super) fn parse_preview_window(s: &str) -> PreviewWindow {
    let mut out = PreviewWindow::default();
    for token in s.split(':') {
        let token = token.trim();
        match token {
            "up" => out.side = Side::Up,
            "down" => out.side = Side::Down,
            "left" => out.side = Side::Left,
            "right" => out.side = Side::Right,
            "wrap" => out.wrap = WrapMode::Wrap,
            "wrap-word" => out.wrap = WrapMode::WrapWord,
            "nowrap" => out.wrap = WrapMode::NoWrap,
            _ => {
                if let Some(pct) = token.strip_suffix('%') {
                    if let Ok(n) = pct.parse::<u16>() {
                        out.size = PaneSize::Percent(n.min(100));
                    }
                } else if let Ok(n) = token.parse::<u16>() {
                    out.size = PaneSize::Lines(n);
                }
                // Anything else: unknown token, deliberately ignored.
            }
        }
    }
    out
}

/// A finished preview: renderable text, or a failure with the first
/// stderr line of the command.
#[derive(Debug, Clone)]
pub(super) enum PreviewContent {
    Ready(Text<'static>),
    Failed(String),
}

/// Message from a preview worker thread back to the event loop.
#[derive(Debug)]
pub(super) struct PreviewMsg {
    pub generation: u64,
    pub row: usize,
    pub content: PreviewContent,
}

/// A spawn the scheduler decided on: run the preview for `row`, tagged
/// with the generation whose results are still welcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpawnRequest {
    pub row: usize,
    pub generation: u64,
}

/// Pure debounce state machine. Owns no threads, does no IO — the
/// event loop feeds it selection changes, polls it with `now`, and
/// routes worker messages back in. Fully deterministic under test.
pub(super) struct PreviewScheduler {
    /// The debounced (row, due-time) waiting to fire.
    pending: Option<(usize, Instant)>,
    /// Bumped on every spawn; messages from older generations are
    /// dropped so a slow superseded command can't overwrite a newer
    /// preview.
    generation: u64,
    /// Finished previews by row index. Failures are cached too — a
    /// broken preview command shouldn't be re-run on every selection
    /// bounce.
    cache: HashMap<usize, PreviewContent>,
}

impl PreviewScheduler {
    pub fn new() -> Self {
        Self {
            pending: None,
            generation: 0,
            cache: HashMap::new(),
        }
    }

    /// The highlighted row changed: (re-)arm the debounce.
    pub fn on_selection_change(&mut self, row: usize, now: Instant) {
        self.pending = Some((row, now + DEBOUNCE));
    }

    /// Fire the debounce if due. Cache hits spawn nothing. A returned
    /// request has already bumped the generation — the caller must
    /// actually spawn it.
    pub fn poll(&mut self, now: Instant) -> Option<SpawnRequest> {
        let (row, due) = self.pending?;
        if now < due {
            return None;
        }
        self.pending = None;
        if self.cache.contains_key(&row) {
            return None;
        }
        self.generation += 1;
        Some(SpawnRequest {
            row,
            generation: self.generation,
        })
    }

    /// Accept a worker message. Stale generations are dropped. Returns
    /// whether the message affects `current_row` (i.e. the pane should
    /// redraw with new content).
    pub fn on_msg(&mut self, msg: PreviewMsg, current_row: Option<usize>) -> bool {
        if msg.generation != self.generation {
            return false;
        }
        let affects = current_row == Some(msg.row);
        self.cache.insert(msg.row, msg.content);
        affects
    }

    /// Cached preview for `row`, if any.
    pub fn cached(&self, row: usize) -> Option<&PreviewContent> {
        self.cache.get(&row)
    }
}

/// Substitute fzf-style field placeholders into a preview template:
/// `{1}`..`{n}` become the row's shell-quoted fields, `{}` the quoted
/// whole original line. Unknown `{...}` runs (e.g. an unsubstituted
/// `{exe}`) pass through untouched. Out-of-range indices substitute an
/// empty quoted string so the command still parses.
pub(super) fn substitute_placeholders(template: &str, fields: &[String], original: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find('}') {
            None => {
                out.push_str(after);
                rest = "";
                break;
            }
            Some(close) => {
                let inner = &after[1..close];
                if inner.is_empty() {
                    out.push_str(&crate::fuzzy::shell_quote(original));
                } else if inner.chars().all(|c| c.is_ascii_digit()) {
                    let idx: usize = inner.parse().unwrap_or(0);
                    let value = idx
                        .checked_sub(1)
                        .and_then(|i| fields.get(i))
                        .map(String::as_str)
                        .unwrap_or("");
                    out.push_str(&crate::fuzzy::shell_quote(value));
                } else {
                    // Not a field placeholder — emit verbatim.
                    out.push_str(&after[..=close]);
                }
                rest = &after[close + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Shared handle to the currently running preview child, if any.
/// Superseding spawns kill it best-effort so at most one preview
/// command runs at a time.
pub(super) type KillSlot = Arc<Mutex<Option<Child>>>;

pub(super) fn new_kill_slot() -> KillSlot {
    Arc::new(Mutex::new(None))
}

/// Kill and reap whatever child currently occupies the slot.
fn supersede(slot: &KillSlot) {
    let prev = slot.lock().expect("kill slot poisoned").take();
    if let Some(mut child) = prev {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Spawn the preview command for `req` on a worker thread. `command`
/// is the fully substituted shell command line; `pane` is the preview
/// pane's inner `(columns, lines)`, exported as `COLUMNS` /
/// `FZF_PREVIEW_COLUMNS` / `FZF_PREVIEW_LINES` for fzf-compatible
/// preview scripts. The previous preview child (if any) is killed
/// before the new one starts.
pub(super) fn spawn_preview_job(
    req: SpawnRequest,
    command: String,
    pane: (u16, u16),
    tx: Sender<PreviewMsg>,
    slot: KillSlot,
) {
    supersede(&slot);
    std::thread::spawn(move || {
        if let Some(content) = run_preview_command(&command, pane, &slot) {
            // Receiver gone means the picker already exited — fine.
            let _ = tx.send(PreviewMsg {
                generation: req.generation,
                row: req.row,
                content,
            });
        }
    });
}

/// Run one preview command to completion. Returns `None` when the
/// child was superseded mid-run (a newer spawn killed and reaped it) —
/// its output is stale by definition and no message should be sent.
fn run_preview_command(command: &str, pane: (u16, u16), slot: &KillSlot) -> Option<PreviewContent> {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    let spawned = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("COLUMNS", pane.0.to_string())
        .env("FZF_PREVIEW_COLUMNS", pane.0.to_string())
        .env("FZF_PREVIEW_LINES", pane.1.to_string())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return Some(PreviewContent::Failed(format!("failed to spawn preview: {e}"))),
    };
    let child_id = child.id();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    // Park the child in the kill slot so a superseding spawn can kill
    // it while we block on its output.
    *slot.lock().expect("kill slot poisoned") = Some(child);

    // Drain stderr on a helper thread to avoid a pipe-buffer deadlock
    // when a preview command is chatty on both streams.
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let mut stdout = Vec::new();
    if let Some(pipe) = stdout_pipe.as_mut() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let stderr = stderr_handle.join().unwrap_or_default();

    // Reap: only if the slot still holds *our* child. If a newer spawn
    // superseded us it already killed and reaped it, and our output is
    // stale.
    let ours = {
        let mut guard = slot.lock().expect("kill slot poisoned");
        match guard.as_ref() {
            Some(c) if c.id() == child_id => guard.take(),
            _ => None,
        }
    };
    let mut child = ours?;
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => return Some(PreviewContent::Failed(format!("preview wait failed: {e}"))),
    };

    if !status.success() {
        let first_line = String::from_utf8_lossy(&stderr)
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("preview command exited with {status}"));
        return Some(PreviewContent::Failed(first_line));
    }

    Some(PreviewContent::Ready(ansi_bytes_to_text(&stdout)))
}

/// Convert ANSI-styled bytes into a ratatui `Text`. On conversion
/// failure, fall back to a plain de-ANSI'd rendering — an ugly preview
/// beats a crashed picker.
pub(super) fn ansi_bytes_to_text(bytes: &[u8]) -> Text<'static> {
    match bytes.into_text() {
        Ok(text) => text,
        Err(_) => Text::raw(strip_ansi(&String::from_utf8_lossy(bytes))),
    }
}

/// Best-effort ANSI escape removal: drops CSI (`ESC [ ... <final>`)
/// and OSC (`ESC ] ... BEL`/`ESC \`) sequences, keeps everything else.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameter/intermediate bytes then a final byte
                // in `@`..`~`.
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: terminated by BEL or ESC \.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Bare escape (or two-char sequence): drop the escape and
            // let the next char through on the following iteration.
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn parse_preview_window_right_percent_wrap_word() {
        let w = parse_preview_window("right:60%:wrap-word");
        assert_eq!(w.side, Side::Right);
        assert_eq!(w.size, PaneSize::Percent(60));
        assert_eq!(w.wrap, WrapMode::WrapWord);
    }

    #[test]
    fn parse_preview_window_up_stacked() {
        let w = parse_preview_window("up:60%:wrap-word");
        assert_eq!(w.side, Side::Up);
        assert_eq!(w.size, PaneSize::Percent(60));
        // Order tolerance: same tokens shuffled parse identically.
        assert_eq!(parse_preview_window("wrap-word:up:60%"), w);
        // Absolute line counts parse as lines, not percent.
        assert_eq!(
            parse_preview_window("down:15").size,
            PaneSize::Lines(15),
        );
    }

    #[test]
    fn parse_preview_window_tolerates_unknown_tokens_with_defaults() {
        let w = parse_preview_window("frobnicate:~3:!!");
        assert_eq!(w, PreviewWindow::default());
        assert_eq!(parse_preview_window(""), PreviewWindow::default());
        // A known token among garbage still lands.
        assert_eq!(parse_preview_window("border-rounded:left").side, Side::Left);
    }

    #[test]
    fn debounce_coalesces_rapid_selection_changes() {
        let mut s = PreviewScheduler::new();
        let t0 = Instant::now();
        s.on_selection_change(1, t0);
        s.on_selection_change(2, t0 + Duration::from_millis(10));
        // Not due yet relative to the *latest* change.
        assert!(s.poll(t0 + Duration::from_millis(50)).is_none());
        // Due: one spawn, for the latest row only.
        let req = s.poll(t0 + Duration::from_millis(120)).unwrap();
        assert_eq!(req.row, 2);
        // Nothing further pending.
        assert!(s.poll(t0 + Duration::from_millis(500)).is_none());
    }

    #[test]
    fn stale_generation_message_is_dropped() {
        let mut s = PreviewScheduler::new();
        let t0 = Instant::now();
        s.on_selection_change(1, t0);
        let old = s.poll(t0 + DEBOUNCE).unwrap();
        // A newer selection supersedes the first spawn.
        s.on_selection_change(2, t0 + DEBOUNCE);
        let new = s.poll(t0 + DEBOUNCE + DEBOUNCE).unwrap();
        assert!(new.generation > old.generation);
        // The old worker reports late: dropped, nothing cached.
        let accepted = s.on_msg(
            PreviewMsg {
                generation: old.generation,
                row: old.row,
                content: PreviewContent::Ready(Text::raw("stale")),
            },
            Some(old.row),
        );
        assert!(!accepted);
        assert!(s.cached(old.row).is_none());
        // The new worker's message lands.
        assert!(s.on_msg(
            PreviewMsg {
                generation: new.generation,
                row: new.row,
                content: PreviewContent::Ready(Text::raw("fresh")),
            },
            Some(new.row),
        ));
        assert!(s.cached(new.row).is_some());
    }

    #[test]
    fn cache_hit_skips_spawn() {
        let mut s = PreviewScheduler::new();
        let t0 = Instant::now();
        s.on_selection_change(3, t0);
        let req = s.poll(t0 + DEBOUNCE).unwrap();
        s.on_msg(
            PreviewMsg {
                generation: req.generation,
                row: 3,
                content: PreviewContent::Ready(Text::raw("cached")),
            },
            Some(3),
        );
        // Selecting the row again: debounce arms, but poll spawns
        // nothing because the cache already has it.
        s.on_selection_change(3, t0 + Duration::from_millis(500));
        assert!(s.poll(t0 + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn substitute_placeholders_shell_quotes_fields() {
        let fields = vec!["/tmp/o'reilly".to_string(), "sess-1".to_string()];
        let out = substitute_placeholders(
            "path show --project {1} --session {2}",
            &fields,
            "/tmp/o'reilly\tsess-1",
        );
        // The single quote survives via sh quoting.
        assert_eq!(
            out,
            r#"path show --project '/tmp/o'\''reilly' --session 'sess-1'"#
        );
        // `{}` substitutes the quoted whole line.
        let whole = substitute_placeholders("echo {}", &fields, "a\tb");
        assert_eq!(whole, "echo 'a\tb'");
        // Out-of-range index quotes an empty string; `{exe}`-style
        // non-numeric placeholders pass through.
        assert_eq!(
            substitute_placeholders("{exe} p {9}", &fields, "x"),
            "{exe} p ''"
        );
    }

    #[test]
    fn ansi_conversion_of_markdown_to_ansi_sample() {
        let ansi = crate::term::markdown_to_ansi("# Title\n**bold**");
        let text = ansi_bytes_to_text(ansi.as_bytes());
        insta::assert_debug_snapshot!(text);
    }

    #[test]
    #[cfg(unix)]
    fn failed_command_yields_failed_state_with_stderr_line() {
        let (tx, rx) = mpsc::channel();
        let slot = new_kill_slot();
        spawn_preview_job(
            SpawnRequest {
                row: 0,
                generation: 1,
            },
            "echo boom >&2; echo more-noise >&2; exit 3".to_string(),
            (80, 20),
            tx,
            slot,
        );
        let msg = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(msg.generation, 1);
        match msg.content {
            PreviewContent::Failed(line) => assert_eq!(line, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn successful_command_yields_ready_text() {
        let (tx, rx) = mpsc::channel();
        spawn_preview_job(
            SpawnRequest {
                row: 2,
                generation: 7,
            },
            "printf 'hello preview'".to_string(),
            (80, 20),
            tx,
            new_kill_slot(),
        );
        let msg = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        match msg.content {
            PreviewContent::Ready(text) => {
                let flat: String = text
                    .lines
                    .iter()
                    .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
                    .collect();
                assert_eq!(flat, "hello preview");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        assert_eq!(strip_ansi("\u{1b}[1mbold\u{1b}[0m"), "bold");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
