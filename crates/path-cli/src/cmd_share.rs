//! `path share` — interactive Pathbase upload across installed agent
//! harnesses. See `docs/superpowers/specs/2026-05-07-path-share-command-design.md`.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use std::path::PathBuf;

use crate::artifact::ArtifactType;
use crate::config::Config;
use crate::harness::{
    Harness, HarnessBundle, is_not_found_claude, is_not_found_codex, is_not_found_copilot,
    is_not_found_cursor, is_not_found_gemini, is_not_found_opencode, is_not_found_pi,
};
use crate::providers;
use crate::remote::RepoSpec;

#[derive(Args, Debug)]
pub struct ShareArgs {
    /// Pathbase server URL (defaults to the stored session's server)
    #[arg(long)]
    pub url: Option<String>,

    /// Force the anonymous endpoint, ignoring any stored credentials
    #[arg(long, conflicts_with_all = ["repo", "public"])]
    pub anon: bool,

    /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
    #[arg(long, value_parser = crate::remote::parse_repo_spec)]
    pub repo: Option<RepoSpec>,

    /// Human-readable display label for the uploaded graph
    /// (defaults to the toolpath document id). Free-form; not used
    /// in the URL — graphs are addressed by UUID server-side.
    #[arg(long, alias = "slug")]
    pub name: Option<String>,

    /// Mark the uploaded graph public (default: unlisted, addressable only by UUID)
    #[arg(long)]
    pub public: bool,

    /// Narrow the picker to one harness, or skip the picker entirely
    /// when used with --session.
    #[arg(long, value_enum)]
    pub harness: Option<Harness>,

    /// Skip the picker. Requires --harness; requires --project for
    /// claude/gemini/pi.
    #[arg(long, requires = "harness")]
    pub session: Option<String>,

    /// Override cwd-as-project. Filters the picker to sessions tied to
    /// this project across all harnesses.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Skip writing the cache; derive in-memory only
    #[arg(long)]
    pub no_cache: bool,
}

/// One artifact surfaced by a provider — today always an agent session.
/// Rows feed both the unified `share` picker and `p cache sync`.
#[derive(Debug, Clone)]
pub(crate) struct ArtifactRow {
    pub(crate) artifact_type: ArtifactType,
    /// Project path for keyed providers; `None` for codex/opencode.
    pub(crate) path: Option<String>,
    /// Recorded cwd from the session (codex/opencode only).
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) last_activity: Option<DateTime<Utc>>,
    /// Message count — populated only for harness artifact types
    /// (agent sessions); `None` for future non-session artifact kinds.
    pub(crate) message_count: Option<usize>,
    pub(crate) matches_cwd: bool,
}

/// Aggregate sessions across the harnesses in `bundle`, ranked so that
/// rows whose project (or recorded cwd) canonicalizes to `cwd` come
/// first, sorted by descending `last_activity`.
///
/// Filters: `harness_filter` keeps only rows from one harness; `project_filter`
/// keeps only rows whose project (for keyed) or cwd (for session-keyed)
/// canonicalizes to that path.
pub(crate) fn gather_artifacts(
    bundle: &HarnessBundle,
    cwd: &std::path::Path,
    harness_filter: Option<ArtifactType>,
    project_filter: Option<&std::path::Path>,
) -> Vec<ArtifactRow> {
    let mut rows = Vec::new();
    let canonical_cwd = canonicalize_or_self(cwd);
    let canonical_project = project_filter.map(canonicalize_or_self);

    let want = |h: ArtifactType| harness_filter.is_none_or(|f| f == h);

    if want(ArtifactType::Claude)
        && let Some(mgr) = &bundle.claude
    {
        collect_claude(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Gemini)
        && let Some(mgr) = &bundle.gemini
    {
        collect_gemini(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Pi)
        && let Some(mgr) = &bundle.pi
    {
        collect_pi(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Codex)
        && let Some(mgr) = &bundle.codex
    {
        collect_codex(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Copilot)
        && let Some(mgr) = &bundle.copilot
    {
        collect_copilot(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Opencode)
        && let Some(mgr) = &bundle.opencode
    {
        collect_opencode(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
    }
    if want(ArtifactType::Cursor)
        && let Some(mgr) = &bundle.cursor
    {
        collect_cursor(mgr, &canonical_cwd, canonical_project.as_deref(), &mut rows);
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
    out: &mut Vec<ArtifactRow>,
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
            out.push(ArtifactRow {
                artifact_type: ArtifactType::Claude,
                path: Some(m.project_path),
                cwd: None,
                session_id: m.session_id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: Some(m.message_count),
                matches_cwd,
            });
        }
    }
}

fn collect_gemini(
    mgr: &toolpath_gemini::GeminiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
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
            out.push(ArtifactRow {
                artifact_type: ArtifactType::Gemini,
                path: Some(m.project_path),
                cwd: None,
                session_id: m.session_uuid,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity: m.last_activity,
                message_count: Some(m.message_count),
                matches_cwd,
            });
        }
    }
}

fn collect_pi(
    mgr: &toolpath_pi::PiConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
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
            // Pi's SessionMeta.timestamp is the session *start*, so it
            // never moves as the session grows; prefer the file's mtime
            // as the change-detecting last_activity, falling back to
            // the header timestamp when the stat fails.
            let last_activity = std::fs::metadata(&m.file_path)
                .and_then(|md| md.modified())
                .ok()
                .map(DateTime::<Utc>::from)
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                        .ok()
                        .map(|d| d.with_timezone(&Utc))
                });
            out.push(ArtifactRow {
                artifact_type: ArtifactType::Pi,
                path: Some(project.clone()),
                cwd: None,
                session_id: m.id,
                title: m
                    .first_user_message
                    .unwrap_or_else(|| "(no prompt)".to_string()),
                last_activity,
                message_count: Some(m.entry_count),
                matches_cwd,
            });
        }
    }
}

fn collect_codex(
    mgr: &toolpath_codex::CodexConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
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
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Codex,
            path: None,
            cwd: cwd_str,
            session_id: m.id,
            title: m
                .first_user_message
                .unwrap_or_else(|| "(no prompt)".to_string()),
            last_activity: m.last_activity,
            message_count: Some(m.line_count),
            matches_cwd,
        });
    }
}

fn collect_copilot(
    mgr: &toolpath_copilot::CopilotConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let metas = match mgr.list_sessions() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_copilot(&e) => return,
        Err(e) => {
            eprintln!("warning: copilot aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        // Copilot stores cwd as a String (from session.start `context.cwd`).
        let stored = m.cwd.as_deref().map(std::path::PathBuf::from);
        if let Some(filter) = project_filter {
            match &stored {
                Some(p) if paths_match(p, filter) => {}
                _ => continue,
            }
        }
        let matches_cwd = stored
            .as_deref()
            .map(|p| paths_match(p, canonical_cwd))
            .unwrap_or(false);
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Copilot,
            path: None,
            cwd: m.cwd,
            session_id: m.id,
            title: m
                .first_user_message
                .unwrap_or_else(|| "(no prompt)".to_string()),
            last_activity: m.last_activity,
            message_count: Some(m.line_count),
            matches_cwd,
        });
    }
}

fn collect_opencode(
    mgr: &toolpath_opencode::OpencodeConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
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
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Opencode,
            path: None,
            cwd: Some(cwd_str),
            session_id: m.id,
            title,
            last_activity: m.last_activity,
            message_count: Some(m.message_count),
            matches_cwd,
        });
    }
}

fn collect_cursor(
    mgr: &toolpath_cursor::CursorConvo,
    canonical_cwd: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut Vec<ArtifactRow>,
) {
    let metas = match mgr.io().list_session_metadata() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return,
        Err(e) if is_not_found_cursor(&e) => return,
        Err(e) => {
            eprintln!("warning: cursor aggregation failed: {e}");
            return;
        }
    };
    for m in metas {
        // Cursor stores each composer's workspace as the absolute
        // path of the folder Cursor.app was open on. Sessions
        // without a workspace (numeric/remote workspace ids) are
        // dropped from the picker — we can't tell what they're
        // tied to.
        let Some(workspace) = m.workspace_path.as_ref() else {
            continue;
        };
        if let Some(filter) = project_filter
            && !paths_match(workspace, filter)
        {
            continue;
        }
        let matches_cwd = paths_match(workspace, canonical_cwd);
        let cwd_str = workspace.to_string_lossy().into_owned();
        let title = match (&m.first_user_message, &m.name) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, Some(n)) if !n.is_empty() => n.clone(),
            _ => "(no prompt)".to_string(),
        };
        out.push(ArtifactRow {
            artifact_type: ArtifactType::Cursor,
            path: None,
            cwd: Some(cwd_str),
            session_id: m.id,
            title,
            last_activity: m.last_activity,
            message_count: Some(m.message_count),
            matches_cwd,
        });
    }
}

pub fn run(args: ShareArgs, config: &Config) -> Result<()> {
    let harness = args.harness.map(|h| h.artifact_type());

    if args.session.is_some() && harness.is_none() {
        anyhow::bail!("--session requires --harness");
    }

    // Build upload args + base URL once and reuse for both the explicit
    // path and the picker path. `needs_auth` decides whether preflight
    // can fall back to anon on credential failure.
    let upload_args = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
    };
    let base_url = crate::cmd_export::resolve_upload_base_url(config, &upload_args);
    let needs_auth = upload_args.repo.is_some() || upload_args.public || upload_args.name.is_some();

    if let (Some(h), Some(session)) = (harness, &args.session) {
        // Explicit-args: validate creds before derive so a credential
        // failure doesn't waste the derive/cache work.
        let auth =
            crate::cmd_pathbase::preflight_auth(config, &base_url, upload_args.anon, needs_auth)?;
        return share_explicit(h, session.as_str(), &args, auth, base_url, config);
    }

    let cwd = std::env::current_dir()?;
    let bundle = providers::harness_bundle(config);
    let project_filter = args.project.as_deref();
    let rows = gather_artifacts(&bundle, &cwd, harness, project_filter);

    if rows.is_empty() {
        return bail_no_sessions(&bundle, project_filter, config);
    }

    if !crate::fuzzy::available() {
        eprintln!(
            "Interactive `path share` needs `fzf` on PATH and a TTY.\n\
             \n\
             Manual recipe:\n  \
             path import <harness>      # writes a cache entry, prints its id\n  \
             path export pathbase --input <id>"
        );
        anyhow::bail!("fzf unavailable; run `path import <harness>` then `path export pathbase`");
    }

    // We have rows AND fzf available — now validate credentials before
    // making the user pick a session. If preflight returns Anon (either
    // explicit --anon, no creds + no auth flags, or auth probe failed
    // and fell back), the picker still fires with that knowledge baked in.
    let auth =
        crate::cmd_pathbase::preflight_auth(config, &base_url, upload_args.anon, needs_auth)?;

    let lines: Vec<String> = rows.iter().map(format_picker_row).collect();
    let header = format!("share an agent session (Enter = upload to {base_url})");
    let opts = crate::fuzzy::PickOptions {
        with_nth: "4",
        prompt: "share> ",
        preview: Some("{exe} show --ansi {1} --project {2} --session {3}"),
        // Stacked layout: preview above the list, list below. Fits narrow
        // terminals better than the default side-by-side and gives the
        // session preview the full terminal width to render `path show`.
        preview_window: "up:60%:wrap-word",
        header: Some(&header),
        tiebreak: "index",
        multi: false,
    };
    let line = match crate::fuzzy::pick(&lines, &opts)? {
        crate::fuzzy::PickResult::Selected(v) => match v.into_iter().next() {
            Some(l) => l,
            // Selected with an empty payload should not happen (fzf exits 0
            // only when at least one row was confirmed), but treat it like
            // no-match for safety.
            None => return Ok(()),
        },
        // No row matched the query — exit 0, same as today, no extra noise.
        crate::fuzzy::PickResult::NoMatch => return Ok(()),
        // Esc / Ctrl-C: deliberate user cancel. Signal to the shell with
        // exit 130 so it's distinguishable from a successful share.
        crate::fuzzy::PickResult::Cancelled => std::process::exit(130),
    };
    let (h, key, session, title) = parse_picker_row(&line)
        .ok_or_else(|| anyhow::anyhow!("internal: failed to parse picker row"))?;

    let explicit = ShareArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: args.repo.clone(),
        name: args.name.clone(),
        public: args.public,
        harness: h.harness(),
        session: None, // unused by share_explicit
        project: if h.path_keyed() {
            Some(PathBuf::from(&key))
        } else {
            None
        },
        no_cache: args.no_cache,
    };
    // Show the conversation title in the confirmation line; the session id
    // is opaque and doesn't help the user verify they picked the right
    // thing. `{:?}` adds the surrounding quotes per the spec.
    eprintln!("Picked {} session {:?}", h.name(), title);
    share_explicit(h, &session, &explicit, auth, base_url, config)
}

fn bail_no_sessions(
    bundle: &HarnessBundle,
    project_filter: Option<&std::path::Path>,
    config: &Config,
) -> Result<()> {
    if let Some(p) = project_filter {
        anyhow::bail!(
            "No agent sessions found in project {}. Run without --project to see sessions across all projects.",
            p.display()
        );
    }

    let mut summary = String::from("No agent sessions found.\n");
    // Pad harness names so the path column lines up: "opencode:" is the
    // longest at 9 chars (8 + colon).
    let home = config.home_dir().map(std::path::PathBuf::as_path);
    summary.push_str(&format_status_line(
        "claude",
        &harness_status_claude(bundle, home),
    ));
    summary.push_str(&format_status_line(
        "gemini",
        &harness_status_gemini(bundle, home),
    ));
    summary.push_str(&format_status_line(
        "codex",
        &harness_status_codex(bundle, home),
    ));
    summary.push_str(&format_status_line(
        "copilot",
        &harness_status_copilot(bundle, home),
    ));
    summary.push_str(&format_status_line(
        "opencode",
        &harness_status_opencode(bundle, home),
    ));
    summary.push_str(&format_status_line(
        "cursor",
        &harness_status_cursor(bundle, home),
    ));
    summary.push_str(&format_status_line("pi", &harness_status_pi(bundle, home)));
    eprint!("{summary}");
    anyhow::bail!("no shareable sessions");
}

/// Human-readable status of a harness's on-disk store: either the (possibly
/// home-relative) path with a "(0 sessions)" hint, or the path with a
/// "not found" hint when the directory/database is absent.
#[derive(Debug, PartialEq, Eq)]
struct HarnessStatus {
    /// Display path (tilde-prefixed when under `$HOME`).
    path: String,
    /// True when the path exists on disk.
    exists: bool,
}

impl HarnessStatus {
    fn render(&self) -> String {
        if self.exists {
            format!("{} (0 sessions)", self.path)
        } else {
            format!("{} not found", self.path)
        }
    }

    /// Status when the resolver itself failed (e.g. no $HOME).
    fn unresolved() -> Self {
        Self {
            path: "<no home directory>".to_string(),
            exists: false,
        }
    }
}

/// Format a single status line, padding the harness name so that the path
/// column lines up across all five rows. The longest name is "opencode" (8).
fn format_status_line(name: &str, status: &HarnessStatus) -> String {
    format!("  {:<9} {}\n", format!("{name}:"), status.render())
}

fn harness_status_claude(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.claude else {
        return HarnessStatus::unresolved();
    };
    let p = mgr.resolver().projects_dir();
    HarnessStatus {
        path: crate::config::home_relative(&p, home),
        exists: p.exists(),
    }
}

fn harness_status_gemini(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.gemini else {
        return HarnessStatus::unresolved();
    };
    let p = mgr.resolver().tmp_dir();
    HarnessStatus {
        path: crate::config::home_relative(&p, home),
        exists: p.exists(),
    }
}

fn harness_status_codex(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.codex else {
        return HarnessStatus::unresolved();
    };
    let p = mgr.resolver().sessions_root();
    HarnessStatus {
        path: crate::config::home_relative(&p, home),
        exists: p.exists(),
    }
}

fn harness_status_copilot(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.copilot else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().session_state_dir() {
        Ok(p) => HarnessStatus {
            path: crate::config::home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_opencode(
    bundle: &HarnessBundle,
    home: Option<&std::path::Path>,
) -> HarnessStatus {
    let Some(mgr) = &bundle.opencode else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().db_path() {
        Ok(p) => HarnessStatus {
            path: crate::config::home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn harness_status_pi(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.pi else {
        return HarnessStatus::unresolved();
    };
    let p = mgr.resolver().sessions_dir().to_path_buf();
    HarnessStatus {
        path: crate::config::home_relative(&p, home),
        exists: p.exists(),
    }
}

fn harness_status_cursor(bundle: &HarnessBundle, home: Option<&std::path::Path>) -> HarnessStatus {
    let Some(mgr) = &bundle.cursor else {
        return HarnessStatus::unresolved();
    };
    match mgr.resolver().db_path() {
        Ok(p) => HarnessStatus {
            path: crate::config::home_relative(&p, home),
            exists: p.exists(),
        },
        Err(_) => HarnessStatus::unresolved(),
    }
}

fn share_explicit(
    harness: ArtifactType,
    session: &str,
    args: &ShareArgs,
    auth: crate::cmd_pathbase::AuthMode,
    base_url: String,
    config: &Config,
) -> Result<()> {
    let project = match (harness.path_keyed(), args.project.as_ref()) {
        (true, Some(p)) => Some(p.to_string_lossy().into_owned()),
        (true, None) => anyhow::bail!(
            "--project required when --harness is {} and --session is set",
            harness.name()
        ),
        (false, _) => None,
    };

    // Fast path: when the manifest shows this exact source state is
    // already in the cache, upload the cached doc instead of re-deriving
    // — a derive would reproduce it byte-for-byte anyway.
    if !args.no_cache
        && let Some(cache_id) = crate::sync::fresh_cache_id(
            config,
            &providers::harness_bundle(config),
            harness,
            project.as_deref(),
            session,
        )
    {
        let doc_path = crate::cache::cache_path(config, &cache_id)?;
        let body = std::fs::read_to_string(&doc_path)
            .with_context(|| format!("Failed to read {}", doc_path.display()))?;
        eprintln!(
            "Cache is current for {} session {cache_id}; uploading without re-deriving",
            harness.name()
        );
        let session_dir = project.as_deref().map(PathBuf::from).or_else(|| {
            toolpath::v1::Graph::from_json(&body)
                .ok()
                .and_then(|doc| doc_session_dir(&doc))
        });
        let dest = resolve_destination(args, &auth, base_url, session_dir, config)?;
        let summary = format!("{} session {}", harness.name(), cache_id);
        let upload = crate::cmd_export::PathbaseUploadArgs {
            url: args.url.clone(),
            anon: args.anon,
            repo: dest.repo,
            name: args.name.clone(),
            public: args.public,
        };
        return crate::cmd_export::run_pathbase_inner(auth, dest.base_url, upload, &body, &summary);
    }

    let derived = derive_session(harness, project.as_deref(), session, config)?;
    let summary = format!("{} session {}", harness.name(), derived.cache_id);

    if !args.no_cache {
        // The cache entry should always reflect what was just uploaded.
        // `path share` is "ship the current state of this session"; if
        // the conversation has grown since a prior share, the in-memory
        // body has the new turns but a stale cache file would not — and
        // the upload uses the fresh body, not the cache. Always
        // overwrite so cache and upload agree (use `--no-cache` to skip
        // the cache write entirely).
        let path = crate::cache::write_cached(config, &derived.cache_id, &derived.doc, true)?;
        if let Some(stub) = &derived.provenance
            && let Err(e) = crate::sync::record_artifact(config, stub, &derived.cache_id)
        {
            eprintln!("warning: sync manifest not updated: {e}");
        }
        eprintln!(
            "Cached {} session → {} ({})",
            harness.name(),
            derived.cache_id,
            path.display()
        );
    }

    let session_dir = project
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| doc_session_dir(&derived.doc));
    let dest = resolve_destination(args, &auth, base_url, session_dir, config)?;
    let body = derived.doc.to_json()?;
    let upload = crate::cmd_export::PathbaseUploadArgs {
        url: args.url.clone(),
        anon: args.anon,
        repo: dest.repo,
        name: args.name.clone(),
        public: args.public,
    };
    crate::cmd_export::run_pathbase_inner(auth, dest.base_url, upload, &body, &summary)
}

/// The directory a derived session document belongs to: its single
/// path's `base.uri` when that's a `file://` URI (conversation derives
/// record the session's cwd there). This is how session-keyed harnesses
/// (codex/opencode/copilot/cursor), which carry no `--project`, feed the
/// configured-repo lookup.
fn doc_session_dir(doc: &toolpath::v1::Graph) -> Option<PathBuf> {
    let base = doc.single_path()?.path.base.as_ref()?;
    let dir = base.uri.strip_prefix("file://")?;
    if dir.is_empty() {
        return None;
    }
    Some(PathBuf::from(dir))
}

/// Where an upload goes: the repo (`None` = the pathstash default) and
/// the server it lives on.
#[derive(Debug)]
struct ShareDestination {
    repo: Option<RepoSpec>,
    base_url: String,
}

/// Apply the configured remote to the upload destination. `--repo` wins;
/// explicit `--anon` skips config entirely (anonymous uploads have no
/// repo); otherwise a remote configured for the session's directory
/// applies (see `share_config`). A URL-form remote also carries the
/// server, which replaces `base_url` unless `--url` was given — flags
/// win. A configured remote needs an authed upload, so hitting one while
/// unauthenticated is an error rather than a silent fall-through to the
/// anonymous endpoint; a cross-server remote rides the stored token and
/// lets the upload's own 401 handling surface a re-login hint.
fn resolve_destination(
    args: &ShareArgs,
    auth: &crate::cmd_pathbase::AuthMode,
    base_url: String,
    session_dir: Option<PathBuf>,
    config: &Config,
) -> Result<ShareDestination> {
    if args.repo.is_some() || args.anon {
        return Ok(ShareDestination {
            repo: args.repo.clone(),
            base_url,
        });
    }
    let Some(dir) = session_dir else {
        return Ok(ShareDestination {
            repo: None,
            base_url,
        });
    };
    let Some(found) = crate::share_config::resolve_remote(config, &dir)? else {
        return Ok(ShareDestination {
            repo: None,
            base_url,
        });
    };
    if matches!(auth, crate::cmd_pathbase::AuthMode::Anon) {
        let login_url = found
            .base_url
            .as_ref()
            .map(|u| format!(" --url {u}"))
            .unwrap_or_default();
        anyhow::bail!(
            "sessions in {} are configured to upload to {} ({}), which requires login.\n\
             Run `path auth login{login_url}`, or pass --anon to upload anonymously instead.",
            dir.display(),
            found.display,
            found.origin,
        );
    }
    let base_url = match (&args.url, found.base_url) {
        (None, Some(remote_url)) => remote_url,
        _ => base_url,
    };
    eprintln!("Sharing to {} ({})", found.display, found.origin);
    Ok(ShareDestination {
        repo: Some(found.repo),
        base_url,
    })
}

/// Build the TSV line fed to the picker. Three hidden parser-only
/// columns lead the row (harness key, project/cwd, session id); a
/// fourth column carries the pre-formatted display string from
/// `fuzzy::render_row`; a fifth carries the raw title so
/// `parse_picker_row` can recover it without reparsing the display.
///
/// The display column is space-padded rather than tab-separated so the
/// columns line up consistently across pickers — terminal tab stops
/// produce ugly variable gaps in both fzf and skim.
fn format_picker_row(row: &ArtifactRow) -> String {
    let key = row
        .path
        .clone()
        .or_else(|| row.cwd.clone())
        .unwrap_or_default();
    let scope = if row.matches_cwd { "·" } else { " " };
    let leading = format!("{scope} {}", row.artifact_type.padded_name());
    let display = render_row(
        Some(&leading),
        row.last_activity,
        &row.message_count
            .map(|c| count(c, "msgs"))
            .unwrap_or_default(),
        Some(&project_short(&key)),
        &row.title,
    );
    let title = clean_for_picker_display(&row.title);
    format!(
        "{}\t{}\t{}\t{}\t{}",
        row.artifact_type.name(),
        tab_safe(&key),
        tab_safe(&row.session_id),
        display,
        tab_safe(&title),
    )
}

/// Inverse of [`format_picker_row`] — pulls (harness, key, session,
/// title) back out of the line the picker returned. Returns `None` if
/// the line is malformed.
fn parse_picker_row(line: &str) -> Option<(ArtifactType, String, String, String)> {
    let mut parts = line.split('\t');
    let h = ArtifactType::parse(parts.next()?)?;
    let key = parts.next()?.to_string();
    let session = parts.next()?.to_string();
    if session.is_empty() {
        return None;
    }
    // Skip the pre-formatted display column (col 4) to reach the raw
    // title at col 5.
    let title = parts.nth(1).unwrap_or("").to_string();
    Some((h, key, session, title))
}

use crate::fuzzy::{clean_for_picker_display, count, project_short, render_row, tab_safe};

fn derive_session(
    harness: ArtifactType,
    project: Option<&str>,
    session: &str,
    config: &Config,
) -> Result<crate::derive::DerivedDoc> {
    match harness {
        ArtifactType::Claude => {
            crate::derive::derive_claude_session(config, project.expect("path_keyed"), session)
        }
        ArtifactType::Gemini => {
            crate::derive::derive_gemini_session(config, project.expect("path_keyed"), session)
        }
        ArtifactType::Copilot => crate::derive::derive_copilot_session(config, session),
        ArtifactType::Pi => {
            crate::derive::derive_pi_session(config, project.expect("path_keyed"), session, None)
        }
        ArtifactType::Codex => crate::derive::derive_codex_session(config, session),
        ArtifactType::Opencode => crate::derive::derive_opencode_session(config, session, false),
        ArtifactType::Cursor => crate::derive::derive_cursor_session(config, session),
        ArtifactType::Git => {
            anyhow::bail!("share only handles agent sessions; git artifacts go through `p import`")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let resolver = toolpath_claude::PathResolver::new(home).with_claude_dir(&claude_dir);
        HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    #[test]
    fn gather_artifacts_includes_claude_rows_for_a_project() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_artifacts(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Claude);
        assert_eq!(rows[0].session_id, "abc-session-one");
        assert_eq!(rows[0].path.as_deref(), Some("/test/project"));
        assert!(rows[0].matches_cwd, "cwd should match the project path");
    }

    #[test]
    fn gather_artifacts_marks_non_matching_project_rows() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "Add a feature",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/some/other/place");
        let rows = gather_artifacts(&bundle, cwd, None, None);

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].matches_cwd);
    }

    #[test]
    fn gather_artifacts_skips_harness_with_no_home_dir() {
        // Empty bundle => no rows, no panic.
        let bundle = HarnessBundle::default();
        let rows = gather_artifacts(&bundle, Path::new("/anywhere"), None, None);
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_artifacts_filters_by_harness() {
        let temp = TempDir::new().unwrap();
        write_claude_session(
            &temp.path().join(".claude"),
            "-test-project",
            "abc-session-one",
            "hi",
        );
        let bundle = claude_only_bundle(temp.path());
        let cwd = Path::new("/test/project");
        let rows = gather_artifacts(&bundle, cwd, Some(ArtifactType::Codex), None);
        assert!(rows.is_empty(), "filter to codex must drop claude rows");
    }

    fn codex_only_bundle(home: &Path) -> HarnessBundle {
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let resolver = toolpath_codex::PathResolver::new(home).with_codex_dir(&codex_dir);
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
    fn gather_artifacts_includes_codex_rows_with_cwd_match() {
        let temp = TempDir::new().unwrap();
        write_codex_session(
            &temp.path().join(".codex"),
            "00000000-0000-0000-0000-0000000000aa",
            "/work/proj",
        );
        let bundle = codex_only_bundle(temp.path());
        let rows = gather_artifacts(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Codex);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    fn copilot_only_bundle(home: &Path) -> HarnessBundle {
        let copilot_dir = home.join(".copilot");
        std::fs::create_dir_all(&copilot_dir).unwrap();
        let resolver = toolpath_copilot::PathResolver::new().with_copilot_dir(&copilot_dir);
        HarnessBundle {
            copilot: Some(toolpath_copilot::CopilotConvo::with_resolver(resolver)),
            ..Default::default()
        }
    }

    fn write_copilot_session(copilot_dir: &Path, id: &str, cwd: &str) {
        // ~/.copilot/session-state/<id>/events.jsonl (cwd under session.start.context)
        let dir = copilot_dir.join("session-state").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let start = format!(
            r#"{{"type":"session.start","timestamp":"2026-07-01T00:00:00Z","data":{{"copilotVersion":"1.0.67","context":{{"cwd":"{cwd}"}}}}}}"#
        );
        let user =
            r#"{"type":"user.message","timestamp":"2026-07-01T00:00:01Z","data":{"content":"hi"}}"#;
        std::fs::write(dir.join("events.jsonl"), format!("{start}\n{user}\n")).unwrap();
    }

    #[test]
    fn gather_sessions_includes_copilot_rows_with_cwd_match() {
        let temp = TempDir::new().unwrap();
        write_copilot_session(&temp.path().join(".copilot"), "sess-aa", "/work/proj");
        let bundle = copilot_only_bundle(temp.path());
        let rows = gather_artifacts(&bundle, Path::new("/work/proj"), None, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_type, ArtifactType::Copilot);
        assert_eq!(rows[0].cwd.as_deref(), Some("/work/proj"));
        assert!(rows[0].matches_cwd);
    }

    #[test]
    fn gather_sessions_filters_to_copilot() {
        let temp = TempDir::new().unwrap();
        write_copilot_session(&temp.path().join(".copilot"), "sess-aa", "/work/proj");
        let bundle = copilot_only_bundle(temp.path());
        // Filtering to a different harness drops the copilot row.
        let rows = gather_artifacts(
            &bundle,
            Path::new("/work/proj"),
            Some(ArtifactType::Codex),
            None,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn gather_artifacts_ranks_cwd_matches_first() {
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
        let rows = gather_artifacts(&bundle, Path::new("/cwd/project"), None, None);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "in-cwd-session");
        assert!(rows[0].matches_cwd);
        assert!(!rows[1].matches_cwd);
    }

    #[test]
    #[cfg(unix)]
    fn paths_match_canonicalizes_through_symlink() {
        // `paths_match` is the function that produces `ArtifactRow.matches_cwd`
        // (collect_* all delegate to it). Without canonicalization, a user who
        // navigated to a project via a symlink would see their cwd-row sink
        // in the picker because the symlink path string ≠ the project path
        // string. Verify both arguments are canonicalized.
        //
        // Note: we test `paths_match` directly rather than going through
        // `gather_artifacts` because Claude's project-dir slug encoding is
        // lossy (sanitize_project_path: '/', '_', '.' → '-'; unsanitize: only
        // '-' → '/'). On macOS, tempdir paths contain '.' and end up under
        // /private/var/..., so the unsanitized slug never round-trips back to
        // the real on-disk path. This direct test covers the canonicalization
        // bug regardless of platform-specific tempdir layouts.
        let temp = TempDir::new().unwrap();
        let real_project = temp.path().join("real-project");
        std::fs::create_dir_all(&real_project).unwrap();
        let symlink_path = temp.path().join("symlink-to-project");
        std::os::unix::fs::symlink(&real_project, &symlink_path).unwrap();

        // Sanity-check the setup: the symlink and its target are different
        // string-paths but resolve to the same canonical path.
        assert_ne!(real_project, symlink_path);
        assert_eq!(
            std::fs::canonicalize(&real_project).unwrap(),
            std::fs::canonicalize(&symlink_path).unwrap(),
        );

        // The actual property under test.
        assert!(
            paths_match(&real_project, &symlink_path),
            "paths_match must canonicalize both sides so symlink == target"
        );
        // And symmetric.
        assert!(
            paths_match(&symlink_path, &real_project),
            "paths_match must be symmetric across the symlink"
        );
    }

    #[test]
    fn parse_picker_row_roundtrips_keyed() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Claude,
            path: Some("/tmp/proj".to_string()),
            cwd: None,
            session_id: "sess-abc".to_string(),
            title: "Hello\tworld".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (harness, key, session, title) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, ArtifactType::Claude);
        assert_eq!(key, "/tmp/proj");
        assert_eq!(session, "sess-abc");
        // tab_safe replaces the tab with a space, but the title content
        // otherwise round-trips.
        assert_eq!(title, "Hello world");
    }

    #[test]
    fn parse_picker_row_roundtrips_session_keyed() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Codex,
            path: None,
            cwd: Some("/work/proj".to_string()),
            session_id: "0190abcd".to_string(),
            title: "(no prompt)".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: false,
        };
        let line = format_picker_row(&row);
        let (harness, key, session, title) = parse_picker_row(&line).unwrap();
        assert_eq!(harness, ArtifactType::Codex);
        assert_eq!(key, "/work/proj"); // codex has no project; cwd carried as the keyed slot
        assert_eq!(session, "0190abcd");
        assert_eq!(title, "(no prompt)");
    }

    #[test]
    fn parse_picker_row_carries_title_with_unicode() {
        let row = ArtifactRow {
            artifact_type: ArtifactType::Gemini,
            path: Some("/work/proj".to_string()),
            cwd: None,
            session_id: "11111111-2222-3333-4444-555555555555".to_string(),
            title: "Add the share command — finally".to_string(),
            last_activity: None,
            message_count: None,
            matches_cwd: true,
        };
        let line = format_picker_row(&row);
        let (_, _, _, title) = parse_picker_row(&line).unwrap();
        assert_eq!(title, "Add the share command — finally");
    }

    #[test]
    fn harness_status_renders_existing_path_with_zero_sessions() {
        let s = HarnessStatus {
            path: "~/.claude/projects".to_string(),
            exists: true,
        };
        assert_eq!(s.render(), "~/.claude/projects (0 sessions)");
    }

    #[test]
    fn harness_status_renders_missing_path_as_not_found() {
        let s = HarnessStatus {
            path: "~/.gemini/tmp".to_string(),
            exists: false,
        };
        assert_eq!(s.render(), "~/.gemini/tmp not found");
    }

    #[test]
    fn format_status_line_pads_for_alignment() {
        let s = HarnessStatus {
            path: "~/.codex/sessions".to_string(),
            exists: true,
        };
        // "claude:" (7) needs 2 trailing spaces; "opencode:" (9) needs 0;
        // "pi:" (3) needs 6. The visible-path column should always start at
        // the same offset.
        let claude_line = format_status_line("claude", &s);
        let opencode_line = format_status_line("opencode", &s);
        let pi_line = format_status_line("pi", &s);
        let offset = |line: &str| line.find('~').unwrap();
        assert_eq!(offset(&claude_line), offset(&opencode_line));
        assert_eq!(offset(&claude_line), offset(&pi_line));
    }

    #[test]
    fn harness_status_for_missing_claude_dir_reports_not_found() {
        // Bundle whose claude resolver points at a directory that doesn't
        // exist on disk; the status should still resolve a path and report
        // it as missing rather than going through the `unresolved` branch.
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude"); // never created
        let resolver = toolpath_claude::PathResolver::new(temp.path()).with_claude_dir(&claude_dir);
        let bundle = HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        };
        let status = harness_status_claude(&bundle, None);
        assert!(!status.exists, "missing dir must report exists=false");
        assert!(
            status.path.contains("projects"),
            "path must include the projects subdir (got {:?})",
            status.path
        );
    }

    #[test]
    fn harness_status_for_present_claude_dir_reports_existence() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        std::fs::create_dir_all(claude_dir.join("projects")).unwrap();
        let resolver = toolpath_claude::PathResolver::new(temp.path()).with_claude_dir(&claude_dir);
        let bundle = HarnessBundle {
            claude: Some(toolpath_claude::ClaudeConvo::with_resolver(resolver)),
            ..Default::default()
        };
        let status = harness_status_claude(&bundle, None);
        assert!(status.exists);
    }

    fn share_args() -> ShareArgs {
        ShareArgs {
            url: None,
            anon: false,
            repo: None,
            name: None,
            public: false,
            harness: None,
            session: None,
            project: None,
            no_cache: false,
        }
    }

    fn graph_with_base(uri: &str) -> toolpath::v1::Graph {
        let json = format!(
            r#"{{"graph":{{"id":"g"}},"paths":[{{"path":{{"id":"p","head":"s1","base":{{"uri":{uri:?}}}}},"steps":[]}}]}}"#
        );
        toolpath::v1::Graph::from_json(&json).unwrap()
    }

    #[test]
    fn doc_session_dir_reads_file_uri_base() {
        let doc = graph_with_base("file:///work/proj");
        assert_eq!(doc_session_dir(&doc), Some(PathBuf::from("/work/proj")));
    }

    #[test]
    fn doc_session_dir_ignores_non_file_base() {
        let doc = graph_with_base("github:org/repo");
        assert_eq!(doc_session_dir(&doc), None);
        let doc = graph_with_base("file://");
        assert_eq!(doc_session_dir(&doc), None);
    }

    #[test]
    fn doc_session_dir_none_without_base() {
        let json = r#"{"graph":{"id":"g"},"paths":[{"path":{"id":"p","head":"s1"},"steps":[]}]}"#;
        let doc = toolpath::v1::Graph::from_json(json).unwrap();
        assert_eq!(doc_session_dir(&doc), None);
    }

    const DEFAULT_BASE: &str = "https://pathbase.dev";

    fn authed() -> crate::cmd_pathbase::AuthMode {
        crate::cmd_pathbase::AuthMode::Authed {
            token: "tok".into(),
            username: "me".into(),
        }
    }

    #[test]
    fn destination_flag_wins_without_touching_config() {
        let mut args = share_args();
        args.repo = Some(crate::remote::parse_repo_spec("me/flag").unwrap());
        let dest = resolve_destination(
            &args,
            &authed(),
            DEFAULT_BASE.to_string(),
            Some(PathBuf::from("/anywhere")),
            &Config::default(),
        )
        .unwrap();
        let repo = dest.repo.unwrap();
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("me", "flag"));
        assert_eq!(dest.base_url, DEFAULT_BASE);
    }

    #[test]
    fn destination_anon_flag_skips_config() {
        let mut args = share_args();
        args.anon = true;
        let dest = resolve_destination(
            &args,
            &crate::cmd_pathbase::AuthMode::Anon,
            DEFAULT_BASE.to_string(),
            Some(PathBuf::from("/anywhere")),
            &Config::default(),
        )
        .unwrap();
        assert!(dest.repo.is_none());
        assert_eq!(dest.base_url, DEFAULT_BASE);
    }

    #[test]
    fn destination_default_without_session_dir() {
        let dest = resolve_destination(
            &share_args(),
            &crate::cmd_pathbase::AuthMode::Anon,
            DEFAULT_BASE.to_string(),
            None,
            &Config::default(),
        )
        .unwrap();
        assert!(dest.repo.is_none());
    }

    /// One run covering the remote forms: bare `owner/name` (auth
    /// required when logged out, resolves when authed, base URL
    /// untouched) and full URL (base URL replaced, `--url` flag wins,
    /// logged-out hint names the remote's server).
    #[test]
    fn destination_applies_configured_remote() {
        let cfg = TempDir::new().unwrap();
        let bare = cfg.path().join("bare-proj");
        let url = cfg.path().join("url-proj");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&url).unwrap();
        std::fs::write(
            cfg.path().join("config.toml"),
            format!(
                "[[project]]\ndir = {bare:?}\nremote = \"team/sessions\"\n\n\
                 [[project]]\ndir = {url:?}\nremote = \"https://pathbase.internal/u/team/proj\"\n",
                bare = bare.display().to_string(),
                url = url.display().to_string(),
            ),
        )
        .unwrap();
        let config = Config {
            toolpath_config_dir: Some(cfg.path().to_path_buf()),
            ..Config::default()
        };
        let bare_unauthed = resolve_destination(
            &share_args(),
            &crate::cmd_pathbase::AuthMode::Anon,
            DEFAULT_BASE.to_string(),
            Some(bare.clone()),
            &config,
        );
        let bare_authed = resolve_destination(
            &share_args(),
            &authed(),
            DEFAULT_BASE.to_string(),
            Some(bare),
            &config,
        );
        let url_unauthed = resolve_destination(
            &share_args(),
            &crate::cmd_pathbase::AuthMode::Anon,
            DEFAULT_BASE.to_string(),
            Some(url.clone()),
            &config,
        );
        let url_authed = resolve_destination(
            &share_args(),
            &authed(),
            DEFAULT_BASE.to_string(),
            Some(url.clone()),
            &config,
        );
        let mut flag_args = share_args();
        flag_args.url = Some("https://flag.example".to_string());
        let url_flag_wins = resolve_destination(
            &flag_args,
            &authed(),
            "https://flag.example".to_string(),
            Some(url),
            &config,
        );

        let err = bare_unauthed.unwrap_err().to_string();
        assert!(err.contains("team/sessions"), "got: {err}");
        assert!(err.contains("path auth login"), "got: {err}");

        let dest = bare_authed.unwrap();
        let repo = dest.repo.unwrap();
        assert_eq!(
            (repo.owner.as_str(), repo.name.as_str()),
            ("team", "sessions")
        );
        assert_eq!(dest.base_url, DEFAULT_BASE);

        let err = url_unauthed.unwrap_err().to_string();
        assert!(
            err.contains("path auth login --url https://pathbase.internal"),
            "logged-out hint should name the remote's server: {err}"
        );

        let dest = url_authed.unwrap();
        assert_eq!(dest.base_url, "https://pathbase.internal");
        assert_eq!(dest.repo.unwrap().name, "proj");

        let dest = url_flag_wins.unwrap();
        assert_eq!(
            dest.base_url, "https://flag.example",
            "--url must beat the remote's embedded server"
        );
    }

    #[test]
    fn harness_status_for_empty_bundle_is_unresolved() {
        let bundle = HarnessBundle::default();
        // Every harness slot is None, so each status hits the unresolved branch.
        for status in [
            harness_status_claude(&bundle, None),
            harness_status_gemini(&bundle, None),
            harness_status_codex(&bundle, None),
            harness_status_opencode(&bundle, None),
            harness_status_pi(&bundle, None),
        ] {
            assert_eq!(status, HarnessStatus::unresolved());
            assert!(!status.exists);
        }
    }
}
