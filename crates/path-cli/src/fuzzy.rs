//! Interactive session selection backed by a fuzzy picker.
//!
//! The protocol:
//! - `path p list <provider> --format tsv` emits one session per line,
//!   tab-delimited. For single-keyed providers (codex, opencode) col 1
//!   is the session id. For project-keyed providers (claude, gemini,
//!   pi) col 1 is the project path and col 2 is the session id.
//!   Subsequent columns are display-only.
//! - `path show <provider>` emits a markdown summary for one session,
//!   suitable as the picker's `--preview` command.
//! - `path p import <provider>` (and `path share` / `path resume`) with
//!   no `--session` auto-launches the picker when stdin and stderr are
//!   TTYs. Multi-select produces a `Graph`.
//!
//! Two backends, selected at runtime:
//!
//! - **Native picker** (`crates/path-cli/src/tui/`, compiled in unless
//!   the `embedded-picker` feature is disabled). First-party ratatui
//!   picker: adaptive inline/fullscreen layouts, debounced async
//!   previews, fzf-style query operators. Honors the same `{1}`/`{2}`
//!   preview placeholder grammar and `--with-nth`/`--tiebreak`
//!   notation, so all call sites work with either backend.
//! - **External `fzf`** (escape hatch, forced via `--picker fzf`).
//!   Users keep their own fzf config and keybindings.
//!
//! When neither backend can run (no TTY, or no external fzf with the
//! native picker disabled at build time), callers fall through to the
//! documented manual recipe printed by [`print_recipe`].
#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use clap::ValueEnum;
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Backend override for the interactive fuzzy picker, set once at CLI
/// startup via the global `--picker` flag and read by [`pick`].
///
/// `Auto` (the default) prefers the native picker and falls back to
/// external `fzf` when it isn't compiled in. `Fzf` and `Native` are
/// strict — they error out rather than silently using the other backend.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Picker {
    /// Pick whichever backend is available; prefer the native picker
    /// and fall back to external `fzf` when it isn't compiled in.
    /// When external `fzf` is also on `PATH`, a one-time hint is
    /// printed to stderr advising `--picker fzf` for users who want
    /// their fzf config / keybindings.
    #[default]
    Auto,
    /// Force the external `fzf` binary. Errors if it isn't on `PATH`.
    Fzf,
    /// Force the built-in native picker. Errors if path-cli was built
    /// with `--no-default-features`. `skim` is accepted as a hidden
    /// alias for the backend this one replaced.
    #[value(alias = "skim")]
    Native,
}

static PICKER_OVERRIDE: OnceLock<Picker> = OnceLock::new();

/// Set the global picker preference. Called once at CLI startup from
/// the `--picker` flag handler. Subsequent calls are no-ops — the first
/// value wins, so library callers can't accidentally override the
/// user's explicit choice mid-run.
pub fn set_picker_override(picker: Picker) {
    if picker == Picker::Native && args_used_skim_alias(std::env::args()) {
        eprintln!(
            "note: `--picker skim` is now the native picker; the skim backend was removed in 0.17"
        );
    }
    let _ = PICKER_OVERRIDE.set(picker);
}

/// Detect whether the user literally typed the `skim` alias — clap
/// resolves aliases before we see the parsed value, so the raw args
/// are the only place the spelling survives.
fn args_used_skim_alias<I: IntoIterator<Item = String>>(args: I) -> bool {
    let mut prev_was_picker_flag = false;
    for arg in args {
        if prev_was_picker_flag && arg == "skim" {
            return true;
        }
        prev_was_picker_flag = arg == "--picker";
        if arg == "--picker=skim" {
            return true;
        }
    }
    false
}

fn current_picker() -> Picker {
    PICKER_OVERRIDE.get().copied().unwrap_or_default()
}

/// Returns true when an interactive picker can actually run: stdin and
/// stderr are TTYs *and* at least one backend is available (the native
/// picker compiled in, or external `fzf` on `PATH`). Callers use this
/// as a guard before invoking [`pick`]; on `false` they print the
/// manual recipe via [`print_recipe`].
pub fn available() -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    embedded_picker_available() || external_fzf_available()
}

/// True when the external `fzf` binary is on `PATH`. Used by [`pick`]
/// to decide between the native picker and the external fallback, and
/// to tailor the manual-recipe message.
pub fn external_fzf_available() -> bool {
    which("fzf").is_some()
}

/// True when the native picker was compiled in. Always `true` in the
/// default build; `false` under `--no-default-features`.
#[cfg(feature = "embedded-picker")]
pub const fn embedded_picker_available() -> bool {
    true
}

#[cfg(not(feature = "embedded-picker"))]
pub const fn embedded_picker_available() -> bool {
    false
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Replace tabs / newlines / carriage returns with a single space so a
/// value safely fits inside one TSV cell without breaking the row
/// structure or pushing content onto a new line.
pub(crate) fn tab_safe(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Pad `s` with trailing spaces to `width` *characters* (not bytes) so
/// the next column lines up predictably even with multibyte titles.
/// If `s` is longer than `width`, truncate and replace the last visible
/// character with `…` so the boundary is obvious.
pub(crate) fn pad_or_truncate(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count == width {
        s.to_string()
    } else if count < width {
        format!("{s}{}", " ".repeat(width - count))
    } else {
        let head: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Truncate `s` to `width` characters with a trailing `…` if needed.
/// No padding — used for the trailing column of a picker row where
/// the picker is free to horizontally scroll if the line overflows.
pub(crate) fn clip_chars(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let head: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Last two path segments of `p`, slash-joined. Just enough to
/// disambiguate `…/repo/.claude/worktrees/foo` from
/// `…/other-repo/main` in a picker without spending screen real
/// estate on the full absolute path.
pub(crate) fn project_short(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.rsplit('/').take(2).collect();
    if parts.is_empty() {
        return p.to_string();
    }
    let mut out: Vec<&str> = parts.into_iter().collect();
    out.reverse();
    out.join("/")
}

/// Right-align an item count with its unit (`" 875 msgs"`, `"  12 lines"`,
/// `"   4 entries"`). Providers count different things, so the unit is
/// caller-supplied — kept as a single helper rather than one per unit
/// to keep the per-provider call sites grep-able.
pub(crate) fn count(n: usize, unit: &str) -> String {
    format!("{n:>4} {unit}")
}

/// One visible picker column packed into a single fixed-width string,
/// so columns line up consistently across pickers. Terminal tab stops
/// produce ugly variable gaps; explicit space padding here avoids that.
///
/// Layout: `[{leading} ]{when:16}  {count}  [{project:28}]  {title}`.
///
/// - `leading` is an optional prefix (used by `path share` to inject a
///   cwd-match marker and a padded harness name; `None` for single-provider
///   `path p import <provider>` pickers where neither applies).
/// - `when` renders as `YYYY-MM-DD HH:MM` or a 16-char placeholder when
///   the provider doesn't have a timestamp on hand.
/// - `count` is supplied pre-formatted via [`count`] so the picker
///   doesn't need to know that codex counts lines and pi counts entries.
/// - `project` is optional — omitted in per-project views where the
///   project label would be redundant on every row.
/// - `title` is run through [`clean_for_picker_display`] to strip
///   Claude's slash-command and local-command-caveat envelopes.
pub(crate) fn render_row(
    leading: Option<&str>,
    when: Option<chrono::DateTime<chrono::Utc>>,
    count: &str,
    project: Option<&str>,
    title: &str,
) -> String {
    const PROJECT_WIDTH: usize = 28;
    const TITLE_MAX: usize = 96;
    let leading_segment = match leading {
        Some(s) => format!("{s} "),
        None => String::new(),
    };
    let when_str = when
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "       —        ".to_string());
    let project_segment = match project {
        Some(p) => format!("{}  ", pad_or_truncate(&tab_safe(p), PROJECT_WIDTH)),
        None => String::new(),
    };
    let title_str = clip_chars(&clean_for_picker_display(title), TITLE_MAX);
    format!("{leading_segment}{when_str}  {count}  {project_segment}{title_str}")
}

/// Clean up a user prompt for display in a picker row.
///
/// Claude wraps the first user message of a session with XML envelopes
/// when the user invoked a slash command or a local `!cmd`. In their
/// raw form they're unreadable noise — a row like
/// `<command-message>biko</command-message> <command-name>/biko</command-name> <command-args>From origin/main…</command-args>`
/// pushes the actual intent off the end of the picker line. This helper
/// strips the envelope down to its human-readable kernel:
///
/// - Slash command envelopes render as `/<name> <args>`.
/// - `<local-command-caveat>…</local-command-caveat>` is dropped; if a
///   `<bash-input>…</bash-input>` follows, it renders as `! <cmd>`.
/// - Any other XML-like wrappers are stripped, keeping inner content.
///
/// The original prompt is preserved in the session file and shown in
/// full via the `path show` preview pane — this only affects the
/// truncated row text used for picker scanning.
pub(crate) fn clean_for_picker_display(s: &str) -> String {
    if let Some(out) = strip_slash_command_envelope(s) {
        return out;
    }
    if let Some(out) = strip_local_command_caveat(s) {
        return out;
    }
    strip_xml_tags(s)
}

/// Recognize `<command-message>X</command-message> <command-name>/X</command-name> <command-args>Y</command-args>`
/// and re-emit it as `/X Y`. Returns None if the envelope isn't present
/// so the caller can fall through to the next strategy.
fn strip_slash_command_envelope(s: &str) -> Option<String> {
    let name = extract_tag_content(s, "command-name")?;
    let args = extract_tag_content(s, "command-args").unwrap_or_default();
    // `name` already includes the leading `/` per Claude's format.
    let name = name.trim();
    let args = args.trim();
    if args.is_empty() {
        Some(name.to_string())
    } else {
        Some(format!("{name} {args}"))
    }
}

/// Recognize `<local-command-caveat>…</local-command-caveat>` blocks.
/// If a `<bash-input>cmd</bash-input>` follows, re-emit as `! cmd`; if
/// not, fall back to stripping the caveat block and returning what's
/// left. Returns None when no caveat is present.
fn strip_local_command_caveat(s: &str) -> Option<String> {
    if !s.contains("<local-command-caveat>") {
        return None;
    }
    if let Some(cmd) = extract_tag_content(s, "bash-input") {
        return Some(format!("! {}", cmd.trim()));
    }
    // No bash-input — strip the caveat block and return whatever's left.
    let stripped = remove_block(s, "<local-command-caveat>", "</local-command-caveat>");
    Some(strip_xml_tags(&stripped))
}

/// Extract the inner text of the first `<tag>…</tag>` in `s`.
fn extract_tag_content(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)?;
    Some(s[start..start + end].to_string())
}

/// Remove the substring from the first `start` marker through the next
/// `end` marker, inclusive. No-op when either marker isn't found.
fn remove_block(s: &str, start: &str, end: &str) -> String {
    let Some(open_idx) = s.find(start) else {
        return s.to_string();
    };
    let after_open = open_idx + start.len();
    let Some(close_off) = s[after_open..].find(end) else {
        return s.to_string();
    };
    let close_idx = after_open + close_off + end.len();
    format!("{}{}", &s[..open_idx], &s[close_idx..])
}

/// Strip all `<tag>` and `</tag>` markers, keeping the inner text.
/// Best-effort fallback for envelopes we don't have a structured
/// renderer for. Collapses consecutive whitespace afterward so the
/// resulting row doesn't have weird gaps.
fn strip_xml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // Collapse runs of whitespace.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_space = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    collapsed.trim().to_string()
}

/// Substitute the `{exe}` placeholder in a preview command with the
/// shell-quoted absolute path of the currently-running binary.
///
/// Why this exists: previews used to hard-code `path show …`, which
/// resolves whatever `path` is first on `$PATH`. Users with an older
/// `path` from `cargo install path-cli` would see preview errors
/// (mismatched flags) instead of the rendered session. Substituting
/// the running binary makes the preview self-consistent — same code
/// runs in the picker, the preview, and the eventual import.
pub(crate) fn substitute_exe_placeholder(preview: &str) -> String {
    let exe = match std::env::current_exe() {
        Ok(p) => shell_quote(&p.to_string_lossy()),
        // Fallback to bare `path` if we can't locate ourselves — preserves
        // the previous behavior rather than blowing up the preview.
        Err(_) => "path".to_string(),
    };
    preview.replace("{exe}", &exe)
}

/// Single-quote a value for embedding in a `/bin/sh -c` command line.
/// Any embedded single-quote becomes `'\''` so the value survives
/// intact. Used for the `{exe}` substitution here and the `{1}`..`{n}`
/// field substitutions in the native picker's preview pipeline.
pub(crate) fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Outcome of a picker invocation.
///
/// Distinguishes a deliberate user cancel (Esc / Ctrl-C, fzf exit 130)
/// from the no-match case (fzf exit 1). Callers that want to surface a
/// non-zero exit on cancel can match on `Cancelled`; callers that just
/// want the picked lines treat both `NoMatch` and `Cancelled` as "empty
/// selection".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickResult {
    /// Picker exited cleanly with at least one selected line.
    Selected(Vec<String>),
    /// Picker exited cleanly with nothing matched / nothing selected.
    NoMatch,
    /// User pressed Esc / Ctrl-C / Ctrl-D.
    Cancelled,
}

/// Run the fuzzy picker with the supplied lines and options. Honors
/// the global `--picker` override; defaults to `Auto`, which prefers
/// the external `fzf` binary when it's on `PATH` (so users keep their
/// own fzf config / keybindings) and falls back to the embedded skim
/// picker otherwise. Errors out only when the requested backend isn't
/// available — callers should check [`available`] first.
pub fn pick(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    match current_picker() {
        Picker::Fzf => {
            if !external_fzf_available() {
                anyhow::bail!(
                    "`--picker fzf` requested but `fzf` isn't on PATH; install it or pass `--picker auto`/`native`"
                );
            }
            pick_external(lines, opts)
        }
        Picker::Native => pick_embedded(lines, opts),
        Picker::Auto => {
            // The native picker is the default: it's compiled in,
            // behaves consistently across versions, and we control its
            // whole surface. Fall back to external fzf only when it
            // isn't compiled in (`--no-default-features` builds).
            if embedded_picker_available() {
                if external_fzf_available() {
                    hint_external_fzf_available();
                }
                pick_embedded(lines, opts)
            } else {
                pick_external(lines, opts)
            }
        }
    }
}

/// Print a one-time hint (per process) on stderr letting the user know
/// they can opt into their installed `fzf` via `--picker fzf`. Only
/// fires when stderr is a TTY so we don't pollute non-interactive logs.
fn hint_external_fzf_available() {
    static SHOWN: OnceLock<()> = OnceLock::new();
    if !std::io::stderr().is_terminal() {
        return;
    }
    if SHOWN.set(()).is_ok() {
        eprintln!(
            "hint: pass `--picker fzf` to use your installed fzf (and its config + keybindings) instead of the built-in picker."
        );
    }
}

/// Invoke the native picker. Compiled-out under
/// `--no-default-features`, in which case calling this returns a clear
/// error rather than the cryptic "no backend" surface.
#[cfg(feature = "embedded-picker")]
fn pick_embedded(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    crate::tui::pick(lines, opts)
}

#[cfg(not(feature = "embedded-picker"))]
fn pick_embedded(_lines: &[String], _opts: &PickOptions<'_>) -> Result<PickResult> {
    anyhow::bail!(
        "the native picker isn't compiled in (built with `--no-default-features`); install `fzf` and pass `--picker fzf` or rebuild with the default `embedded-picker` feature"
    )
}

/// Probed `(major, minor)` from `fzf --version`. Cached after the first
/// call so we don't shell out per-pick. `None` when fzf isn't on PATH or
/// its output doesn't parse — callers treat that as "conservative
/// defaults" (assume the oldest behavior).
fn fzf_version() -> Option<(u32, u32)> {
    static CACHED: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let output = Command::new("fzf").arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        parse_fzf_version(&String::from_utf8_lossy(&output.stdout))
    })
}

/// Extract `(major, minor)` from an `fzf --version` line. The output
/// looks like `0.65.0 (a1b2c3d)` or `0.46.0` or `0.72.0 (Homebrew)`.
fn parse_fzf_version(s: &str) -> Option<(u32, u32)> {
    let head = s.split_whitespace().next()?;
    let mut parts = head.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// `wrap-word` as a `--preview-window` modifier landed in fzf 0.55
/// (June 2024). On older fzf the unknown modifier aborts the picker
/// before it reads stdin, so substitute plain `wrap` instead — it's
/// been supported for years and the only visible difference is wrapping
/// at character boundaries rather than word boundaries.
fn adjust_preview_window(window: &str, version: Option<(u32, u32)>) -> String {
    let supports_wrap_word = version.map(|v| v >= (0, 55)).unwrap_or(false);
    if supports_wrap_word {
        window.to_string()
    } else {
        window.replace("wrap-word", "wrap")
    }
}

/// `--preview-wrap-sign` landed in fzf 0.50. Older fzf rejects it; the
/// default sign (`↳`) is fine, so just omit the flag.
fn supports_preview_wrap_sign(version: Option<(u32, u32)>) -> bool {
    version.map(|v| v >= (0, 50)).unwrap_or(false)
}

/// Run the external `fzf` binary directly. Same surface as [`pick`] —
/// kept as a dedicated path so call sites that *must* hit external fzf
/// (e.g. integration tests pinning behavior) can opt in.
pub fn pick_external(lines: &[String], opts: &PickOptions<'_>) -> Result<PickResult> {
    let mut args: Vec<String> = vec![
        "--delimiter=\t".into(),
        format!("--with-nth={}", opts.with_nth),
        format!("--prompt={}", opts.prompt),
        format!("--tiebreak={}", opts.tiebreak),
    ];

    if opts.multi {
        args.push("--multi".into());
    }

    let version = fzf_version();
    if let Some(preview) = opts.preview {
        args.push(format!("--preview={}", substitute_exe_placeholder(preview)));
        args.push(format!(
            "--preview-window={}",
            adjust_preview_window(opts.preview_window, version)
        ));
        if supports_preview_wrap_sign(version) {
            args.push("--preview-wrap-sign=".into());
        }
    }

    if let Some(header) = opts.header {
        args.push(format!("--header={}", header));
    }

    let mut child = Command::new("fzf")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn fzf")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Failed to open fzf stdin"))?;
        for line in lines {
            // If fzf exited early (e.g. an unrecognized arg on an old
            // version), writing yields BrokenPipe. Stop feeding lines so
            // the user sees fzf's real stderr message instead of our
            // noisy pipe error — `wait_with_output` below surfaces the
            // exit status.
            if let Err(err) = stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
            {
                if err.kind() == std::io::ErrorKind::BrokenPipe {
                    break;
                }
                return Err(err.into());
            }
        }
    }

    let output = child.wait_with_output().context("Failed to wait on fzf")?;

    // fzf exit codes: 0 = match, 1 = no match, 2 = error, 130 = cancelled.
    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(PickResult::Selected(
                text.lines().map(|s| s.to_string()).collect(),
            ))
        }
        Some(1) => Ok(PickResult::NoMatch),
        Some(130) => Ok(PickResult::Cancelled),
        _ => anyhow::bail!("fzf exited with status {:?}", output.status),
    }
}

/// Options for a picker invocation. Field names mirror fzf's flags;
/// the embedded skim backend maps them onto its equivalents.
pub struct PickOptions<'a> {
    /// Visible columns in fzf's notation, e.g. `2..` to hide col 1.
    pub with_nth: &'a str,
    /// Prompt string shown to the left of the input.
    pub prompt: &'a str,
    /// Optional `--preview` command. Use `{1}`, `{2}` ... to substitute fields.
    pub preview: Option<&'a str>,
    /// `--preview-window` placement. Defaults to `right:60%:wrap` (side-by-side);
    /// pass `up:60%:wrap` for a stacked layout that fits narrow terminals.
    pub preview_window: &'a str,
    /// Optional header line shown above the list.
    pub header: Option<&'a str>,
    /// Tiebreak ordering — `index` preserves input order.
    pub tiebreak: &'a str,
    /// Allow selecting multiple rows.
    pub multi: bool,
}

impl Default for PickOptions<'_> {
    fn default() -> Self {
        Self {
            with_nth: "2..",
            prompt: "> ",
            preview: None,
            preview_window: "right:60%:wrap-word",
            header: None,
            tiebreak: "index",
            multi: false,
        }
    }
}

/// Print a manual recipe for users who can't get an interactive picker.
/// Reached when stdin/stderr aren't TTYs, or — under
/// `--no-default-features` — when external `fzf` is also missing.
/// `provider` is the subcommand name; `project_keyed` toggles the
/// column layout.
pub fn print_recipe(provider: &str, project_keyed: bool) {
    let leader = if !external_fzf_available() && !embedded_picker_available() {
        "Interactive selection needs `fzf` on PATH (or a build with the \
         `embedded-picker` feature) and a TTY."
    } else {
        "Interactive selection needs a TTY."
    };

    if project_keyed {
        eprintln!(
            "{leader}\n\
             \n\
             Manual recipe:\n  \
             path p list {provider} --format tsv \\\n    \
             | fzf --delimiter=$'\\t' --with-nth=3.. \\\n          \
             --preview 'path show {provider} --project {{1}} --session {{2}}' \\\n    \
             | awk -F'\\t' '{{print $1 \"\\n\" $2}}' \\\n    \
             | xargs -L2 sh -c 'path p import {provider} --project \"$1\" --session \"$2\"' --\n"
        );
    } else {
        eprintln!(
            "{leader}\n\
             \n\
             Manual recipe:\n  \
             path p list {provider} --format tsv \\\n    \
             | fzf --delimiter=$'\\t' --with-nth=2.. \\\n          \
             --preview 'path show {provider} --session {{1}}' \\\n    \
             | cut -f1 \\\n    \
             | xargs -I{{}} path p import {provider} --session {{}}\n"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("/usr/local/bin/path"), "'/usr/local/bin/path'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quote() {
        // A path containing a single quote becomes `'…'\''…'` so the
        // shell reassembles the literal correctly.
        assert_eq!(shell_quote("/o'reilly/bin"), "'/o'\\''reilly/bin'");
    }

    #[test]
    fn substitute_exe_replaces_placeholder() {
        // We don't pin the substituted value (it's `current_exe()` at
        // runtime), but we can confirm the placeholder is consumed and
        // the surrounding template survives.
        let out = substitute_exe_placeholder("{exe} show --ansi claude --session {1}");
        assert!(!out.contains("{exe}"), "placeholder not substituted: {out}");
        assert!(out.contains(" show --ansi claude --session {1}"));
    }

    #[test]
    fn substitute_exe_leaves_other_placeholders_alone() {
        // `{1}`, `{2}` etc. are the picker's column placeholders —
        // they must survive the substitution untouched.
        let out = substitute_exe_placeholder("{exe} show --session {1} --project {2}");
        assert!(out.contains("{1}"));
        assert!(out.contains("{2}"));
    }

    use chrono::TimeZone;

    fn jan_first() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 1, 29, 10, 5, 0).unwrap()
    }

    /// Bare row — no leading prefix, no project, real timestamp.
    /// Pin the exact output so a regression in spacing or column order
    /// shows up as a diff on this test.
    #[test]
    fn render_row_bare_matches_expected_format() {
        let out = render_row(None, Some(jan_first()), "  17 msgs", None, "hello world");
        // {when:16}␣␣{count}␣␣{title}
        assert_eq!(out, "2026-01-29 10:05    17 msgs  hello world");
    }

    /// `path share` injects a scope marker + padded harness name as the
    /// `leading` prefix; assert it's separated from the timestamp by
    /// exactly one space so the layout is grep-able.
    #[test]
    fn render_row_with_leading_prefix_separates_with_one_space() {
        let out = render_row(
            Some("· claude  "),
            Some(jan_first()),
            "  17 msgs",
            None,
            "hi",
        );
        assert!(out.starts_with("· claude   2026-01-29 10:05"), "{out}");
    }

    /// Optional project segment pads to PROJECT_WIDTH (28 chars) so
    /// the title column starts at the same offset on every row.
    #[test]
    fn render_row_pads_project_to_fixed_width() {
        let short = render_row(None, Some(jan_first()), "  17 msgs", Some("a/b"), "t");
        let long = render_row(
            None,
            Some(jan_first()),
            "  17 msgs",
            Some("a-much-longer-name"),
            "t",
        );
        let title_offset = |s: &str| s.find("t").and_then(|_| s.rfind("  t")).unwrap();
        assert_eq!(
            title_offset(&short),
            title_offset(&long),
            "title misaligned across rows:\n short={short:?}\n long={long:?}"
        );
    }

    /// Project longer than PROJECT_WIDTH is truncated with a `…`
    /// sentinel; padding never makes a row wider than the budget.
    #[test]
    fn render_row_truncates_oversized_project_with_ellipsis() {
        let long = "x".repeat(40);
        let out = render_row(None, Some(jan_first()), "  17 msgs", Some(&long), "t");
        // The project segment renders 28 chars wide then `  ` separator.
        // Find the literal "  t" that follows it.
        let project_segment = out
            .strip_prefix("2026-01-29 10:05    17 msgs  ")
            .unwrap()
            .strip_suffix("  t")
            .unwrap();
        assert_eq!(project_segment.chars().count(), 28);
        assert!(project_segment.ends_with('…'), "got: {project_segment:?}");
    }

    /// Missing timestamp renders a 16-char placeholder so subsequent
    /// columns still align with timestamped rows. Compare via *char*
    /// position — the placeholder uses an em dash which is 3 bytes but
    /// 1 char, so byte offsets would diverge despite visual alignment.
    #[test]
    fn render_row_missing_when_renders_placeholder_at_fixed_width() {
        let with = render_row(None, Some(jan_first()), "  17 msgs", None, "t");
        let without = render_row(None, None, "  17 msgs", None, "t");
        let char_offset = |s: &str| {
            s.chars()
                .collect::<Vec<_>>()
                .windows(7)
                .position(|w| w.iter().collect::<String>() == "17 msgs")
        };
        assert_eq!(char_offset(&with), char_offset(&without));
    }

    /// Long titles get clipped with a `…` sentinel; the surrounding
    /// columns are untouched.
    #[test]
    fn render_row_clips_long_title_with_ellipsis() {
        let title = "x".repeat(200);
        let out = render_row(None, Some(jan_first()), "  17 msgs", None, &title);
        let title_part = out.strip_prefix("2026-01-29 10:05    17 msgs  ").unwrap();
        // TITLE_MAX = 96; ellipsis takes one slot.
        assert_eq!(title_part.chars().count(), 96);
        assert!(title_part.ends_with('…'), "got: {title_part:?}");
    }

    /// Slash-command envelopes are stripped from the title via
    /// `clean_for_picker_display` before truncation. Belt-and-suspenders
    /// regression guard for the row noise the picker rows used to show.
    #[test]
    fn render_row_strips_slash_command_envelope_from_title() {
        let raw = "<command-message>biko</command-message> <command-name>/biko</command-name> <command-args>do the thing</command-args>";
        let out = render_row(None, Some(jan_first()), "  17 msgs", None, raw);
        assert!(out.ends_with("/biko do the thing"), "got: {out}");
        assert!(!out.contains("<command-"));
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn skim_alias_detected_in_both_flag_spellings() {
        assert!(args_used_skim_alias(args(&[
            "path", "--picker", "skim", "share"
        ])));
        assert!(args_used_skim_alias(args(&[
            "path",
            "--picker=skim",
            "share"
        ])));
    }

    #[test]
    fn skim_alias_not_detected_for_native_or_unrelated_args() {
        assert!(!args_used_skim_alias(args(&["path", "--picker", "native"])));
        assert!(!args_used_skim_alias(args(&["path", "--picker", "fzf"])));
        // "skim" as a positional (not following --picker) doesn't count.
        assert!(!args_used_skim_alias(args(&["path", "query", "skim"])));
    }

    #[test]
    fn parse_fzf_version_handles_common_shapes() {
        assert_eq!(parse_fzf_version("0.46.0\n"), Some((0, 46)));
        assert_eq!(parse_fzf_version("0.65.0 (a1b2c3d)\n"), Some((0, 65)));
        assert_eq!(parse_fzf_version("0.72.0 (Homebrew)\n"), Some((0, 72)));
        assert_eq!(parse_fzf_version("1.0.0\n"), Some((1, 0)));
    }

    #[test]
    fn parse_fzf_version_rejects_unparseable_input() {
        assert_eq!(parse_fzf_version(""), None);
        assert_eq!(parse_fzf_version("not a version"), None);
        assert_eq!(parse_fzf_version("0"), None);
    }

    #[test]
    fn adjust_preview_window_downgrades_wrap_word_on_old_fzf() {
        // The user's bug: fzf 0.46 rejects `wrap-word`. Downgrade to plain `wrap`.
        assert_eq!(
            adjust_preview_window("up:60%:wrap-word", Some((0, 46))),
            "up:60%:wrap"
        );
        assert_eq!(
            adjust_preview_window("right:60%:wrap-word", Some((0, 54))),
            "right:60%:wrap"
        );
    }

    #[test]
    fn adjust_preview_window_preserves_wrap_word_on_new_fzf() {
        assert_eq!(
            adjust_preview_window("up:60%:wrap-word", Some((0, 55))),
            "up:60%:wrap-word"
        );
        assert_eq!(
            adjust_preview_window("up:60%:wrap-word", Some((1, 0))),
            "up:60%:wrap-word"
        );
    }

    #[test]
    fn adjust_preview_window_unknown_version_assumes_old() {
        // If we couldn't probe fzf, lean conservative — `wrap` works on
        // every fzf we care about; `wrap-word` doesn't.
        assert_eq!(
            adjust_preview_window("up:60%:wrap-word", None),
            "up:60%:wrap"
        );
    }

    #[test]
    fn supports_preview_wrap_sign_follows_version_floor() {
        assert!(!supports_preview_wrap_sign(Some((0, 49))));
        assert!(supports_preview_wrap_sign(Some((0, 50))));
        assert!(supports_preview_wrap_sign(Some((0, 72))));
        assert!(supports_preview_wrap_sign(Some((1, 0))));
        // Unknown version → omit the flag, default sign is fine.
        assert!(!supports_preview_wrap_sign(None));
    }

    /// `count` right-aligns the number to 4 chars so the unit column
    /// stays in the same place across rows with different magnitudes.
    #[test]
    fn count_right_aligns_number_to_four_chars() {
        assert_eq!(count(7, "msgs"), "   7 msgs");
        assert_eq!(count(1572, "msgs"), "1572 msgs");
        assert_eq!(count(12, "entries"), "  12 entries");
    }
}
