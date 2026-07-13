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

use crate::cmd_cache::{make_id, write_cached};
use crate::sync::{ArtifactStub, ArtifactType, codex_artifact_id, stat_stamp};

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

pub fn run(args: ImportArgs, pretty: bool) -> Result<()> {
    let docs = derive(args.source)?;
    emit(&docs, args.force, args.no_cache, pretty)
}

pub(crate) struct DerivedDoc {
    pub(crate) cache_id: String,
    pub(crate) doc: Graph,
    /// Identity + source stamp for the sync manifest, captured *before*
    /// the source was read (so a write racing the derive re-syncs next
    /// run). `None` for sources the manifest doesn't track (github,
    /// pathbase) and for bulk `--all` derives that no longer know
    /// per-artifact sources.
    pub(crate) provenance: Option<ArtifactStub>,
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
            #[cfg(not(target_os = "emscripten"))]
            if let Some(stub) = &d.provenance
                && let Err(e) = crate::sync::record_stub(stub, &d.cache_id)
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
        ImportSource::Copilot { session, all } => derive_copilot(session, all),
        ImportSource::Opencode {
            session,
            all,
            project,
            no_snapshot_diffs,
        } => derive_opencode(session, all, project, no_snapshot_diffs),
        ImportSource::Cursor {
            session,
            all,
            project,
        } => derive_cursor(session, all, project),
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
        let cache_id = make_id(ArtifactType::Git.name(), &format!("{repo_tag}-{inner}"));
        Ok(vec![DerivedDoc {
            cache_id,
            doc,
            provenance: Some(ArtifactStub {
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
                docs.push(derive_claude_session_with(manager, &p, head)?);
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

/// Derive a single Claude conversation given an explicit project + session.
/// Used by `cmd_share` after its picker has resolved the pair; mirrors the
/// `(Some(p), Some(s), _)` arm in [`derive_claude_with_manager`].
pub(crate) fn derive_claude_session(project: &str, session: &str) -> Result<DerivedDoc> {
    derive_claude_session_with(&toolpath_claude::ClaudeConvo::new(), project, session)
}

/// [`derive_claude_session`] against a caller-supplied manager, so sync
/// derives from the same roots it enumerated (and tests can inject a
/// fixture resolver).
pub(crate) fn derive_claude_session_with(
    manager: &toolpath_claude::ClaudeConvo,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    let (modified, size) = manager
        .resolver()
        .conversation_file(project, session)
        .map(|p| stat_stamp(&p))
        .unwrap_or((None, None));
    // The caller's project string often comes from claude's lossy dir
    // slugs ('/', '_', '.' all collapsed); leaving it out of the derive
    // lets path.base come from the session's own recorded cwd instead.
    let cfg = toolpath_claude::derive::DeriveConfig {
        project_path: None,
        include_thinking: false,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_claude::derive::derive_path(&convo, &cfg);
    let cache_id = make_id(ArtifactType::Claude.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Claude,
            id: session.to_string(),
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
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
    let pairs: Vec<(String, String)> = match (project, session, all) {
        (Some(p), Some(s), _) => vec![(p, s)],
        (Some(p), None, true) => {
            let ids = manager
                .list_conversations(&p)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            let mut docs = Vec::with_capacity(ids.len());
            for id in &ids {
                docs.push(derive_gemini_session_with(
                    manager,
                    &p,
                    id,
                    include_thinking,
                )?);
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
                        include_thinking,
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
                    include_thinking,
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
            include_thinking,
        )?);
    }
    Ok(docs)
}

/// Derive a single Gemini conversation given an explicit project + session.
pub(crate) fn derive_gemini_session(
    project: &str,
    session: &str,
    include_thinking: bool,
) -> Result<DerivedDoc> {
    derive_gemini_session_with(
        &toolpath_gemini::GeminiConvo::new(),
        project,
        session,
        include_thinking,
    )
}

/// [`derive_gemini_session`] against a caller-supplied manager.
pub(crate) fn derive_gemini_session_with(
    manager: &toolpath_gemini::GeminiConvo,
    project: &str,
    session: &str,
    include_thinking: bool,
) -> Result<DerivedDoc> {
    let entry = manager
        .resolver()
        .list_session_entries(project)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|e| e.id == session || e.session_uuid.as_deref() == Some(session))
        });
    let (artifact_id, (modified, size)) = match &entry {
        Some(e) => (
            e.session_uuid.clone().unwrap_or_else(|| e.id.clone()),
            stat_stamp(&e.path),
        ),
        None => (session.to_string(), (None, None)),
    };
    let cfg = toolpath_gemini::derive::DeriveConfig {
        project_path: Some(project.to_string()),
        include_thinking,
    };
    let convo = manager
        .read_conversation(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_gemini::derive::derive_path(&convo, &cfg);
    let cache_id = make_id(ArtifactType::Gemini.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Gemini,
            id: artifact_id,
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
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

fn derive_codex(session: Option<String>, all: bool) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_codex::CodexConvo::new();

    let session_ids: Vec<String> = match (session, all) {
        (Some(s), _) => vec![s],
        (None, true) => {
            let files = manager
                .io()
                .list_rollout_files()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if files.is_empty() {
                anyhow::bail!("No Codex sessions found in ~/.codex/sessions");
            }
            let mut docs = Vec::with_capacity(files.len());
            for file in &files {
                let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                docs.push(derive_codex_session_with(
                    &manager,
                    codex_artifact_id(stem),
                )?);
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

/// Derive a single Codex session given an explicit session id.
pub(crate) fn derive_codex_session(session: &str) -> Result<DerivedDoc> {
    derive_codex_session_with(&toolpath_codex::CodexConvo::new(), session)
}

/// [`derive_codex_session`] against a caller-supplied manager.
pub(crate) fn derive_codex_session_with(
    manager: &toolpath_codex::CodexConvo,
    session: &str,
) -> Result<DerivedDoc> {
    let file = manager.resolver().find_rollout_file(session).ok();
    let (modified, size) = file.as_deref().map(stat_stamp).unwrap_or((None, None));
    let artifact_id = file
        .as_deref()
        .and_then(|f| f.file_stem())
        .and_then(|stem| stem.to_str())
        .map(|stem| codex_artifact_id(stem).to_string())
        .unwrap_or_else(|| session.to_string());
    let config = toolpath_codex::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_codex::derive::derive_path(&s, &config);
    let cache_id = make_id(ArtifactType::Codex.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Codex,
            id: artifact_id,
            path: None,
            modified,
            size,
        }),
    })
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

fn derive_copilot(session: Option<String>, all: bool) -> Result<Vec<DerivedDoc>> {
    let manager = toolpath_copilot::CopilotConvo::new();
    let config = toolpath_copilot::derive::DeriveConfig { project_path: None };

    let session_ids: Vec<String> = match (session, all) {
        (Some(s), _) => vec![s],
        (None, true) => {
            let sessions = manager
                .read_all_sessions()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            if sessions.is_empty() {
                anyhow::bail!("No Copilot sessions found in ~/.copilot/session-state");
            }
            return wrap_paths_copilot(toolpath_copilot::derive::derive_project(
                &sessions, &config,
            ));
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
                        return wrap_paths_copilot(vec![toolpath_copilot::derive::derive_path(
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
                        anyhow::anyhow!("No Copilot sessions found in ~/.copilot/session-state")
                    })?;
                return wrap_paths_copilot(vec![toolpath_copilot::derive::derive_path(
                    &s, &config,
                )]);
            }
        }
    };

    let mut paths: Vec<toolpath::v1::Path> = Vec::with_capacity(session_ids.len());
    for sid in &session_ids {
        let s = manager
            .read_session(sid)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        paths.push(toolpath_copilot::derive::derive_path(&s, &config));
    }
    wrap_paths_copilot(paths)
}

/// Derive a single Copilot session given an explicit session id.
pub(crate) fn derive_copilot_session(session: &str) -> Result<DerivedDoc> {
    let manager = toolpath_copilot::CopilotConvo::new();
    let config = toolpath_copilot::derive::DeriveConfig { project_path: None };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = toolpath_copilot::derive::derive_path(&s, &config);
    let cache_id = make_id("copilot", &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
    })
}

fn wrap_paths_copilot(paths: Vec<toolpath::v1::Path>) -> Result<Vec<DerivedDoc>> {
    Ok(paths
        .into_iter()
        .map(|p| {
            let cache_id = make_id("copilot", &p.path.id);
            DerivedDoc {
                cache_id,
                doc: Graph::from_path(p),
            }
        })
        .collect())
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
                    out.push(derive_one(&m.id)?);
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

/// Derive a single opencode session given an explicit session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_session(
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    derive_opencode_session_with(
        &toolpath_opencode::OpencodeConvo::new(),
        session,
        no_snapshot_diffs,
    )
}

/// [`derive_opencode_session`] against a caller-supplied manager.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_opencode_session_with(
    manager: &toolpath_opencode::OpencodeConvo,
    session: &str,
    no_snapshot_diffs: bool,
) -> Result<DerivedDoc> {
    let modified = manager
        .io()
        .list_sessions(None)
        .ok()
        .and_then(|sessions| sessions.into_iter().find(|s| s.id == session))
        .and_then(|s| s.last_activity());
    let config = toolpath_opencode::derive::DeriveConfig {
        no_snapshot_diffs,
        ..Default::default()
    };
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let path =
        toolpath_opencode::derive::derive_path_with_resolver(&s, &config, manager.resolver());
    let cache_id = make_id(ArtifactType::Opencode.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Opencode,
            id: session.to_string(),
            path: None,
            modified,
            size: None,
        }),
    })
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
) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (session, all, project);
        anyhow::bail!(
            "'path import cursor' requires a native environment (SQLite not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager = toolpath_cursor::CursorConvo::new();
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
                    out.push(derive_one(&m.id)?);
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

/// Derive a single cursor composer given an explicit composer id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_cursor_session(session: &str) -> Result<DerivedDoc> {
    derive_cursor_session_with(&toolpath_cursor::CursorConvo::new(), session)
}

/// [`derive_cursor_session`] against a caller-supplied manager.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn derive_cursor_session_with(
    manager: &toolpath_cursor::CursorConvo,
    session: &str,
) -> Result<DerivedDoc> {
    let modified = manager
        .io()
        .read_composer_headers()
        .ok()
        .and_then(|h| {
            h.all_composers
                .into_iter()
                .find(|c| c.composer_id == session)
        })
        .and_then(|c| c.last_updated_at_utc());
    let s = manager
        .read_session(session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let cfg = toolpath_cursor::DeriveConfig::default();
    let path = toolpath_cursor::derive_path(&s, &cfg);
    let cache_id = make_id(ArtifactType::Cursor.name(), &path.path.id);
    Ok(DerivedDoc {
        cache_id,
        doc: Graph::from_path(path),
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Cursor,
            id: session.to_string(),
            path: None,
            modified,
            size: None,
        }),
    })
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
                docs.push(derive_pi_session_with(manager, &p, &m.id)?);
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

/// Derive a single Pi session given an explicit project + session.
pub(crate) fn derive_pi_session(
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
    derive_pi_session_with(&manager, project, session)
}

/// [`derive_pi_session`] against a caller-supplied manager.
pub(crate) fn derive_pi_session_with(
    manager: &toolpath_pi::PiConvo,
    project: &str,
    session: &str,
) -> Result<DerivedDoc> {
    let file = toolpath_pi::reader::list_session_files(manager.resolver(), project)
        .ok()
        .and_then(|files| {
            files.into_iter().find(|f| {
                toolpath_pi::reader::peek_header(f).is_ok_and(|h| h.id == session)
                    || f.file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| stem.split_once('_'))
                        .is_some_and(|(_, rest)| rest == session)
            })
        });
    let (modified, size) = file.as_deref().map(stat_stamp).unwrap_or((None, None));
    let artifact_id = session.to_string();
    let config = toolpath_pi::DeriveConfig::default();
    let session = manager
        .read_session(project, session)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let doc = Graph::from_path(toolpath_pi::derive::derive_path(&session, &config));
    let cache_id = make_id(ArtifactType::Pi.name(), &doc_inner_id(&doc));
    Ok(DerivedDoc {
        cache_id,
        doc,
        provenance: Some(ArtifactStub {
            artifact_type: ArtifactType::Pi,
            id: artifact_id,
            path: Some(project.to_string()),
            modified,
            size,
        }),
    })
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

/// Compute the local cache id a Pathbase ref would land at, without
/// hitting the network. Lets `path resume` probe the cache before
/// deciding whether to fetch.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn pathbase_cache_id_of(target: &str, url_flag: Option<&str>) -> Result<String> {
    let (_base, ref_) = parse_pathbase_ref(target, url_flag)?;
    let PathRef { owner, repo, id } = ref_;
    Ok(make_id("pathbase", &format!("{owner}-{repo}-{id}")))
}

/// Fetch a Pathbase ref (`https://host/u/owner/repos/repo/graphs/<uuid>`
/// URL or bare `owner/repo/<uuid>` triple) and parse it as a toolpath
/// document. Used by `path import pathbase` and `path resume <url>`.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn pathbase_fetch_to_doc(target: &str, url_flag: Option<&str>) -> Result<DerivedDoc> {
    use crate::cmd_pathbase::{credentials_path, graphs_download, load_session, resolve_url};

    let (base, ref_) = parse_pathbase_ref(target, url_flag)?;
    let stored = load_session(&credentials_path()?)?;
    let base_url = base
        .or_else(|| stored.as_ref().map(|s| s.url.clone()))
        .unwrap_or_else(|| resolve_url(None));

    let token = stored.as_ref().map(|s| s.token.as_str());

    let PathRef { owner, repo, id } = ref_;
    let body = graphs_download(&base_url, token, &owner, &repo, &id)?;
    let cache_id = make_id("pathbase", &format!("{owner}-{repo}-{id}"));
    let doc = Graph::from_json(&body)
        .map_err(|e| anyhow::anyhow!("server returned a non-toolpath document: {e}"))?;
    Ok(DerivedDoc {
        cache_id,
        doc,
        provenance: None,
    })
}

fn derive_pathbase(target: String, url_flag: Option<String>) -> Result<Vec<DerivedDoc>> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (target, url_flag);
        anyhow::bail!("'path import pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        Ok(vec![pathbase_fetch_to_doc(&target, url_flag.as_deref())?])
    }
}

/// What the user pointed at on the import side. Pathbase 1.1+
/// addresses graphs by UUID, so `id` is always a parseable UUID string;
/// `parse_pathbase_ref` rejects non-UUID trailing segments.
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug, PartialEq)]
struct PathRef {
    owner: String,
    repo: String,
    id: String,
}

/// Parse a positional ref for `path import pathbase`. Returns `(override_base, ref)`.
///
/// Accepted shapes (trailing identifier must be a UUID):
/// - Full URL: `https://host/u/<owner>/repos/<repo>/graphs/<uuid>` —
///   host overrides the server URL. Also accepts the older
///   `https://host/<owner>/<repo>/{paths|graphs}/<uuid>` shape and the
///   short `https://host/<owner>/<repo>/<uuid>` form.
/// - `<owner>/<repo>/<uuid>` — bare triple, used with `--url` or the
///   stored session.
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
            anyhow::anyhow!("expected URL ending in /<owner>/<repo>/graphs/<uuid> (got {target})")
        })?;
        Ok((Some(format!("{scheme}{host}")), triple))
    } else {
        let base = url_flag.map(|u| resolve_url(Some(u.to_string())));
        let segs: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
        let triple = extract_triple(&segs)
            .ok_or_else(|| anyhow::anyhow!("expected `<owner>/<repo>/<uuid>`, got `{target}`"))?;
        Ok((base, triple))
    }
}

/// Pull (owner, repo, uuid) from a slash-split URL path. Accepts all of:
///
/// - `/u/<owner>/repos/<repo>/graphs/<uuid>` — current canonical form.
/// - `/<owner>/<repo>/{paths|graphs}/<uuid>` — short SvelteKit route.
/// - `/<owner>/<repo>/<uuid>` — bare triple.
///
/// The trailing segment must parse as a UUID (the only addressing
/// scheme Pathbase 1.1+ accepts for graphs).
#[cfg(not(target_os = "emscripten"))]
fn extract_triple(segs: &[&str]) -> Option<PathRef> {
    let n = segs.len();
    if n < 3 {
        return None;
    }
    let id = segs[n - 1];
    if uuid::Uuid::parse_str(id).is_err() {
        return None;
    }

    // Look back through the canonical layouts in order of specificity.
    let (owner, repo) =
        if n >= 6 && segs[n - 6] == "u" && segs[n - 4] == "repos" && segs[n - 2] == "graphs" {
            (segs[n - 5], segs[n - 3])
        } else if n >= 4 && (segs[n - 2] == "paths" || segs[n - 2] == "graphs") {
            (segs[n - 4], segs[n - 3])
        } else {
            (segs[n - 3], segs[n - 2])
        };

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(PathRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        id: id.to_string(),
    })
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    const UUID: &str = "fe94b6f9-b0af-4cdd-b9ca-3c9a2a697537";

    #[test]
    fn parse_pathbase_ref_full_url_canonical_form() {
        let url = format!("https://pathbase.dev/u/alex/repos/pathstash/graphs/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_with_url_flag() {
        let target = format!("alex/pathstash/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&target, Some("https://other.example/")).unwrap();
        assert_eq!(base.as_deref(), Some("https://other.example"));
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_bare_triple_no_flag() {
        let target = format!("alex/pathstash/{UUID}");
        let (base, ref_) = parse_pathbase_ref(&target, None).unwrap();
        assert_eq!(base, None);
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_url_with_trailing_slash() {
        let url = format!("https://pathbase.dev/alex/pathstash/{UUID}/");
        let (base, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(base.as_deref(), Some("https://pathbase.dev"));
        assert_eq!(ref_.id, UUID);
    }

    #[test]
    fn parse_pathbase_ref_short_route_with_graphs_delimiter() {
        let url = format!("https://pathbase.dev/alex/pathstash/graphs/{UUID}");
        let (_, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(
            ref_,
            PathRef {
                owner: "alex".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_legacy_paths_delimiter_still_parses() {
        // Pre-1.1 share URLs used `/<owner>/<repo>/paths/<id>`. Keep
        // parsing them for back-compat — `id` still has to be a UUID
        // because that's the only addressing scheme the new server
        // understands; legacy slug-style refs no longer resolve.
        let url = format!("https://pathbase.dev/anon/pathstash/paths/{UUID}");
        let (_, ref_) = parse_pathbase_ref(&url, None).unwrap();
        assert_eq!(
            ref_,
            PathRef {
                owner: "anon".into(),
                repo: "pathstash".into(),
                id: UUID.into(),
            }
        );
    }

    #[test]
    fn parse_pathbase_ref_rejects_non_uuid_trailing_segment() {
        // Pathbase 1.1+ addresses graphs by UUID; a slug-style ref
        // can no longer be resolved, so fail at the parse step.
        assert!(parse_pathbase_ref("alex/pathstash/my-path", None).is_err());
        assert!(parse_pathbase_ref("https://pathbase.dev/alex/pathstash/my-path", None).is_err());
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

    #[test]
    #[cfg(not(target_os = "emscripten"))]
    fn pathbase_fetch_to_doc_url_input() {
        use crate::cmd_pathbase::tests::MockServer;
        let body = r#"{"graph":{"id":"g1"},"paths":[{"path":{"id":"p1","head":"s1"},"steps":[{"step":{"id":"s1","actor":"agent:claude-code","timestamp":"2026-01-01T00:00:00Z"},"change":{}}]}]}"#;
        let server = MockServer::start("HTTP/1.1 200 OK", body);
        let url = format!("{}/u/alex/repos/pathstash/graphs/{UUID}", server.base());

        let derived = pathbase_fetch_to_doc(&url, None).unwrap();

        assert_eq!(derived.cache_id, format!("pathbase-alex-pathstash-{UUID}"));
        assert!(derived.doc.into_single_path().is_some());
    }
}
