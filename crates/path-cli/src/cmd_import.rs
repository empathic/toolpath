//! `path import <source>` — ingest external formats into toolpath documents.
//!
//! Default behavior writes each derived document into the on-disk cache at
//! `$CONFIG_DIR/documents/` under `<source>-<inner-id>.json` and prints the
//! path to stdout. `--no-cache` sends the JSON to stdout instead, for shell
//! composition with `render | query | validate`.

#[cfg(not(target_os = "emscripten"))]
use crate::fuzzy;
#[cfg(not(target_os = "emscripten"))]
use anyhow::Context;
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::Graph;

#[cfg(not(target_os = "emscripten"))]
use crate::artifact::{ArtifactRef, ArtifactType};
#[cfg(not(target_os = "emscripten"))]
use crate::cache::make_id;
use crate::cache::write_cached;
use crate::config::Config;
use crate::derive::{
    DerivedDoc, derive_claude_session_with, derive_codex_session_with, derive_copilot_session_with,
    derive_gemini_session_with, derive_pi_session_with,
};
#[cfg(not(target_os = "emscripten"))]
use crate::derive::{derive_cursor_session_with, derive_opencode_session_with, doc_inner_id};
use crate::providers;

#[derive(Subcommand, Debug)]
pub enum ImportSource {
    /// Import from git repository history
    Git {
        /// Path to the git repository
        #[arg(short, long, default_value = ".")]
        repo: PathBuf,

        /// Branch name(s). Format: `name` or `name:start`
        #[arg(short, long, required = true)]
        branch: Vec<String>,

        /// Global base commit (overrides per-branch starts)
        #[arg(long)]
        base: Option<String>,

        /// Remote name for URI generation
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Graph title (for multi-branch output)
        #[arg(long)]
        title: Option<String>,
    },
    /// Import from a GitHub pull request
    Github {
        /// PR URL (e.g. <https://github.com/owner/repo/pull/42>)
        #[arg(index = 1)]
        url: Option<String>,

        /// Repository in owner/repo format (alternative to URL)
        #[arg(short, long)]
        repo: Option<String>,

        /// Pull request number (required with --repo)
        #[arg(long)]
        pr: Option<u64>,

        /// Exclude CI check runs
        #[arg(long)]
        no_ci: bool,

        /// Exclude reviews and comments
        #[arg(long)]
        no_comments: bool,
    },
    /// Import from Claude conversation logs
    Claude {
        /// Project path (omit to interactively pick across all projects)
        #[arg(short, long)]
        project: Option<String>,

        /// Specific session ID (omit to interactively pick)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions in the project
        #[arg(long)]
        all: bool,
    },
    /// Import from Gemini CLI conversation logs
    Gemini {
        /// Project path (omit to interactively pick across all projects)
        #[arg(short, long)]
        project: Option<String>,

        /// Specific session UUID (omit to interactively pick)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions in the project
        #[arg(long)]
        all: bool,
    },
    /// Import from Codex CLI rollout files
    Codex {
        /// Session id, UUID, or filename stem (default: most recent)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions (emits one Path per session)
        #[arg(long)]
        all: bool,
    },
    /// Import from GitHub Copilot CLI session logs (preview)
    Copilot {
        /// Session id or unique prefix (default: most recent)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions (emits one Path per session)
        #[arg(long)]
        all: bool,
    },
    /// Import from opencode session databases
    Opencode {
        /// Session id (default: most recent)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions (emits one Path per session)
        #[arg(long)]
        all: bool,

        /// Filter by project id (SHA of repo's first root commit)
        #[arg(long)]
        project: Option<String>,

        /// Skip snapshot-based file diff extraction
        #[arg(long)]
        no_snapshot_diffs: bool,
    },
    /// Import from Cursor (IDE) composers in `state.vscdb`
    Cursor {
        /// Composer UUID (default: interactive pick, or most recent
        /// when no picker is available)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all composers (emits one Path per composer)
        #[arg(long)]
        all: bool,

        /// Filter by workspace folder path (absolute). Matches by
        /// canonical equality against each composer's
        /// `workspaceIdentifier.uri.fsPath`.
        #[arg(long)]
        project: Option<String>,
    },
    /// Import from Pi (pi.dev) coding-agent session logs
    Pi {
        /// Project path (omit to interactively pick across all projects)
        #[arg(short, long)]
        project: Option<String>,

        /// Specific session ID (default: most recent or interactive pick)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions in the project
        #[arg(long)]
        all: bool,

        /// Override the Pi sessions base directory (default: ~/.pi/agent/sessions)
        #[arg(long)]
        base: Option<PathBuf>,
    },
    /// Import from Pathbase (download a previously uploaded path)
    Pathbase {
        /// Full Pathbase URL or bare `<owner>/<repo>/<slug>` triple
        #[arg(index = 1)]
        target: String,

        /// Pathbase server URL (overrides $PATHBASE_URL; ignored if target is a URL)
        #[arg(long)]
        url: Option<String>,
    },
}

#[derive(clap::Args, Debug)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub source: ImportSource,

    /// Overwrite the cache entry if it already exists
    #[arg(long, global = true)]
    pub force: bool,

    /// Print the toolpath JSON to stdout instead of writing the cache
    #[arg(long, global = true)]
    pub no_cache: bool,
}

pub fn run(args: ImportArgs, pretty: bool, config: &Config) -> Result<()> {
    let docs = derive(args.source, config)?;
    emit(&docs, args.force, args.no_cache, pretty, config)
}

#[cfg_attr(target_os = "emscripten", expect(unused_variables))]
fn emit(
    docs: &[DerivedDoc],
    force: bool,
    no_cache: bool,
    pretty: bool,
    config: &Config,
) -> Result<()> {
    if docs.is_empty() {
        anyhow::bail!("no documents produced");
    }
    for d in docs {
        if no_cache {
            let json = if pretty {
                d.doc.to_json_pretty()?
            } else {
                d.doc.to_json()?
            };
            println!("{}", json);
        } else {
            // The implicit sync in `path query` fills the cache under
            // these same IDs; re-importing an artifact whose record is
            // still fresh is a no-op, not an exists-error.
            #[cfg(not(target_os = "emscripten"))]
            if !force
                && let Some(stub) = &d.provenance
                && crate::sync::record_is_current(config, stub, &d.cache_id)
            {
                println!("{}", crate::cache::cache_path(&d.cache_id)?.display());
                eprintln!(
                    "{} is already up to date (pass --force to re-derive)",
                    d.cache_id
                );
                continue;
            }
            let path = write_cached(&d.cache_id, &d.doc, force)?;
            println!("{}", path.display());
            #[cfg(not(target_os = "emscripten"))]
            if let Some(stub) = &d.provenance
                && let Err(e) = crate::sync::record_artifact(config, stub, &d.cache_id)
            {
                eprintln!("warning: sync manifest not updated: {e}");
            }
            let summary = doc_summary(&d.doc);
            eprintln!("Imported {} → {}", summary, d.cache_id);
        }
    }
    Ok(())
}

fn doc_summary(doc: &Graph) -> String {
    if let Some(p) = doc.single_path() {
        format!("graph {} (1 path, {} steps)", doc.graph.id, p.steps.len())
    } else {
        format!("graph {} ({} paths)", doc.graph.id, doc.paths.len())
    }
}

fn derive(source: ImportSource, config: &Config) -> Result<Vec<DerivedDoc>> {
    match source {
        ImportSource::Git {
            repo,
            branch,
            base,
            remote,
            title,
        } => derive_git(repo, branch, base, remote, title),
        ImportSource::Github {
            url,
            repo,
            pr,
            no_ci,
            no_comments,
        } => derive_github(url, repo, pr, no_ci, no_comments),
        ImportSource::Claude {
            project,
            session,
            all,
        } => derive_claude(project, session, all, config),
        ImportSource::Gemini {
            project,
            session,
            all,
        } => derive_gemini(project, session, all, config),
        ImportSource::Codex { session, all } => derive_codex(session, all, config),
        ImportSource::Copilot { session, all } => derive_copilot(session, all, config),
        ImportSource::Opencode {
            session,
            all,
            project,
            no_snapshot_diffs,
        } => derive_opencode(session, all, project, no_snapshot_diffs, config),
        ImportSource::Cursor {
            session,
            all,
            project,
        } => derive_cursor(session, all, project, config),
        ImportSource::Pi {
            project,
            session,
            all,
            base,
        } => derive_pi(project, session, all, base, config),
        ImportSource::Pathbase { target, url } => derive_pathbase(target, url),
    }
}

// ── per-source derivations ─────────────────────────────────────────────

fn derive_git(
    repo_path: PathBuf,
    branches: Vec<String>,
    base: Option<String>,
    remote: String,
    title: Option<String>,
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (repo_path, branches, base, remote, title);
        anyhow::bail!(
            "'path import git' requires a native environment with access to a git repository"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let repo_path = if repo_path.is_absolute() {
            repo_path
        } else {
            std::env::current_dir()?.join(&repo_path)
        };

        let repo = git2::Repository::open(&repo_path)
            .with_context(|| format!("Failed to open repository at {:?}", repo_path))?;

        let config = toolpath_git::DeriveConfig {
            remote,
            title,
            base,
        };

        let doc = toolpath_git::derive(&repo, &branches, &config)?;
        // Fold a short hash of the canonical repo path into the cache id so
        // two repos on the same branch (both `main`) don't collide.
        let canonical = std::fs::canonicalize(&repo_path).unwrap_or(repo_path.clone());
        let repo_tag = short_path_hash(&canonical.to_string_lossy());
        let inner = doc_inner_id(&doc);
        let cache_id = make_id(ArtifactType::Git.name(), &format!("{repo_tag}-{inner}"));
        Ok(vec![DerivedDoc {
            cache_id,
            doc,
            provenance: Some(ArtifactRef {
                artifact_type: ArtifactType::Git,
                id: format!("{repo_tag}-{inner}"),
                path: Some(canonical.to_string_lossy().into_owned()),
                modified: None,
                size: None,
            }),
        }])
    }
}

/// 8-hex-char stable hash of a path string — used as a repo tag in
/// cache ids so imports from different repos don't collide.
fn short_path_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

fn derive_github(
    url: Option<String>,
    repo: Option<String>,
    pr: Option<u64>,
    no_ci: bool,
    no_comments: bool,
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (url, repo, pr, no_ci, no_comments);
        anyhow::bail!("'path import github' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let (owner, repo_name, pr_number) = if let Some(url_str) = &url {
            let parsed = toolpath_github::parse_pr_url(url_str).ok_or_else(|| {
                anyhow::anyhow!("Invalid PR URL. Expected: https://github.com/owner/repo/pull/N")
            })?;
            (parsed.owner, parsed.repo, parsed.number)
        } else if let (Some(repo_str), Some(pr_num)) = (&repo, pr) {
            let (o, r) = repo_str
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("Repository must be in owner/repo format"))?;
            (o.to_string(), r.to_string(), pr_num)
        } else {
            anyhow::bail!(
                "Provide a PR URL or both --repo and --pr.\n\
                 Usage: path import github https://github.com/owner/repo/pull/42\n\
                 Usage: path import github --repo owner/repo --pr 42"
            );
        };

        let token = toolpath_github::resolve_token()?;
        let config = toolpath_github::DeriveConfig {
            token,
            include_ci: !no_ci,
            include_comments: !no_comments,
            ..Default::default()
        };

        let path = toolpath_github::derive_pull_request(&owner, &repo_name, pr_number, &config)?;
        let doc = Graph::from_path(path);
        let cache_id = make_id("github", &format!("{owner}_{repo_name}-{pr_number}"));
        Ok(vec![DerivedDoc {
            cache_id,
            doc,
            provenance: None,
        }])
    }
}

fn derive_claude(
    project: Option<String>,
    session: Option<String>,
    all: bool,
    config: &Config,
) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_claude::ClaudeConvo::with_resolver(providers::claude_resolver(config));
    derive_claude_with_manager(&manager, project, session, all)
}

fn derive_claude_with_manager(
    manager: &toolpath_claude::ClaudeConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    // Interactive picker fires only when no explicit `--session` (and not
    // `--all`); the same flow handles single- and multi-select. If fzf isn't
    // available, we fall back to most-recent for explicit-project, or print
    // the recipe and bail when `--project` is also missing.
    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let heads = manager
                .list_conversations(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let mut docs = Vec::with_capacity(heads.len());
            for head in &heads {
                match derive_claude_session_with(manager, &p, head) {
                    Ok(doc) => docs.push(doc),
                    Err(e) => eprintln!("Warning: skipping session {head}: {e}"),
                }
            }
            return Ok(docs);
        }
        (Some(p), None, false) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                if let Some(picks) = pick_claude_in_project(manager, &p)? {
                    picks
                } else {
                    let convo = manager
                        .most_recent_conversation(&p)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                        .ok_or_else(|| {
                            anyhow::anyhow!("No conversations found for project: {}", p)
                        })?;
                    return Ok(vec![derive_claude_session_with(
                        manager,
                        &p,
                        &convo.session_id,
                    )?]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let convo = manager
                    .most_recent_conversation(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No conversations found for project: {}", p))?;
                return Ok(vec![derive_claude_session_with(
                    manager,
                    &p,
                    &convo.session_id,
                )?]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_claude_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fuzzy::print_recipe("claude", true);
                        anyhow::bail!("--project required when not running interactively");
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                anyhow::bail!("--project required");
            }
        }
    };

    let mut docs = Vec::with_capacity(pairs.len());
    for (project_path, session_id) in &pairs {
        docs.push(derive_claude_session_with(
            manager,
            project_path,
            session_id,
        )?);
    }
    Ok(docs)
}

#[cfg(not(target_os = "emscripten"))]
fn pick_claude_in_project(
    manager: &toolpath_claude::ClaudeConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .list_conversation_metadata(project)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            // Cols 1+2 are hidden ID columns the parser keys on; col 3
            // is the visible display string.
            format!(
                "{}\t{}\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    None,
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "claude session> ",
        preview: Some("{exe} show --ansi claude --project {1} --session {2}"),
        header: Some("pick a Claude session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_claude_global(
    manager: &toolpath_claude::ClaudeConvo,
) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut metas: Vec<toolpath_claude::ConversationMetadata> = Vec::new();
    for p in &projects {
        if let Ok(ms) = manager.list_conversation_metadata(p) {
            metas.extend(ms);
        }
    }
    metas.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    Some(&project_short(&m.project_path)),
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "claude session> ",
        preview: Some("{exe} show --ansi claude --project {1} --session {2}"),
        header: Some("pick a Claude session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

fn derive_gemini(
    project: Option<String>,
    session: Option<String>,
    all: bool,
    config: &Config,
) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_gemini::GeminiConvo::with_resolver(providers::gemini_resolver(config));
    derive_gemini_with_manager(&manager, project, session, all)
}

fn derive_gemini_with_manager(
    manager: &toolpath_gemini::GeminiConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let ids = manager
                .list_conversations(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let mut docs = Vec::with_capacity(ids.len());
            for id in &ids {
                match derive_gemini_session_with(manager, &p, id) {
                    Ok(doc) => docs.push(doc),
                    Err(e) => eprintln!("Warning: skipping session {id}: {e}"),
                }
            }
            return Ok(docs);
        }
        (Some(p), None, false) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                if let Some(picks) = pick_gemini_in_project(manager, &p)? {
                    picks
                } else {
                    let convo = manager
                        .most_recent_conversation(&p)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                        .ok_or_else(|| {
                            anyhow::anyhow!("No conversations found for project: {}", p)
                        })?;
                    return Ok(vec![derive_gemini_session_with(
                        manager,
                        &p,
                        &convo.session_uuid,
                    )?]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let convo = manager
                    .most_recent_conversation(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No conversations found for project: {}", p))?;
                return Ok(vec![derive_gemini_session_with(
                    manager,
                    &p,
                    &convo.session_uuid,
                )?]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_gemini_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fuzzy::print_recipe("gemini", true);
                        anyhow::bail!("--project required when not running interactively");
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                anyhow::bail!("--project required");
            }
        }
    };

    let mut docs = Vec::with_capacity(pairs.len());
    for (project_path, session_uuid) in &pairs {
        docs.push(derive_gemini_session_with(
            manager,
            project_path,
            session_uuid,
        )?);
    }
    Ok(docs)
}

#[cfg(not(target_os = "emscripten"))]
fn pick_gemini_in_project(
    manager: &toolpath_gemini::GeminiConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .list_conversation_metadata(project)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_uuid),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    None,
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "gemini session> ",
        preview: Some("{exe} show --ansi gemini --project {1} --session {2}"),
        header: Some("pick a Gemini session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_gemini_global(
    manager: &toolpath_gemini::GeminiConvo,
) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut metas: Vec<toolpath_gemini::ConversationMetadata> = Vec::new();
    for p in &projects {
        if let Ok(ms) = manager.list_conversation_metadata(p) {
            metas.extend(ms);
        }
    }
    metas.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_uuid),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    Some(&project_short(&m.project_path)),
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "gemini session> ",
        preview: Some("{exe} show --ansi gemini --project {1} --session {2}"),
        header: Some("pick a Gemini session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

fn derive_codex(session: Option<String>, all: bool, config: &Config) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_codex::CodexConvo::with_resolver(providers::codex_resolver(config));

    let session_ids: Vec<String> = match (session, all) {
        (Some(s), _) => vec![s],
        (None, true) => {
            let ids = manager
                .list_session_ids()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if ids.is_empty() {
                anyhow::bail!("No Codex sessions found in ~/.codex/sessions");
            }
            let mut docs = Vec::with_capacity(ids.len());
            for id in &ids {
                match derive_codex_session_with(&manager, id) {
                    Ok(doc) => docs.push(doc),
                    Err(e) => eprintln!("Warning: skipping session {id}: {e}"),
                }
            }
            return Ok(docs);
        }
        (None, false) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_codex(&manager)? {
                    Some(picks) => picks,
                    None => {
                        let s = manager
                            .most_recent_session()
                            .map_err(|e| anyhow::anyhow!("{}", e))?
                            .ok_or_else(|| {
                                anyhow::anyhow!("No Codex sessions found in ~/.codex/sessions")
                            })?;
                        return Ok(vec![derive_codex_session_with(&manager, &s.id)?]);
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let s = manager
                    .most_recent_session()
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("No Codex sessions found in ~/.codex/sessions")
                    })?;
                return Ok(vec![derive_codex_session_with(&manager, &s.id)?]);
            }
        }
    };

    let mut docs = Vec::with_capacity(session_ids.len());
    for sid in &session_ids {
        docs.push(derive_codex_session_with(&manager, sid)?);
    }
    Ok(docs)
}

#[cfg(not(target_os = "emscripten"))]
fn pick_codex(manager: &toolpath_codex::CodexConvo) -> Result<Option<Vec<String>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .list_sessions()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            let cwd_short = m.cwd.as_ref().map(|p| project_short(&p.to_string_lossy()));
            format!(
                "{}\t{}",
                tab_safe(&m.id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.line_count, "lines"),
                    cwd_short.as_deref(),
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "2",
        prompt: "codex session> ",
        preview: Some("{exe} show --ansi codex --session {1}"),
        header: Some("pick a Codex session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

fn derive_copilot(session: Option<String>, all: bool, config: &Config) -> Result<Vec<DerivedDoc>> {
    let manager =
        toolpath_copilot::CopilotConvo::with_resolver(providers::copilot_resolver(config));

    let session_ids: Vec<String> = match (session, all) {
        (Some(s), _) => vec![s],
        (None, true) => {
            let metas = manager
                .list_sessions()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if metas.is_empty() {
                anyhow::bail!("No Copilot sessions found in ~/.copilot/session-state");
            }
            let mut docs = Vec::with_capacity(metas.len());
            for m in &metas {
                match derive_copilot_session_with(&manager, &m.id) {
                    Ok(doc) => docs.push(doc),
                    Err(e) => eprintln!("Warning: skipping session {}: {e}", m.id),
                }
            }
            return Ok(docs);
        }
        (None, false) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_copilot(&manager)? {
                    Some(picks) => picks,
                    None => {
                        let s = manager
                            .most_recent_session()
                            .map_err(|e| anyhow::anyhow!("{}", e))?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No Copilot sessions found in ~/.copilot/session-state"
                                )
                            })?;
                        return Ok(vec![derive_copilot_session_with(&manager, &s.id)?]);
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let s = manager
                    .most_recent_session()
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("No Copilot sessions found in ~/.copilot/session-state")
                    })?;
                return Ok(vec![derive_copilot_session_with(&manager, &s.id)?]);
            }
        }
    };

    let mut docs = Vec::with_capacity(session_ids.len());
    for sid in &session_ids {
        docs.push(derive_copilot_session_with(&manager, sid)?);
    }
    Ok(docs)
}

#[cfg(not(target_os = "emscripten"))]
fn pick_copilot(manager: &toolpath_copilot::CopilotConvo) -> Result<Option<Vec<String>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .list_sessions()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            let cwd_short = m.cwd.as_deref().map(project_short);
            format!(
                "{}\t{}",
                tab_safe(&m.id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.line_count, "lines"),
                    cwd_short.as_deref(),
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "2",
        prompt: "copilot session> ",
        preview: Some("{exe} show --ansi copilot --session {1}"),
        header: Some("pick a Copilot session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

fn derive_opencode(
    session: Option<String>,
    all: bool,
    project: Option<String>,
    no_snapshot_diffs: bool,
    config: &Config,
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (session, all, project, no_snapshot_diffs, config);
        anyhow::bail!(
            "'path import opencode' requires a native environment (SQLite + git2 not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager =
            toolpath_opencode::OpencodeConvo::with_resolver(providers::opencode_resolver(config));
        let derive_one = |sid: &str| derive_opencode_session_with(&manager, sid, no_snapshot_diffs);

        let session_ids: Vec<String> = match (session, all) {
            (Some(s), _) => vec![s],
            (None, true) => {
                let metas = manager
                    .io()
                    .list_session_metadata(project.as_deref())
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                if metas.is_empty() {
                    anyhow::bail!("No opencode sessions found");
                }
                let mut out = Vec::with_capacity(metas.len());
                for m in &metas {
                    match derive_one(&m.id) {
                        Ok(doc) => out.push(doc),
                        Err(e) => eprintln!("Warning: skipping session {}: {e}", m.id),
                    }
                }
                return Ok(out);
            }
            (None, false) => match pick_opencode(&manager, project.as_deref())? {
                Some(picks) => picks,
                None => {
                    let s = manager
                        .most_recent_session()
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                        .ok_or_else(|| anyhow::anyhow!("No opencode sessions found"))?;
                    return Ok(vec![derive_one(&s.id)?]);
                }
            },
        };

        let mut docs = Vec::with_capacity(session_ids.len());
        for sid in &session_ids {
            docs.push(derive_one(sid)?);
        }
        Ok(docs)
    }
}

#[cfg(not(target_os = "emscripten"))]
fn pick_opencode(
    manager: &toolpath_opencode::OpencodeConvo,
    project: Option<&str>,
) -> Result<Option<Vec<String>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .io()
        .list_session_metadata(project)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            let dir_short = project_short(&m.directory.to_string_lossy());
            let title = m
                .first_user_message
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(m.title.as_str());
            format!(
                "{}\t{}",
                tab_safe(&m.id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    Some(&dir_short),
                    title,
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "2",
        prompt: "opencode session> ",
        preview: Some("{exe} show --ansi opencode --session {1}"),
        header: Some("pick an opencode session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

// ── Cursor ──────────────────────────────────────────────────────────────

fn derive_cursor(
    session: Option<String>,
    all: bool,
    project: Option<String>,
    config: &Config,
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (session, all, project, config);
        anyhow::bail!(
            "'path import cursor' requires a native environment (SQLite not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager =
            toolpath_cursor::CursorConvo::with_resolver(providers::cursor_resolver(config));
        let derive_one = |sid: &str| derive_cursor_session_with(&manager, sid);

        let workspace_filter = project
            .as_deref()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)));
        let workspace_match = |m: &toolpath_cursor::CursorSessionMetadata| -> bool {
            match (&workspace_filter, &m.workspace_path) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(want), Some(have)) => {
                    let canonical = std::fs::canonicalize(have).unwrap_or_else(|_| have.clone());
                    &canonical == want
                }
            }
        };

        let session_ids: Vec<String> = match (session, all) {
            (Some(s), _) => vec![s],
            (None, true) => {
                let metas = manager
                    .io()
                    .list_session_metadata()
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                let filtered: Vec<_> = metas.into_iter().filter(workspace_match).collect();
                if filtered.is_empty() {
                    anyhow::bail!("No Cursor composers found");
                }
                let mut out = Vec::with_capacity(filtered.len());
                for m in &filtered {
                    match derive_one(&m.id) {
                        Ok(doc) => out.push(doc),
                        Err(e) => eprintln!("Warning: skipping session {}: {e}", m.id),
                    }
                }
                return Ok(out);
            }
            (None, false) => match pick_cursor(&manager, workspace_filter.as_deref())? {
                Some(picks) => picks,
                None => {
                    // Fall back to the newest composer (matching the
                    // workspace filter, when given).
                    let metas = manager
                        .io()
                        .list_session_metadata()
                        .map_err(|e| anyhow::anyhow!("{}", e))?;
                    let pick = metas
                        .into_iter()
                        .filter(workspace_match)
                        .max_by_key(|m| {
                            m.last_activity
                                .unwrap_or_else(chrono::DateTime::<chrono::Utc>::default)
                        })
                        .ok_or_else(|| anyhow::anyhow!("No Cursor composers found"))?;
                    return Ok(vec![derive_one(&pick.id)?]);
                }
            },
        };

        let mut docs = Vec::with_capacity(session_ids.len());
        for sid in &session_ids {
            docs.push(derive_one(sid)?);
        }
        Ok(docs)
    }
}

#[cfg(not(target_os = "emscripten"))]
fn pick_cursor(
    manager: &toolpath_cursor::CursorConvo,
    workspace_filter: Option<&std::path::Path>,
) -> Result<Option<Vec<String>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .io()
        .list_session_metadata()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let metas: Vec<_> = metas
        .into_iter()
        .filter(|m| match (workspace_filter, &m.workspace_path) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(want), Some(have)) => {
                let canonical = std::fs::canonicalize(have).unwrap_or_else(|_| have.clone());
                canonical == want
            }
        })
        .collect();
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            let dir_short = m
                .workspace_path
                .as_ref()
                .map(|p| project_short(&p.to_string_lossy()))
                .unwrap_or_else(|| "<no workspace>".to_string());
            let title = m
                .first_user_message
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(m.name.as_deref())
                .unwrap_or("");
            format!(
                "{}\t{}",
                tab_safe(&m.id),
                render_row(
                    None,
                    m.last_activity,
                    &count(m.message_count, "msgs"),
                    Some(&dir_short),
                    title,
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "2",
        prompt: "cursor composer> ",
        preview: Some("{exe} show --ansi cursor --session {1}"),
        header: Some("pick a Cursor composer (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

fn derive_pi(
    project: Option<String>,
    session: Option<String>,
    all: bool,
    base: Option<PathBuf>,
    config: &Config,
) -> Result<Vec<DerivedDoc>> {
    let mut resolver = providers::pi_resolver(config);
    if let Some(path) = base {
        resolver = resolver.with_sessions_dir(&path);
    }
    let manager = toolpath_pi::PiConvo::with_resolver(resolver);
    derive_pi_with_manager(&manager, project, session, all)
}

fn derive_pi_with_manager(
    manager: &toolpath_pi::PiConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let metas = manager
                .list_sessions(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if metas.is_empty() {
                anyhow::bail!("No Pi sessions found for project: {}", p);
            }
            let mut docs = Vec::with_capacity(metas.len());
            for m in &metas {
                match derive_pi_session_with(manager, &p, &m.id) {
                    Ok(doc) => docs.push(doc),
                    Err(e) => eprintln!("Warning: skipping session {}: {e}", m.id),
                }
            }
            return Ok(docs);
        }
        (Some(p), None, false) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                if let Some(picks) = pick_pi_in_project(manager, &p)? {
                    picks
                } else {
                    let session = manager
                        .most_recent_session(&p)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                        .ok_or_else(|| {
                            anyhow::anyhow!("No Pi sessions found for project: {}", p)
                        })?;
                    return Ok(vec![derive_pi_session_with(
                        manager,
                        &p,
                        &session.header.id,
                    )?]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let session = manager
                    .most_recent_session(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No Pi sessions found for project: {}", p))?;
                return Ok(vec![derive_pi_session_with(
                    manager,
                    &p,
                    &session.header.id,
                )?]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_pi_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fuzzy::print_recipe("pi", true);
                        anyhow::bail!("--project required when not running interactively");
                    }
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                anyhow::bail!("--project required");
            }
        }
    };

    let mut docs: Vec<DerivedDoc> = Vec::with_capacity(pairs.len());
    for (project_path, session_id) in &pairs {
        docs.push(derive_pi_session_with(manager, project_path, session_id)?);
    }
    Ok(docs)
}

#[cfg(not(target_os = "emscripten"))]
fn pick_pi_in_project(
    manager: &toolpath_pi::PiConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let metas = manager
        .list_sessions(project)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    if metas.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = metas
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}",
                tab_safe(project),
                tab_safe(&m.id),
                render_row(
                    None,
                    parse_rfc3339(&m.timestamp),
                    &count(m.entry_count, "entries"),
                    None,
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "pi session> ",
        preview: Some("{exe} show --ansi pi --project {1} --session {2}"),
        header: Some("pick a Pi session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_pi_global(manager: &toolpath_pi::PiConvo) -> Result<Option<Vec<(String, String)>>> {
    if !fuzzy::available() {
        return Ok(None);
    }
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut all: Vec<(String, toolpath_pi::SessionMeta)> = Vec::new();
    for p in &projects {
        if let Ok(ms) = manager.list_sessions(p) {
            for m in ms {
                all.push((p.clone(), m));
            }
        }
    }
    all.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    if all.is_empty() {
        return Ok(None);
    }
    let lines: Vec<String> = all
        .iter()
        .map(|(project, m)| {
            format!(
                "{}\t{}\t{}",
                tab_safe(project),
                tab_safe(&m.id),
                render_row(
                    None,
                    parse_rfc3339(&m.timestamp),
                    &count(m.entry_count, "entries"),
                    Some(&project_short(project)),
                    m.first_user_message.as_deref().unwrap_or("(no prompt)"),
                ),
            )
        })
        .collect();
    let opts = fuzzy::PickOptions {
        with_nth: "3",
        prompt: "pi session> ",
        preview: Some("{exe} show --ansi pi --project {1} --session {2}"),
        header: Some("pick a Pi session (TAB = multi-select, Enter = confirm)"),
        preview_window: "right:60%:wrap-word",
        tiebreak: "index",
        multi: true,
    };
    let selected = match fuzzy::pick(&lines, &opts)? {
        fuzzy::PickResult::Selected(v) => v,
        fuzzy::PickResult::NoMatch | fuzzy::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

/// Parse fzf output where each line is `<project>\t<session>\t…`.
fn parse_project_session(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let project = parts.next()?.to_string();
            let session = parts.next()?.to_string();
            if project.is_empty() || session.is_empty() {
                None
            } else {
                Some((project, session))
            }
        })
        .collect()
}

#[cfg(not(target_os = "emscripten"))]
fn parse_single_id(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let id = line.split('\t').next()?.to_string();
            if id.is_empty() { None } else { Some(id) }
        })
        .collect()
}

#[cfg(not(target_os = "emscripten"))]
use crate::fuzzy::{count, project_short, render_row, tab_safe};

/// Parse an RFC 3339 timestamp string into `DateTime<Utc>` for picker
/// row rendering. Returns `None` when the string isn't parseable so
/// `fuzzy::render_row` falls back to its placeholder column rather
/// than blowing up the row. Used by the Pi provider, whose session
/// metadata stores timestamps as raw strings.
#[cfg(not(target_os = "emscripten"))]
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

fn derive_pathbase(target: String, url_flag: Option<String>) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (target, url_flag);
        anyhow::bail!("'path import pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        Ok(vec![crate::derive::pathbase_fetch_to_doc(
            &target,
            url_flag.as_deref(),
        )?])
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    fn setup_claude_manager() -> (tempfile::TempDir, toolpath_claude::ClaudeConvo) {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"uuid-1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"uuid-2","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there"}}"#;
        std::fs::write(
            project_dir.join("session-abc.jsonl"),
            format!("{}\n{}\n", entry1, entry2),
        )
        .unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);
        (temp, manager)
    }

    #[test]
    fn derive_claude_session_returns_one_doc() {
        let (_t, mgr) = setup_claude_manager();
        let out = derive_claude_with_manager(
            &mgr,
            Some("/test/project".to_string()),
            Some("session-abc".to_string()),
            false,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].cache_id.starts_with("claude-"));
    }

    fn setup_claude_manager_with_two_sessions() -> (tempfile::TempDir, toolpath_claude::ClaudeConvo)
    {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Use sufficiently distinct slugs so toolpath-claude's 8-char id
        // prefix doesn't alias them into the same path.id.
        for (slug, ts) in [
            ("alpha-session-one", "2024-01-01"),
            ("bravo-session-two", "2024-01-02"),
        ] {
            let u = format!(
                r#"{{"type":"user","uuid":"u-{slug}","timestamp":"{ts}T00:00:00Z","cwd":"/test/project","message":{{"role":"user","content":"hi"}}}}"#
            );
            let a = format!(
                r#"{{"type":"assistant","uuid":"a-{slug}","timestamp":"{ts}T00:00:01Z","message":{{"role":"assistant","content":"hello"}}}}"#
            );
            std::fs::write(
                project_dir.join(format!("{slug}.jsonl")),
                format!("{u}\n{a}\n"),
            )
            .unwrap();
        }

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        (temp, toolpath_claude::ClaudeConvo::with_resolver(resolver))
    }

    #[test]
    fn derive_claude_all_emits_one_cache_entry_per_session() {
        let (_t, mgr) = setup_claude_manager_with_two_sessions();
        let out = derive_claude_with_manager(&mgr, Some("/test/project".to_string()), None, true)
            .unwrap();
        assert_eq!(out.len(), 2);
        // Distinct cache ids so both can land in the cache without collision.
        assert_ne!(out[0].cache_id, out[1].cache_id);
        for d in &out {
            assert!(d.cache_id.starts_with("claude-"));
        }
    }
}
