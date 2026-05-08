//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Args, ValueEnum};
use std::path::PathBuf;

use crate::cmd_export::RepoSpec;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum HarnessArg {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

#[derive(Args, Debug)]
pub struct ShareArgs {
    /// Pathbase server URL (defaults to the stored session's server)
    #[arg(long)]
    pub url: Option<String>,

    /// Force the anonymous endpoint, ignoring any stored credentials
    #[arg(long, conflicts_with_all = ["repo", "public"])]
    pub anon: bool,

    /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
    #[arg(long, value_parser = crate::cmd_export::parse_repo_spec)]
    pub repo: Option<RepoSpec>,

    /// Override the auto-derived slug (defaults to the toolpath document id)
    #[arg(long)]
    pub slug: Option<String>,

    /// Make the uploaded path publicly listable (default: secret/unlisted)
    #[arg(long)]
    pub public: bool,

    /// Narrow the picker to one harness, or skip the picker entirely
    /// when used with --session.
    #[arg(long, value_enum)]
    pub harness: Option<HarnessArg>,

    /// Skip the picker. Requires --harness; requires --project for
    /// claude/gemini/pi.
    #[arg(long, requires = "harness")]
    pub session: Option<String>,

    /// Override cwd-as-project. Filters the picker to sessions tied to
    /// this project across all harnesses.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Overwrite the cache entry if it already exists
    #[arg(long)]
    pub force: bool,

    /// Skip writing the cache; derive in-memory only
    #[arg(long)]
    pub no_cache: bool,
}

/// Which agent harness a session was produced by.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Harness {
    Claude,
    Gemini,
    Codex,
    Opencode,
    Pi,
}

impl Harness {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Gemini => "gemini",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
        }
    }

    /// Padded so all five symbols line up in the fzf column.
    pub(crate) fn symbol(&self) -> &'static str {
        match self {
            Harness::Claude => "claude  ",
            Harness::Gemini => "gemini  ",
            Harness::Codex => "codex   ",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi      ",
        }
    }

    /// True when the underlying provider keys sessions by project path.
    /// claude/gemini/pi: true. codex/opencode: false (sessions store cwd
    /// per-row, not as a directory key).
    pub(crate) fn project_keyed(&self) -> bool {
        matches!(self, Harness::Claude | Harness::Gemini | Harness::Pi)
    }

    pub(crate) fn from_arg(arg: HarnessArg) -> Self {
        match arg {
            HarnessArg::Claude => Harness::Claude,
            HarnessArg::Gemini => Harness::Gemini,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Opencode => Harness::Opencode,
            HarnessArg::Pi => Harness::Pi,
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Harness::Claude),
            "gemini" => Some(Harness::Gemini),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::Opencode),
            "pi" => Some(Harness::Pi),
            _ => None,
        }
    }
}

/// One row in the unified session picker.
#[derive(Debug, Clone)]
pub(crate) struct SessionRow {
    pub(crate) harness: Harness,
    /// Project path for keyed providers; `None` for codex/opencode.
    pub(crate) project: Option<String>,
    /// Recorded cwd from the session (codex/opencode only).
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) last_activity: Option<DateTime<Utc>>,
    pub(crate) message_count: usize,
    pub(crate) matches_cwd: bool,
}

/// Bundle of provider managers used during aggregation. Production code
/// builds this from real `$HOME` via `from_environment`; tests construct
/// it directly with provider-specific resolvers.
#[derive(Default)]
pub(crate) struct HarnessBundle {
    pub(crate) claude: Option<toolpath_claude::ClaudeConvo>,
    pub(crate) gemini: Option<toolpath_gemini::GeminiConvo>,
    pub(crate) codex: Option<toolpath_codex::CodexConvo>,
    pub(crate) opencode: Option<toolpath_opencode::OpencodeConvo>,
    pub(crate) pi: Option<toolpath_pi::PiConvo>,
}

impl HarnessBundle {
    /// Build the production bundle. Each provider is included
    /// unconditionally (its `new()` doesn't fail on a missing home dir);
    /// `gather_sessions` skips the ones whose listing returns empty/NotFound.
    pub(crate) fn from_environment() -> Self {
        Self {
            claude: Some(toolpath_claude::ClaudeConvo::new()),
            gemini: Some(toolpath_gemini::GeminiConvo::new()),
            codex: Some(toolpath_codex::CodexConvo::new()),
            opencode: Some(toolpath_opencode::OpencodeConvo::new()),
            pi: Some(toolpath_pi::PiConvo::new()),
        }
    }
}

/// Aggregate sessions across the harnesses in `bundle`, ranked so that
/// rows whose project (or recorded cwd) canonicalizes to `cwd` come
/// first, sorted by descending `last_activity`.
///
/// Filters: `harness_filter` keeps only rows from one harness; `project_filter`
/// keeps only rows whose project (for keyed) or cwd (for session-keyed)
/// canonicalizes to that path.
pub(crate) fn gather_sessions(
    bundle: &HarnessBundle,
    cwd: &std::path::Path,
    harness_filter: Option<Harness>,
    project_filter: Option<&std::path::Path>,
) -> Vec<SessionRow> {
    let mut rows = Vec::new();
    let canonical_cwd = canonicalize_or_self(cwd);
    let canonical_project = project_filter.map(canonicalize_or_self);

    let want = |h: Harness| harness_filter.is_none_or(|f| f == h);

    if want(Harness::Claude)
        && let Some(mgr) = &bundle.claude
    {
        collect_claude(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(Harness::Gemini)
        && let Some(mgr) = &bundle.gemini
    {
        collect_gemini(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(Harness::Pi)
        && let Some(mgr) = &bundle.pi
    {
        collect_pi(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(Harness::Codex)
        && let Some(mgr) = &bundle.codex
    {
        collect_codex(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(Harness::Opencode)
        && let Some(mgr) = &bundle.opencode
    {
        collect_opencode(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }

    rows.sort_by(|a, b| {
        b.matches_cwd
            .cmp(&a.matches_cwd)
            .then_with(|| b.last_activity.cmp(&a.last_activity))
    });
    rows
}

fn canonicalize_or_self(p: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn paths_match(a: &std::path::Path, b: &std::path::Path) -> bool {
    canonicalize_or_self(a) == canonicalize_or_self(b)
}

fn collect_claude(
    mgr: &toolpath_claude::ClaudeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_claude(&e) => return,
        Err(e) => {
            eprintln!("warning: claude aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: claude project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            out.push(SessionRow {
                harness: Harness::Claude,
                project: Some(m.project_path),
                cwd: None,
                session_id: m.session_id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: m.message_count,
                matches_cwd,
            });
        }
    }
}

fn collect_gemini(
    mgr: &toolpath_gemini::GeminiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_gemini(&e) => return,
        Err(e) => {
            eprintln!("warning: gemini aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_conversation_metadata(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: gemini project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            out.push(SessionRow {
                harness: Harness::Gemini,
                project: Some(m.project_path),
                cwd: None,
                session_id: m.session_uuid,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: m.message_count,
                matches_cwd,
            });
        }
    }
}

fn collect_pi(
    mgr: &toolpath_pi::PiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let projects = match mgr.list_projects() {
        Ok(ps) if !ps.is_empty() => ps,
        Ok(_) => return,
        Err(e) if is_not_found_pi(&e) => return,
        Err(e) => {
            eprintln!("warning: pi aggregation failed: {e}");
            return;
        }
    };
    for project in projects {
        let project_path = std::path::Path::new(&project);
        if let Some(filter) = project_filter
            && !paths_match(project_path, filter)
        {
            continue;
        }
        let metas = match mgr.list_sessions(&project) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: pi project {project} failed: {e}");
                continue;
            }
        };
        let matches_cwd = paths_match(project_path, canonical_cwd);
        for m in metas {
            // SessionMeta.timestamp is a String; parse to DateTime when possible.
            let last_activity = chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                .ok()
                .map(|d| d.with_timezone(&Utc));
            out.push(SessionRow {
                harness: Harness::Pi,
                project: Some(project.clone()),
                cwd: None,
                session_id: m.id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity,
                message_count: m.entry_count,
                matches_cwd,
            });
        }
    }
}

fn collect_codex(
    mgr: &toolpath_codex::CodexConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let metas = match mgr.list_sessions() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_codex(&e) => return,
        Err(e) => {
            eprintln!("warning: codex aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        let cwd_str = m.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        if let Some(filter) = project_filter {
            let stored = match cwd_str.as_deref() {
                Some(s) => std::path::PathBuf::from(s),
                None => continue,
            };
            if !paths_match(&stored, filter) {
                continue;
            }
        }
        let matches_cwd = m
            .cwd
            .as_deref()
            .map(|p| paths_match(p, canonical_cwd))
            .unwrap_or(false);
        out.push(SessionRow {
            harness: Harness::Codex,
            project: None,
            cwd: cwd_str,
            session_id: m.id,
            title: m
                .first_user_message
                .unwrap_or_else(|| "(no prompt)".to_string()),
            last_activity: m.last_activity,
            message_count: m.line_count,
            matches_cwd,
        });
    }
}

fn collect_opencode(
    mgr: &toolpath_opencode::OpencodeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<SessionRow>,
) {
    let metas = match mgr.io().list_session_metadata(None) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_opencode(&e) => return,
        Err(e) => {
            eprintln!("warning: opencode aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        if let Some(filter) = project_filter
            && !paths_match(&m.directory, filter)
        {
            continue;
        }
        let matches_cwd = paths_match(&m.directory, canonical_cwd);
        let cwd_str = m.directory.to_string_lossy().into_owned();
        let title = match (&m.first_user_message, m.title.is_empty()) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, false) => m.title.clone(),
            _ => "(no prompt)".to_string(),
        };
        out.push(SessionRow {
            harness: Harness::Opencode,
            project: None,
            cwd: Some(cwd_str),
            session_id: m.id,
            title,
            last_activity: m.last_activity,
            message_count: m.message_count,
            matches_cwd,
        });
    }
}

fn is_not_found_claude(err: &toolpath_claude::ConvoError) -> bool {
    use toolpath_claude::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::ClaudeDirectoryNotFound(_))
}

fn is_not_found_gemini(err: &toolpath_gemini::ConvoError) -> bool {
    use toolpath_gemini::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::GeminiDirectoryNotFound(_))
}

fn is_not_found_pi(err: &toolpath_pi::PiError) -> bool {
    use toolpath_pi::PiError;
    matches!(err, PiError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, PiError::ProjectNotFound(_))
}

fn is_not_found_codex(err: &toolpath_codex::ConvoError) -> bool {
    use toolpath_codex::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::CodexDirectoryNotFound(_))
}

fn is_not_found_opencode(err: &toolpath_opencode::ConvoError) -> bool {
    use toolpath_opencode::ConvoError;
    matches!(err, ConvoError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        || matches!(err, ConvoError::NoHomeDirectory)
        || matches!(err, ConvoError::OpencodeDirectoryNotFound(_))
        || matches!(err, ConvoError::DatabaseNotFound(_))
}

pub fn run(args: ShareArgs) -> Result<()> {
    let harness = args.harness.map(Harness::from_arg);

    if let (Some(h), Some(session)) = (harness, &args.session) {
        return share_explicit(h, session.as_str(), &args);
    }
    if args.session.is_some() && harness.is_none() {
        anyhow::bail!("--session requires --harness");
    }

    let cwd = std::env::current_dir()?;
    let bundle = HarnessBundle::from_environment();
    let project_filter = args.project.as_deref();
    let rows = gather_sessions(&bundle, &cwd, harness, project_filter);

    if rows.is_empty() {
        return bail_no_sessions(&bundle, project_filter);
    }

    if !crate::fzf::available() {
        eprintln!(
            "Interactive `path share` needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path import <harness>      # writes a cache entry, prints its id\n  \
             path export pathbase --input <id>"
        );
        anyhow::bail!("fzf unavailable; run `path import <harness>` then `path export pathbase`");
    }

    let lines: Vec<String> = rows.iter().map(format_picker_row).collect();
    let host = pathbase_host_for_picker(&args);
    let header = format!("share an agent session (Enter = upload to {host})");
    let opts = crate::fzf::PickOptions {
        with_nth: "4..",
        prompt: "share> ",
        preview: Some("path show {1} --project {2} --session {3}"),
        header: Some(&header),
        tiebreak: "index",
        multi: false,
    };
    let selected = crate::fzf::pick(&lines, &opts)?;
    let line = match selected.into_iter().next() {
        Some(l) => l,
        None => return Ok(()), // user cancelled
    };
    let (h, key, session) = parse_picker_row(&line)
        .ok_or_else(|| anyhow::anyhow!("internal: failed to parse picker row"))?;

    let mut explicit = ShareArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        slug: args.slug.clone(),
        public: args.public,
        harness: Some(harness_to_arg(h)),
        session: Some(session.clone()),
        project: if h.project_keyed() {
            Some(PathBuf::from(&key))
        } else {
            None
        },
        force: args.force,
        no_cache: args.no_cache,
    };
    eprintln!(
        "Picked {} session {}",
        h.name(),
        explicit.session.as_deref().unwrap_or("?")
    );
    let session_id = explicit.session.take().unwrap();
    share_explicit(h, &session_id, &explicit)
}

fn harness_to_arg(h: Harness) -> HarnessArg {
    match h {
        Harness::Claude => HarnessArg::Claude,
        Harness::Gemini => HarnessArg::Gemini,
        Harness::Codex => HarnessArg::Codex,
        Harness::Opencode => HarnessArg::Opencode,
        Harness::Pi => HarnessArg::Pi,
    }
}

fn pathbase_host_for_picker(args: &ShareArgs) -> String {
    use crate::cmd_pathbase::resolve_url;
    if let Some(u) = &args.url {
        return resolve_url(Some(u.clone()));
    }
    // Best-effort: if there's a stored session, surface its URL; otherwise fall back to default.
    let path = match crate::cmd_pathbase::credentials_path() {
        Ok(p) => p,
        Err(_) => return resolve_url(None),
    };
    match crate::cmd_pathbase::load_session(&path) {
        Ok(Some(s)) => s.url,
        _ => resolve_url(None),
    }
}

fn bail_no_sessions(bundle: &HarnessBundle, project_filter: Option<&std::path::Path>) -> Result<()> {
    if let Some(p) = project_filter {
        anyhow::bail!(
            "No agent sessions found in project {}. Run without --project to see sessions across all projects.",
            p.display()
        );
    }

    let mut summary = String::from("No agent sessions found.\n");
    summary.push_str(&probe_summary_line("claude", bundle.claude.is_some()));
    summary.push_str(&probe_summary_line("gemini", bundle.gemini.is_some()));
    summary.push_str(&probe_summary_line("codex", bundle.codex.is_some()));
    summary.push_str(&probe_summary_line("opencode", bundle.opencode.is_some()));
    summary.push_str(&probe_summary_line("pi", bundle.pi.is_some()));
    eprint!("{summary}");
    anyhow::bail!("no shareable sessions");
}

fn probe_summary_line(name: &str, present: bool) -> String {
    if present {
        format!("  {name}: 0 sessions\n")
    } else {
        format!("  {name}: not configured\n")
    }
}

fn share_explicit(harness: Harness, session: &str, args: &ShareArgs) -> Result<()> {
    let project = match (harness.project_keyed(), args.project.as_ref()) {
        (true, Some(p)) => Some(p.to_string_lossy().into_owned()),
        (true, None) => anyhow::bail!(
            "--project required when --harness is {} and --session is set",
            harness.name()
        ),
        (false, _) => None,
    };

    let derived = derive_one(harness, project.as_deref(), session)?;
    let summary = format!("{} session {}", harness.name(), derived.cache_id);

    if !args.no_cache {
        let path = crate::cmd_cache::write_cached(&derived.cache_id, &derived.doc, args.force)?;
        eprintln!(
            "Imported {} session → {} ({})",
            harness.name(),
            derived.cache_id,
            path.display()
        );
    }

    let body = derived.doc.to_json()?;
    let upload = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        slug: args.slug.clone(),
        public: args.public,
    };
    crate::cmd_export::run_pathbase_inner(upload, &body, &summary)
}

/// Build the TSV line fed to fzf. Cols 1–3 are hidden (harness/key/session,
/// used as parser keys); cols 4..8 are visible to the user.
fn format_picker_row(row: &SessionRow) -> String {
    let key = row
        .project
        .clone()
        .or_else(|| row.cwd.clone())
        .unwrap_or_default();
    let when = row
        .last_activity
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "          —     ".to_string());
    let scope = if row.matches_cwd { "·" } else { " " };
    let project_short = project_short(&key);
    let title = fzf_title(&row.title);
    format!(
        "{}\t{}\t{}\t{}\t{}\t{} msgs\t{}\t{}\t{}",
        row.harness.name(),
        tab_safe(&key),
        tab_safe(&row.session_id),
        row.harness.symbol(),
        when,
        row.message_count,
        scope,
        tab_safe(&project_short),
        title,
    )
}

/// Inverse of [`format_picker_row`] — pulls (harness, key, session) back
/// out of the line fzf returned. Returns `None` if the line is malformed.
fn parse_picker_row(line: &str) -> Option<(Harness, String, String)> {
    let mut parts = line.split('\t');
    let h = Harness::parse(parts.next()?)?;
    let key = parts.next()?.to_string();
    let session = parts.next()?.to_string();
    if session.is_empty() {
        return None;
    }
    Some((h, key, session))
}

fn tab_safe(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn fzf_title(s: &str) -> String {
    const MAX: usize = 120;
    let safe = tab_safe(s);
    if safe.chars().count() > MAX {
        let head: String = safe.chars().take(MAX - 1).collect();
        format!("{head}…")
    } else {
        safe
    }
}

fn project_short(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.rsplit('/').take(2).collect();
    if parts.is_empty() {
        return p.to_string();
    }
    let mut out: Vec<&str> = parts.into_iter().collect();
    out.reverse();
    out.join("/")
}

fn derive_one(
    harness: Harness,
    project: Option<&str>,
    session: &str,
) -> Result<crate::cmd_import::DerivedDoc> {
    match harness {
        Harness::Claude => {
            crate::cmd_import::derive_claude_pair(project.expect("project_keyed"), session)
        }
        Harness::Gemini => crate::cmd_import::derive_gemini_pair(
            project.expect("project_keyed"),
            session,
            false,
        ),
        Harness::Pi => {
            crate::cmd_import::derive_pi_pair(project.expect("project_keyed"), session, None)
        }
        Harness::Codex => crate::cmd_import::derive_codex_one(session),
        Harness::Opencode => crate::cmd_import::derive_opencode_one(session, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_name_and_symbol_are_distinct() {
        let all = [
            Harness::Claude,
            Harness::Gemini,
            Harness::Codex,
            Harness::Opencode,
            Harness::Pi,
        ];
        let names: Vec<&str> = all.iter().map(|h| h.name()).collect();
        let symbols: Vec<&str> = all.iter().map(|h| h.symbol()).collect();
        assert_eq!(names.len(), 5);
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            5,
            "names must be unique"
        );
        assert_eq!(
            symbols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5,
            "symbols must be unique"
        );
    }

    #[test]
    fn harness_project_keyed_matches_design() {
        assert!(Harness::Claude.project_keyed());
        assert!(Harness::Gemini.project_keyed());
        assert!(Harness::Pi.project_keyed());
        assert!(!Harness::Codex.project_keyed());
        assert!(!Harness::Opencode.project_keyed());
    }

    #[test]
    fn harness_from_arg_roundtrips() {
        for (arg, harness) in [
            (HarnessArg::Claude, Harness::Claude),
            (HarnessArg::Gemini, Harness::Gemini),
            (HarnessArg::Codex, Harness::Codex),
            (HarnessArg::Opencode, Harness::Opencode),
            (HarnessArg::Pi, Harness::Pi),
        ] {
            assert_eq!(Harness::from_arg(arg), harness);
        }
    }

    use std::path::Path;
    use tempfile::TempDir;

    fn write_claude_session(claude_dir: &Path, project_slug: &str, session: &str, prompt: &str) {
        let project_dir = claude_dir.join("projects").join(project_slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let user = format!(
            r#"{{"type":"user","uuid":"u-{session}","timestamp":"2024-01-02T00:00:00Z","cwd":"/test/project","message":{{"role":"user","content":"{prompt}"}}}}"#
        );
        let asst = format!(
            r#"{{"type":"assistant","uuid":"a-{session}","timestamp":"2024-01-02T00:00:01Z","message":{{"role":"assistant","content":"hi"}}}}"#
        );
        std::fs::write(
            project_dir.join(format!("{session}.jsonl")),
            format!("{user}\n{asst}\n"),
        )
        .unwrap();
    }

    fn claude_only_bundle(home: &Path) -> HarnessBundle {
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    #[test]
    fn gather_sessions_includes_claude_rows_for_a_project() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_sessions(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, Harness::Claude);
        assert_eq!(rows[0].session_id, "abc-session-one");
        assert_eq!(rows[0].project.as_deref(), Some("/test/project"));
        assert!(rows[0].matches_cwd, "cwd should match the project path");
    }

    #[test]
    fn gather_sessions_marks_non_matching_project_rows() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/some/other/place");
        let rows = gather_sessions(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_skips_harness_with_no_home_dir() {
        // Empty bundle => no rows, no panic.
        let bundle = HarnessBundle::default();
        let rows = gather_sessions(&bundle, Path::new("/anywhere"), None, None);
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_sessions_filters_by_harness() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "hi",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_sessions(&bundle, cwd, Some(Harness::Codex), None);
        assert!(rows.is_empty(), "filter to codex must drop claude rows");
    }

    fn codex_only_bundle(home: &Path) -> HarnessBundle {
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let resolver = toolpath_codex::PathResolver::new().with_codex_dir(&codex_dir);
        HarnessBundle {
            codex: Some(toolpath_codex::CodexConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn write_codex_session(codex_dir: &Path, id: &str, cwd: &str) {
        // Date-bucketed layout: ~/.codex/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl
        let dir = codex_dir.join("sessions/2026/05/07");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("rollout-2026-05-07T00-00-00-{id}.jsonl"));
        let meta = format!(
            r#"{{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-05-07T00:00:00Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"test","source":"cli","model_provider":"openai"}}}}"#
        );
        let user = r#"{"timestamp":"2026-05-07T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
        std::fs::write(file, format!("{meta}\n{user}\n")).unwrap();
    }

    #[test]
    fn gather_sessions_includes_codex_rows_with_cwd_match() {
        let temp = TempDir::new().unwrap();
        write_codex_session(
            &temp.path().join(".codex"),
            "00000000-0000-0000-0000-0000000000aa",
            "/work/proj",
        );
        let bundle = codex_only_bundle(temp.path());
        let rows = gather_sessions(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].harness, Harness::Codex);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_ranks_cwd_matches_first() {
        // Two claude sessions: one in cwd (older), one elsewhere (newer).
        // Despite the elsewhere row being newer, the cwd-match must come first.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        write_claude_session(&claude_dir, "-cwd-project", "in-cwd-session", "hi");
        // Bump activity on the not-in-cwd session by writing a later timestamp.
        let not_dir = claude_dir.join("projects").join("-other-project");
        std::fs::create_dir_all(&not_dir).unwrap();
        std::fs::write(
            not_dir.join("not-in-cwd-session.jsonl"),
            r#"{"type":"user","uuid":"u-x","timestamp":"2030-01-01T00:00:00Z","cwd":"/other/project","message":{"role":"user","content":"later"}}"#.to_string()
                + "\n",
        )
        .unwrap();
        let bundle = claude_only_bundle(temp.path());
        let rows = gather_sessions(&bundle, Path::new("/cwd/project"), None, None);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "in-cwd-session");
        assert!(rows[0].matches_cwd);
        assert!(!rows[1].matches_cwd);
    }

    #[test]
    fn parse_picker_row_roundtrips_keyed() {
        let row = SessionRow {
            harness: Harness::Claude,
            project: Some("/tmp/proj".to_string()),
            cwd: None,
            session_id: "sess-abc".to_string(),
            title: "Hello\tworld".to_string(),
            last_activity: None,
            message_count: 3,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (harness, key, session) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, Harness::Claude);
        assert_eq!(key, "/tmp/proj");
        assert_eq!(session, "sess-abc");
    }

    #[test]
    fn parse_picker_row_roundtrips_session_keyed() {
        let row = SessionRow {
            harness: Harness::Codex,
            project: None,
            cwd: Some("/work/proj".to_string()),
            session_id: "0190abcd".to_string(),
            title: "(no prompt)".to_string(),
            last_activity: None,
            message_count: 0,
            matches_cwd: false,
        };
        let line = format_picker_row(&row);
        let (harness, key, session) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, Harness::Codex);
        assert_eq!(key, "/work/proj"); // codex has no project; cwd carried as the keyed slot
        assert_eq!(session, "0190abcd");
    }
}
