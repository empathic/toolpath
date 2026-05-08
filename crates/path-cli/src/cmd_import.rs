//! `path import <source>` — ingest external formats into toolpath documents.
//!
//! Default behavior writes each derived document into the on-disk cache at
//! `$CONFIG_DIR/documents/` under `<source>-<inner-id>.json` and prints the
//! path to stdout. `--no-cache` sends the JSON to stdout instead, for shell
//! composition with `render | query | validate`.

#[cfg(not(target_os = "emscripten"))]
use crate::fzf;
#[cfg(not(target_os = "emscripten"))]
use anyhow::Context;
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;
use toolpath::v1::Graph;

use crate::cmd_cache::{make_id, write_cached};

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

        /// Include thinking blocks in conversation.append text
        #[arg(long)]
        include_thinking: bool,
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
    /// Import from Pi (pi.dev) coding-agent session logs
    Pi {
        /// Project path (omit to interactively pick across all projects)
        #[arg(short, long)]
        project: Option<String>,

        /// Specific session ID (default: most recent or interactive pick)
        #[arg(short, long)]
        session: Option<String>,

        /// Process all sessions in the project (emits a Graph)
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

pub fn run(args: ImportArgs, pretty: bool) -> Result<()> {
    let docs = derive(args.source)?;
    emit(&docs, args.force, args.no_cache, pretty)
}

pub(crate) struct DerivedDoc {
    pub(crate) cache_id: String,
    pub(crate) doc: Graph,
}

fn emit(docs: &[DerivedDoc], force: bool, no_cache: bool, pretty: bool) -> Result<()> {
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
            let path = write_cached(&d.cache_id, &d.doc, force)?;
            println!("{}", path.display());
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

fn derive(source: ImportSource) -> Result<Vec<DerivedDoc>> {
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
        } => derive_claude(project, session, all),
        ImportSource::Gemini {
            project,
            session,
            all,
            include_thinking,
        } => derive_gemini(project, session, all, include_thinking),
        ImportSource::Codex { session, all } => derive_codex(session, all),
        ImportSource::Opencode {
            session,
            all,
            project,
            no_snapshot_diffs,
        } => derive_opencode(session, all, project, no_snapshot_diffs),
        ImportSource::Pi {
            project,
            session,
            all,
            base,
        } => derive_pi(project, session, all, base),
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
        let cache_id = make_id("git", &format!("{repo_tag}-{inner}"));
        Ok(vec![DerivedDoc { cache_id, doc }])
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

/// Extract the inner identifier from a graph (without source prefix).
fn doc_inner_id(doc: &Graph) -> String {
    doc.graph.id.clone()
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
        Ok(vec![DerivedDoc { cache_id, doc }])
    }
}

fn derive_claude(
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_claude::ClaudeConvo::new();
    derive_claude_with_manager(&manager, project, session, all)
}

fn derive_claude_with_manager(
    manager: &toolpath_claude::ClaudeConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    let make_config = |p: &str| toolpath_claude::derive::DeriveConfig {
        project_path: Some(p.to_string()),
        include_thinking: false,
    };

    // Interactive picker fires only when no explicit `--session` (and not
    // `--all`); the same flow handles single- and multi-select. If fzf isn't
    // available, we fall back to most-recent for explicit-project, or print
    // the recipe and bail when `--project` is also missing.
    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let convos = manager
                .read_all_conversations(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = make_config(&p);
            return wrap_paths_claude(toolpath_claude::derive::derive_project(&convos, &cfg));
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
                    let cfg = make_config(&p);
                    return wrap_paths_claude(vec![toolpath_claude::derive::derive_path(
                        &convo, &cfg,
                    )]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let convo = manager
                    .most_recent_conversation(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No conversations found for project: {}", p))?;
                let cfg = make_config(&p);
                return wrap_paths_claude(vec![toolpath_claude::derive::derive_path(&convo, &cfg)]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_claude_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fzf::print_recipe("claude", true);
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

    let mut paths: Vec<toolpath::v1::Path> = Vec::with_capacity(pairs.len());
    for (project_path, session_id) in &pairs {
        let convo = manager
            .read_conversation(project_path, session_id)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let cfg = make_config(project_path);
        paths.push(toolpath_claude::derive::derive_path(&convo, &cfg));
    }
    wrap_paths_claude(paths)
}

/// Derive a single Claude conversation given an explicit project + session.
/// Used by `cmd_share` after its picker has resolved the pair; mirrors the
/// `(Some(p), Some(s), _)` arm in [`derive_claude_with_manager`].
pub(crate) fn derive_claude_pair(project: &str, session: &str) -> Result<DerivedDoc> {
    let manager = toolpath_claude::ClaudeConvo::new();
    let cfg = toolpath_claude::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking: false,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_claude::derive::derive_path(&convo, &cfg);
    let cache_id = make_id("claude", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}

fn wrap_paths_claude(paths: Vec<toolpath::v1::Path>) -> Result<Vec<DerivedDoc>> {
    Ok(paths
        .into_iter()
        .map(|p| {
            let cache_id = make_id("claude", &p.path.id);
            DerivedDoc {
                cache_id,
                doc: Graph::from_path(p),
            }
        })
        .collect())
}

#[cfg(not(target_os = "emscripten"))]
fn pick_claude_in_project(
    manager: &toolpath_claude::ClaudeConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
            // Cols 1+2 are hidden ID columns the parser uses; cols 3+ are
            // visible to fzf for display + fuzzy-match. Lead the visible
            // section with the title so the user is searching what the
            // conversation was *about*, not on UUIDs.
            format!(
                "{}\t{}\t{}\t{}\t{} msgs",
                tab_safe(&m.project_path),
                tab_safe(&m.session_id),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                short_timestamp(m.last_activity),
                m.message_count,
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "claude session> ",
        preview: Some("path show claude --project {1} --session {2}"),
        header: Some("pick a Claude session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_claude_global(
    manager: &toolpath_claude::ClaudeConvo,
) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
                "{}\t{}\t{}\t{}\t{} msgs\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_id),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                short_timestamp(m.last_activity),
                m.message_count,
                tab_safe(&project_short(&m.project_path)),
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "claude session> ",
        preview: Some("path show claude --project {1} --session {2}"),
        header: Some("pick a Claude session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

fn derive_gemini(
    project: Option<String>,
    session: Option<String>,
    all: bool,
    include_thinking: bool,
) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_gemini::GeminiConvo::new();
    derive_gemini_with_manager(&manager, project, session, all, include_thinking)
}

fn derive_gemini_with_manager(
    manager: &toolpath_gemini::GeminiConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
    include_thinking: bool,
) -> Result<Vec<DerivedDoc>> {
    let make_config = |p: &str| toolpath_gemini::derive::DeriveConfig {
        project_path: Some(p.to_string()),
        include_thinking,
    };

    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let convos = manager
                .read_all_conversations(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let cfg = make_config(&p);
            return wrap_paths_gemini(toolpath_gemini::derive::derive_project(&convos, &cfg));
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
                    let cfg = make_config(&p);
                    return wrap_paths_gemini(vec![toolpath_gemini::derive::derive_path(
                        &convo, &cfg,
                    )]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let convo = manager
                    .most_recent_conversation(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No conversations found for project: {}", p))?;
                let cfg = make_config(&p);
                return wrap_paths_gemini(vec![toolpath_gemini::derive::derive_path(&convo, &cfg)]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_gemini_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fzf::print_recipe("gemini", true);
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

    let mut paths: Vec<toolpath::v1::Path> = Vec::with_capacity(pairs.len());
    for (project_path, session_uuid) in &pairs {
        let convo = manager
            .read_conversation(project_path, session_uuid)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let cfg = make_config(project_path);
        paths.push(toolpath_gemini::derive::derive_path(&convo, &cfg));
    }
    wrap_paths_gemini(paths)
}

/// Derive a single Gemini conversation given an explicit project + session.
pub(crate) fn derive_gemini_pair(
    project: &str,
    session: &str,
    include_thinking: bool,
) -> Result<DerivedDoc> {
    let manager = toolpath_gemini::GeminiConvo::new();
    let cfg = toolpath_gemini::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_gemini::derive::derive_path(&convo, &cfg);
    let cache_id = make_id("gemini", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}

fn wrap_paths_gemini(paths: Vec<toolpath::v1::Path>) -> Result<Vec<DerivedDoc>> {
    Ok(paths
        .into_iter()
        .map(|p| {
            let cache_id = make_id("gemini", &p.path.id);
            DerivedDoc {
                cache_id,
                doc: Graph::from_path(p),
            }
        })
        .collect())
}

#[cfg(not(target_os = "emscripten"))]
fn pick_gemini_in_project(
    manager: &toolpath_gemini::GeminiConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
                "{}\t{}\t{}\t{}\t{} msgs",
                tab_safe(&m.project_path),
                tab_safe(&m.session_uuid),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                short_timestamp(m.last_activity),
                m.message_count,
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "gemini session> ",
        preview: Some("path show gemini --project {1} --session {2}"),
        header: Some("pick a Gemini session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_gemini_global(
    manager: &toolpath_gemini::GeminiConvo,
) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
                "{}\t{}\t{}\t{}\t{} msgs\t{}",
                tab_safe(&m.project_path),
                tab_safe(&m.session_uuid),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                short_timestamp(m.last_activity),
                m.message_count,
                tab_safe(&project_short(&m.project_path)),
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "gemini session> ",
        preview: Some("path show gemini --project {1} --session {2}"),
        header: Some("pick a Gemini session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

fn derive_codex(session: Option<String>, all: bool) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_codex::CodexConvo::new();
    let config = toolpath_codex::derive::DeriveConfig { project_path: None };

    let session_ids: Vec<String> = match (session, all) {
        (Some(s), _) => vec![s],
        (None, true) => {
            let sessions = manager
                .read_all_sessions()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if sessions.is_empty() {
                anyhow::bail!("No Codex sessions found in ~/.codex/sessions");
            }
            return wrap_paths_codex(toolpath_codex::derive::derive_project(&sessions, &config));
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
                        return wrap_paths_codex(vec![toolpath_codex::derive::derive_path(
                            &s, &config,
                        )]);
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
                return wrap_paths_codex(vec![toolpath_codex::derive::derive_path(&s, &config)]);
            }
        }
    };

    let mut paths: Vec<toolpath::v1::Path> = Vec::with_capacity(session_ids.len());
    for sid in &session_ids {
        let s = manager
            .read_session(sid)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        paths.push(toolpath_codex::derive::derive_path(&s, &config));
    }
    wrap_paths_codex(paths)
}

/// Derive a single Codex session given an explicit session id.
pub(crate) fn derive_codex_one(session: &str) -> Result<DerivedDoc> {
    let manager = toolpath_codex::CodexConvo::new();
    let config = toolpath_codex::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_codex::derive::derive_path(&s, &config);
    let cache_id = make_id("codex", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}

fn wrap_paths_codex(paths: Vec<toolpath::v1::Path>) -> Result<Vec<DerivedDoc>> {
    Ok(paths
        .into_iter()
        .map(|p| {
            let cache_id = make_id("codex", &p.path.id);
            DerivedDoc {
                cache_id,
                doc: Graph::from_path(p),
            }
        })
        .collect())
}

#[cfg(not(target_os = "emscripten"))]
fn pick_codex(manager: &toolpath_codex::CodexConvo) -> Result<Option<Vec<String>>> {
    if !fzf::available() {
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
            format!(
                "{}\t{}\t{}\t{} lines\t{}",
                tab_safe(&m.id),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                short_timestamp(m.last_activity),
                m.line_count,
                tab_safe(
                    &m.cwd
                        .as_ref()
                        .map(|p| project_short(&p.to_string_lossy()))
                        .unwrap_or_default(),
                ),
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "2..",
        prompt: "codex session> ",
        preview: Some("path show codex --session {1}"),
        header: Some("pick a Codex session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

fn derive_opencode(
    session: Option<String>,
    all: bool,
    project: Option<String>,
    no_snapshot_diffs: bool,
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (session, all, project, no_snapshot_diffs);
        anyhow::bail!(
            "'path import opencode' requires a native environment (SQLite + git2 not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager = toolpath_opencode::OpencodeConvo::new();
        let config = toolpath_opencode::derive::DeriveConfig {
            no_snapshot_diffs,
            ..Default::default()
        };
        let derive_one = |sid: &str| -> Result<toolpath::v1::Path> {
            let s = manager
                .read_session(sid)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            Ok(toolpath_opencode::derive::derive_path_with_resolver(
                &s,
                &config,
                manager.resolver(),
            ))
        };

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
                    out.push(derive_one(&m.id)?);
                }
                return wrap_paths_opencode(out);
            }
            (None, false) => match pick_opencode(&manager, project.as_deref())? {
                Some(picks) => picks,
                None => {
                    let s = manager
                        .most_recent_session()
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                        .ok_or_else(|| anyhow::anyhow!("No opencode sessions found"))?;
                    return wrap_paths_opencode(vec![
                        toolpath_opencode::derive::derive_path_with_resolver(
                            &s,
                            &config,
                            manager.resolver(),
                        ),
                    ]);
                }
            },
        };

        let mut paths: Vec<toolpath::v1::Path> = Vec::with_capacity(session_ids.len());
        for sid in &session_ids {
            paths.push(derive_one(sid)?);
        }
        wrap_paths_opencode(paths)
    }
}

/// Derive a single opencode session given an explicit session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_one(
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    let manager = toolpath_opencode::OpencodeConvo::new();
    let config = toolpath_opencode::derive::DeriveConfig {
        no_snapshot_diffs,
        ..Default::default()
    };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path =
        toolpath_opencode::derive::derive_path_with_resolver(&s, &config, manager.resolver());
    let cache_id = make_id("opencode", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}

fn wrap_paths_opencode(paths: Vec<toolpath::v1::Path>) -> Result<Vec<DerivedDoc>> {
    Ok(paths
        .into_iter()
        .map(|p| {
            let cache_id = make_id("opencode", &p.path.id);
            DerivedDoc {
                cache_id,
                doc: Graph::from_path(p),
            }
        })
        .collect())
}

#[cfg(not(target_os = "emscripten"))]
fn pick_opencode(
    manager: &toolpath_opencode::OpencodeConvo,
    project: Option<&str>,
) -> Result<Option<Vec<String>>> {
    if !fzf::available() {
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
            format!(
                "{}\t{}\t{}\t{} msgs\t{}",
                tab_safe(&m.id),
                fzf_title(
                    m.first_user_message
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(m.title.as_str()),
                ),
                short_timestamp(m.last_activity),
                m.message_count,
                tab_safe(&project_short(&m.directory.to_string_lossy())),
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "2..",
        prompt: "opencode session> ",
        preview: Some("path show opencode --session {1}"),
        header: Some("pick an opencode session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_single_id(&selected)))
}

fn derive_pi(
    project: Option<String>,
    session: Option<String>,
    all: bool,
    base: Option<PathBuf>,
) -> Result<Vec<DerivedDoc>> {
    let manager = if let Some(path) = base {
        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&path);
        toolpath_pi::PiConvo::with_resolver(resolver)
    } else {
        toolpath_pi::PiConvo::new()
    };
    derive_pi_with_manager(&manager, project, session, all)
}

fn derive_pi_with_manager(
    manager: &toolpath_pi::PiConvo,
    project: Option<String>,
    session: Option<String>,
    all: bool,
) -> Result<Vec<DerivedDoc>> {
    let config = toolpath_pi::DeriveConfig::default();

    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let sessions = manager
                .read_all_sessions(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if sessions.is_empty() {
                anyhow::bail!("No Pi sessions found for project: {}", p);
            }
            let doc = toolpath_pi::derive::derive_graph(&sessions, None, &config);
            let cache_id = make_id("pi", &doc_inner_id(&doc));
            return Ok(vec![DerivedDoc { cache_id, doc }]);
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
                    let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
                    let cache_id = make_id("pi", &doc_inner_id(&doc));
                    return Ok(vec![DerivedDoc { cache_id, doc }]);
                }
            }
            #[cfg(target_os = "emscripten")]
            {
                let session = manager
                    .most_recent_session(&p)
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("No Pi sessions found for project: {}", p))?;
                let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
                let cache_id = make_id("pi", &doc_inner_id(&doc));
                return Ok(vec![DerivedDoc { cache_id, doc }]);
            }
        }
        (None, _, _) => {
            #[cfg(not(target_os = "emscripten"))]
            {
                match pick_pi_global(manager)? {
                    Some(picks) => picks,
                    None => {
                        fzf::print_recipe("pi", true);
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
        let session = manager
            .read_session(project_path, session_id)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
        let cache_id = make_id("pi", &doc_inner_id(&doc));
        docs.push(DerivedDoc { cache_id, doc });
    }
    Ok(docs)
}

/// Derive a single Pi session given an explicit project + session.
pub(crate) fn derive_pi_pair(
    project: &str,
    session: &str,
    base: Option<PathBuf>,
) -> Result<DerivedDoc> {
    let manager = if let Some(path) = base {
        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&path);
        toolpath_pi::PiConvo::with_resolver(resolver)
    } else {
        toolpath_pi::PiConvo::new()
    };
    let config = toolpath_pi::DeriveConfig::default();
    let session = manager
        .read_session(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
    let cache_id = make_id("pi", &doc_inner_id(&doc));
    Ok(DerivedDoc { cache_id, doc })
}

#[cfg(not(target_os = "emscripten"))]
fn pick_pi_in_project(
    manager: &toolpath_pi::PiConvo,
    project: &str,
) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
                "{}\t{}\t{}\t{}\t{} entries",
                tab_safe(project),
                tab_safe(&m.id),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                tab_safe(&m.timestamp),
                m.entry_count,
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "pi session> ",
        preview: Some("path show pi --project {1} --session {2}"),
        header: Some("pick a Pi session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
    };
    Ok(Some(parse_project_session(&selected)))
}

#[cfg(not(target_os = "emscripten"))]
fn pick_pi_global(manager: &toolpath_pi::PiConvo) -> Result<Option<Vec<(String, String)>>> {
    if !fzf::available() {
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
                "{}\t{}\t{}\t{}\t{} entries\t{}",
                tab_safe(project),
                tab_safe(&m.id),
                fzf_title(m.first_user_message.as_deref().unwrap_or("(no prompt)")),
                tab_safe(&m.timestamp),
                m.entry_count,
                tab_safe(&project_short(project)),
            )
        })
        .collect();
    let opts = fzf::PickOptions {
        with_nth: "3..",
        prompt: "pi session> ",
        preview: Some("path show pi --project {1} --session {2}"),
        header: Some("pick a Pi session (TAB = multi-select, Enter = confirm)"),
        tiebreak: "index",
        multi: true,
    };
    let selected = match fzf::pick(&lines, &opts)? {
        fzf::PickResult::Selected(v) => v,
        fzf::PickResult::NoMatch | fzf::PickResult::Cancelled => Vec::new(),
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

/// Replace tabs/newlines so a TSV row stays one line with stable columns.
fn tab_safe(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Display-friendly title cell for an fzf row: tab-safe, single-line, capped
/// in length so a long pasted prompt doesn't push later columns off screen.
/// fzf still fuzzy-matches on the truncated form — full prompt text lives in
/// the preview pane via `path show`.
#[cfg(not(target_os = "emscripten"))]
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

/// Compact timestamp for fzf rows — `YYYY-MM-DD HH:MM` is enough resolution
/// to disambiguate sessions in a picker without eating a full RFC 3339 line.
#[cfg(not(target_os = "emscripten"))]
fn short_timestamp(t: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match t {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "          —     ".to_string(), // pad so the column stays aligned
    }
}

/// Last two path segments — enough to disambiguate `…/repo/.claude/worktrees/foo`
/// from `…/other-repo/main` without showing the full absolute path.
#[cfg(not(target_os = "emscripten"))]
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

fn derive_pathbase(target: String, url_flag: Option<String>) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (target, url_flag);
        anyhow::bail!("'path import pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use crate::cmd_pathbase::{credentials_path, load_session, paths_download, resolve_url};

        let (base, ref_) = parse_pathbase_ref(&target, url_flag.as_deref())?;
        let stored = load_session(&credentials_path()?)?;
        let base_url = base
            .or_else(|| stored.as_ref().map(|s| s.url.clone()))
            .unwrap_or_else(|| resolve_url(None));

        let token = stored.as_ref().map(|s| s.token.as_str());

        let PathRef { owner, repo, slug } = ref_;
        let body = paths_download(&base_url, token, &owner, &repo, &slug)?;
        let cache_id = make_id("pathbase", &format!("{owner}-{repo}-{slug}"));
        let doc = Graph::from_json(&body)
            .map_err(|e| anyhow::anyhow!("server returned a non-toolpath document: {e}"))?;
        Ok(vec![DerivedDoc { cache_id, doc }])
    }
}

/// What the user pointed at on the import side. The Pathbase API
/// addresses paths as `<owner>/<repo>/<slug>` triples; that's the only
/// shape we accept today. (Anon paths surface a UUID-as-slug under the
/// `anon` user, so they fold cleanly into the same struct.)
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, PartialEq)]
struct PathRef {
    owner: String,
    repo: String,
    slug: String,
}

/// Parse a positional ref for `path import pathbase`. Returns `(override_base, ref)`.
///
/// Accepted shapes:
/// - Full URL: `https://host/<owner>/<repo>/<slug>` — host overrides the
///   server URL.
/// - `owner/repo/slug` — bare triple, used with `--url` or the stored session.
#[cfg(not(target_os = "emscripten"))]
fn parse_pathbase_ref(target: &str, url_flag: Option<&str>) -> Result<(Option<String>, PathRef)> {
    use crate::cmd_pathbase::resolve_url;

    let scheme = if target.starts_with("https://") {
        Some("https://")
    } else if target.starts_with("http://") {
        Some("http://")
    } else {
        None
    };

    if let Some(scheme) = scheme {
        let rest = &target[scheme.len()..];
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (h, p),
            None => anyhow::bail!("URL has no path segments: {target}"),
        };
        if host.is_empty() {
            anyhow::bail!("URL is missing a host: {target}");
        }
        let path = path.split(['?', '#']).next().unwrap_or("");
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let triple = extract_triple(&segs).ok_or_else(|| {
            anyhow::anyhow!("expected URL ending in /<owner>/<repo>/<slug>, got {target}")
        })?;
        Ok((Some(format!("{scheme}{host}")), triple))
    } else {
        let base = url_flag.map(|u| resolve_url(Some(u.to_string())));
        let segs: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
        let triple = extract_triple(&segs)
            .ok_or_else(|| anyhow::anyhow!("expected `<owner>/<repo>/<slug>`, got `{target}`"))?;
        Ok((base, triple))
    }
}

/// Pull (owner, repo, slug) from a slash-split URL path. Two real shapes
/// in the wild:
///
/// - `/<owner>/<repo>/<slug>` — short authed URL (3 segs).
/// - `/<owner>/<repo>/paths/<slug>` — what the anon endpoint returns
///   (owner=anon), and likely the canonical SvelteKit route for repo-scoped
///   paths too (4 segs with literal `paths` second-to-last).
///
/// Heuristic: if the second-to-last segment is the literal `paths`, treat
/// it as a route delimiter and look back two more segments for owner/repo.
/// Otherwise take the last three segments.
#[cfg(not(target_os = "emscripten"))]
fn extract_triple(segs: &[&str]) -> Option<PathRef> {
    let n = segs.len();
    if n < 3 {
        return None;
    }

    let (owner, repo, slug) = if n >= 4 && segs[n - 2] == "paths" {
        (segs[n - 4], segs[n - 3], segs[n - 1])
    } else {
        (segs[n - 3], segs[n - 2], segs[n - 1])
    };

    if owner.is_empty() || repo.is_empty() || slug.is_empty() {
        return None;
    }
    Some(PathRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        slug: slug.to_string(),
    })
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    #[test]
    fn parse_pathbase_ref_full_url_owner_repo_slug() {
        let (base, ref_) =
            parse_pathbase_ref("https://pathbase.dev/alex/pathstash/my-path", None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                slug: "my-path".into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_with_url_flag() {
        let (base, ref_) =
            parse_pathbase_ref("alex/pathstash/my-path", Some("https://other.example/")).unwrap();
        assert_eq!(base.as_deref(), Some("https://other.example"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                slug: "my-path".into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_no_flag() {
        let (base, ref_) = parse_pathbase_ref("alex/pathstash/my-path", None).unwrap();
        assert_eq!(base, None);
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                slug: "my-path".into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_url_with_trailing_slash() {
        let (base, ref_) =
            parse_pathbase_ref("https://pathbase.dev/alex/pathstash/my-path/", None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(ref_.slug, "my-path");
    }

    #[test]
    fn parse_pathbase_ref_anon_url_with_paths_delimiter() {
        // The anon endpoint returns URLs like `/anon/pathstash/paths/<uuid>`.
        // Parser must recognize the literal `paths` as a route delimiter.
        let (_, ref_) =
            parse_pathbase_ref("https://pathbase.dev/anon/pathstash/paths/abc-123", None).unwrap();
        assert_eq!(
            ref_,
            PathRef {
                owner: "anon".into(),
                repo: "pathstash".into(),
                slug: "abc-123".into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_rejects_too_few_segments() {
        assert!(parse_pathbase_ref("https://pathbase.dev/just-one", None).is_err());
        assert!(parse_pathbase_ref("just/two", None).is_err());
    }

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
