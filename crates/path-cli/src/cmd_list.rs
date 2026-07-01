#[cfg(not(target_os = "emscripten"))]
use anyhow::Context;
use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ListSource {
    /// List git branches in a repository
    Git {
        /// Path to the git repository
        #[arg(short, long, default_value = ".")]
        repo: PathBuf,

        /// Remote name for URI generation
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// List GitHub pull requests
    Github {
        /// Repository in owner/repo format
        #[arg(short, long)]
        repo: String,
    },
    /// List Claude projects or sessions
    Claude {
        /// Project path — if omitted, lists all projects (pretty) or all
        /// sessions across all projects (tsv/json).
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List Gemini CLI projects or sessions
    Gemini {
        /// Project path — if omitted, lists all projects (pretty) or all
        /// sessions across all projects (tsv/json).
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List Codex CLI sessions (global, newest first)
    Codex {},
    /// List GitHub Copilot CLI sessions (global, newest first; preview)
    Copilot {},
    /// List opencode sessions (global, newest first)
    Opencode {
        /// Filter by project id (SHA of repo's first root commit)
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List Cursor (IDE) composers (global, newest first). Reads
    /// `<user-data>/User/globalStorage/state.vscdb`.
    Cursor {
        /// Filter by workspace folder path (absolute). Matches by
        /// canonical equality against each composer's
        /// `workspaceIdentifier.uri.fsPath`.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List Pi (pi.dev) projects or sessions
    Pi {
        /// Project path — if omitted, lists all projects (pretty) or all
        /// sessions across all projects (tsv/json).
        #[arg(short, long)]
        project: Option<String>,

        /// Override the Pi sessions base directory (default: ~/.pi/agent/sessions)
        #[arg(long)]
        base: Option<PathBuf>,
    },
}

/// Output format selector. When neither `--format` nor `--json` is set, the
/// CLI picks `pretty` for a TTY and `tsv` when stdout is piped — that way
/// `path list claude | fzf …` and `path list claude` both do the right thing.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ListFormat {
    /// Human-readable, with section headers and aligned columns.
    Pretty,
    /// One JSON object per call.
    Json,
    /// One session per line, tab-delimited. See README for column layout.
    Tsv,
}

/// Resolve the effective format. `--format` wins; `--json` is the deprecated
/// alias; otherwise auto-detect by stdout TTY.
pub fn resolve_format(format: Option<ListFormat>, json_flag: bool) -> ListFormat {
    if let Some(f) = format {
        return f;
    }
    if json_flag {
        return ListFormat::Json;
    }
    if std::io::stdout().is_terminal() {
        ListFormat::Pretty
    } else {
        ListFormat::Tsv
    }
}

pub fn run(source: ListSource, format: Option<ListFormat>, json_flag: bool) -> Result<()> {
    let fmt = resolve_format(format, json_flag);
    match source {
        ListSource::Git { repo, remote } => run_git(repo, remote, fmt),
        ListSource::Github { repo } => run_github(repo, fmt),
        ListSource::Claude { project } => run_claude(project, fmt),
        ListSource::Gemini { project } => run_gemini(project, fmt),
        ListSource::Codex {} => run_codex(fmt),
        ListSource::Copilot {} => run_copilot(fmt),
        ListSource::Opencode { project } => run_opencode(project, fmt),
        ListSource::Cursor { project } => run_cursor(project, fmt),
        ListSource::Pi { project, base } => run_pi(project, base, fmt),
    }
}

// ── Git ─────────────────────────────────────────────────────────────────────

fn run_git(repo_path: PathBuf, remote: String, fmt: ListFormat) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (repo_path, remote, fmt);
        anyhow::bail!(
            "'path list git' requires a native environment with access to a git repository"
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

        let uri = toolpath_git::get_repo_uri(&repo, &remote)?;
        let branches = toolpath_git::list_branches(&repo)?;

        match fmt {
            ListFormat::Json => {
                let items: Vec<serde_json::Value> = branches
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "name": b.name,
                            "head": b.head,
                            "subject": b.subject,
                            "author": b.author,
                            "timestamp": b.timestamp,
                        })
                    })
                    .collect();
                let output = serde_json::json!({
                    "source": "git",
                    "uri": uri,
                    "branches": items,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            ListFormat::Tsv => {
                for b in &branches {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        sanitize_tsv(&b.name),
                        sanitize_tsv(&b.head),
                        sanitize_tsv(&b.timestamp),
                        sanitize_tsv(&b.author),
                        sanitize_tsv(&b.subject),
                    );
                }
            }
            ListFormat::Pretty => {
                println!("Repository: {}", uri);
                println!();
                if branches.is_empty() {
                    println!("  (no local branches)");
                } else {
                    for b in &branches {
                        println!("  {} {} {}", b.head_short, b.name, truncate(&b.subject, 60));
                    }
                }
            }
        }
        Ok(())
    }
}

// ── GitHub ──────────────────────────────────────────────────────────────────

fn run_github(repo: String, fmt: ListFormat) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (repo, fmt);
        anyhow::bail!("'path list github' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let (owner, repo_name) = repo
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("Repository must be in owner/repo format"))?;

        let token = toolpath_github::resolve_token()?;
        let config = toolpath_github::DeriveConfig {
            token,
            ..Default::default()
        };

        let prs = toolpath_github::list_pull_requests(owner, repo_name, &config)?;

        match fmt {
            ListFormat::Json => {
                let items: Vec<serde_json::Value> = prs
                    .iter()
                    .map(|pr| {
                        serde_json::json!({
                            "number": pr.number,
                            "title": pr.title,
                            "state": pr.state,
                            "author": pr.author,
                            "head_branch": pr.head_branch,
                            "base_branch": pr.base_branch,
                            "created_at": pr.created_at,
                            "updated_at": pr.updated_at,
                        })
                    })
                    .collect();
                let output = serde_json::json!({
                    "source": "github",
                    "repo": format!("{}/{}", owner, repo_name),
                    "pull_requests": items,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            ListFormat::Tsv => {
                for pr in &prs {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        pr.number,
                        sanitize_tsv(&pr.updated_at),
                        sanitize_tsv(&pr.state),
                        sanitize_tsv(&pr.author),
                        sanitize_tsv(&pr.title),
                    );
                }
            }
            ListFormat::Pretty => {
                println!("Pull requests for {}/{}:", owner, repo_name);
                println!();
                if prs.is_empty() {
                    println!("  (none)");
                } else {
                    for pr in &prs {
                        println!(
                            "  #{:<5} {:>8} {}  {}",
                            pr.number,
                            pr.state,
                            pr.author,
                            truncate(&pr.title, 50),
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

// ── Claude ──────────────────────────────────────────────────────────────────

fn run_claude(project: Option<String>, fmt: ListFormat) -> Result<()> {
    let manager = toolpath_claude::ClaudeConvo::new();

    match (project, fmt) {
        // TSV/JSON without --project: emit sessions across every project so
        // that fzf can pick one without the user pre-selecting a project.
        (None, ListFormat::Tsv) => list_claude_sessions_all(&manager, ListFormat::Tsv),
        (None, ListFormat::Json) => list_claude_sessions_all(&manager, ListFormat::Json),
        (None, ListFormat::Pretty) => list_claude_projects(&manager, ListFormat::Pretty),
        (Some(p), f) => list_claude_sessions(&manager, &p, f),
    }
}

fn list_claude_projects(manager: &toolpath_claude::ClaudeConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = projects
                .iter()
                .map(|p| serde_json::json!({ "path": p }))
                .collect();
            let output = serde_json::json!({
                "source": "claude",
                "projects": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for p in &projects {
                println!("{}", sanitize_tsv(p));
            }
        }
        ListFormat::Pretty => {
            println!("Claude projects:");
            println!();
            if projects.is_empty() {
                println!("  (none)");
            } else {
                for p in &projects {
                    println!("  {}", p);
                }
            }
        }
    }
    Ok(())
}

fn list_claude_sessions(
    manager: &toolpath_claude::ClaudeConvo,
    project_path: &str,
    fmt: ListFormat,
) -> Result<()> {
    let metadata = manager
        .list_conversation_metadata(project_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = metadata
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "session_id": m.session_id,
                        "project_path": m.project_path,
                        "messages": m.message_count,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "claude",
                "project": project_path,
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &metadata {
                emit_claude_tsv(m);
            }
        }
        ListFormat::Pretty => {
            println!("Sessions for {}:", project_path);
            println!();
            if metadata.is_empty() {
                println!("  (none)");
            } else {
                for m in &metadata {
                    let date = m
                        .last_activity
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("  {} {:>4} msgs  {}", &m.session_id, m.message_count, date);
                }
            }
        }
    }
    Ok(())
}

fn list_claude_sessions_all(manager: &toolpath_claude::ClaudeConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let mut all: Vec<toolpath_claude::ConversationMetadata> = Vec::new();
    for project in &projects {
        match manager.list_conversation_metadata(project) {
            Ok(metas) => all.extend(metas),
            Err(_) => continue, // skip unreadable projects rather than aborting
        }
    }
    all.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = all
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "session_id": m.session_id,
                        "project_path": m.project_path,
                        "messages": m.message_count,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "claude",
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &all {
                emit_claude_tsv(m);
            }
        }
        ListFormat::Pretty => unreachable!("pretty falls back to project listing"),
    }
    Ok(())
}

fn emit_claude_tsv(m: &toolpath_claude::ConversationMetadata) {
    println!(
        "{}\t{}\t{}\t{}\t{}",
        sanitize_tsv(&m.project_path),
        sanitize_tsv(&m.session_id),
        m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
        m.message_count,
        m.first_user_message
            .as_deref()
            .map(sanitize_tsv)
            .unwrap_or_default(),
    );
}

// ── Gemini ──────────────────────────────────────────────────────────────────

fn run_gemini(project: Option<String>, fmt: ListFormat) -> Result<()> {
    let manager = toolpath_gemini::GeminiConvo::new();

    match (project, fmt) {
        (None, ListFormat::Tsv) => list_gemini_sessions_all(&manager, ListFormat::Tsv),
        (None, ListFormat::Json) => list_gemini_sessions_all(&manager, ListFormat::Json),
        (None, ListFormat::Pretty) => list_gemini_projects(&manager, ListFormat::Pretty),
        (Some(p), f) => list_gemini_sessions(&manager, &p, f),
    }
}

fn list_gemini_projects(manager: &toolpath_gemini::GeminiConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = projects
                .iter()
                .map(|p| serde_json::json!({ "path": p }))
                .collect();
            let output = serde_json::json!({
                "source": "gemini",
                "projects": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for p in &projects {
                println!("{}", sanitize_tsv(p));
            }
        }
        ListFormat::Pretty => {
            println!("Gemini projects:");
            println!();
            if projects.is_empty() {
                println!("  (none)");
            } else {
                for p in &projects {
                    println!("  {}", p);
                }
            }
        }
    }
    Ok(())
}

fn list_gemini_sessions(
    manager: &toolpath_gemini::GeminiConvo,
    project_path: &str,
    fmt: ListFormat,
) -> Result<()> {
    let metadata = manager
        .list_conversation_metadata(project_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = metadata
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "session_uuid": m.session_uuid,
                        "project_path": m.project_path,
                        "messages": m.message_count,
                        "sub_agents": m.sub_agent_count,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "gemini",
                "project": project_path,
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &metadata {
                emit_gemini_tsv(m);
            }
        }
        ListFormat::Pretty => {
            println!("Gemini sessions for {}:", project_path);
            println!();
            if metadata.is_empty() {
                println!("  (none)");
            } else {
                for m in &metadata {
                    let date = m
                        .last_activity
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let sub = if m.sub_agent_count > 0 {
                        format!(" +{} sub", m.sub_agent_count)
                    } else {
                        String::new()
                    };
                    println!(
                        "  {} {:>4} msgs{}  {}",
                        &m.session_uuid, m.message_count, sub, date
                    );
                }
            }
        }
    }
    Ok(())
}

fn list_gemini_sessions_all(manager: &toolpath_gemini::GeminiConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let mut all: Vec<toolpath_gemini::ConversationMetadata> = Vec::new();
    for project in &projects {
        match manager.list_conversation_metadata(project) {
            Ok(metas) => all.extend(metas),
            Err(_) => continue,
        }
    }
    all.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = all
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "session_uuid": m.session_uuid,
                        "project_path": m.project_path,
                        "messages": m.message_count,
                        "sub_agents": m.sub_agent_count,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "gemini",
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &all {
                emit_gemini_tsv(m);
            }
        }
        ListFormat::Pretty => unreachable!("pretty falls back to project listing"),
    }
    Ok(())
}

fn emit_gemini_tsv(m: &toolpath_gemini::ConversationMetadata) {
    println!(
        "{}\t{}\t{}\t{}\t{}",
        sanitize_tsv(&m.project_path),
        sanitize_tsv(&m.session_uuid),
        m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
        m.message_count,
        m.first_user_message
            .as_deref()
            .map(sanitize_tsv)
            .unwrap_or_default(),
    );
}

// ── Codex ───────────────────────────────────────────────────────────────────

fn run_codex(fmt: ListFormat) -> Result<()> {
    let manager = toolpath_codex::CodexConvo::new();
    let sessions = manager
        .list_sessions()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = sessions
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "cwd": m.cwd,
                        "cli_version": m.cli_version,
                        "git_branch": m.git_branch,
                        "git_commit": m.git_commit,
                        "first_user_message": m.first_user_message,
                        "line_count": m.line_count,
                        "file_path": m.file_path,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "codex",
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &sessions {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    sanitize_tsv(&m.id),
                    m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    m.line_count,
                    m.cwd
                        .as_ref()
                        .map(|p| sanitize_tsv(&p.to_string_lossy()))
                        .unwrap_or_default(),
                    m.first_user_message
                        .as_deref()
                        .map(sanitize_tsv)
                        .unwrap_or_default(),
                );
            }
        }
        ListFormat::Pretty => {
            if sessions.is_empty() {
                println!("No Codex sessions found in ~/.codex/sessions.");
            } else {
                println!("Codex sessions:");
                println!();
                for m in &sessions {
                    let date = m
                        .last_activity
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let prompt = m
                        .first_user_message
                        .as_deref()
                        .map(|s| truncate(s, 60))
                        .unwrap_or_default();
                    let id_short: String = m.id.chars().take(8).collect();
                    let cwd = m
                        .cwd
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    println!(
                        "  {} {:>4} lines  {}  {}  {}",
                        id_short, m.line_count, date, cwd, prompt
                    );
                }
            }
        }
    }
    Ok(())
}

// ── Copilot (preview) ─────────────────────────────────────────────────────────

fn run_copilot(fmt: ListFormat) -> Result<()> {
    let manager = toolpath_copilot::CopilotConvo::new();
    let sessions = manager
        .list_sessions()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = sessions
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "started_at": m.started_at.map(|t| t.to_rfc3339()),
                        "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                        "cwd": m.cwd,
                        "cli_version": m.version,
                        "first_user_message": m.first_user_message,
                        "line_count": m.line_count,
                        "dir_path": m.dir_path,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "copilot",
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &sessions {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    sanitize_tsv(&m.id),
                    m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    m.line_count,
                    m.cwd.as_deref().map(sanitize_tsv).unwrap_or_default(),
                    m.first_user_message
                        .as_deref()
                        .map(sanitize_tsv)
                        .unwrap_or_default(),
                );
            }
        }
        ListFormat::Pretty => {
            if sessions.is_empty() {
                println!("No Copilot sessions found in ~/.copilot/session-state.");
            } else {
                println!("Copilot sessions:");
                println!();
                for m in &sessions {
                    let date = m
                        .last_activity
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let prompt = m
                        .first_user_message
                        .as_deref()
                        .map(|s| truncate(s, 60))
                        .unwrap_or_default();
                    let id_short: String = m.id.chars().take(8).collect();
                    let cwd = m.cwd.clone().unwrap_or_default();
                    println!(
                        "  {} {:>4} lines  {}  {}  {}",
                        id_short, m.line_count, date, cwd, prompt
                    );
                }
            }
        }
    }
    Ok(())
}

// ── opencode ────────────────────────────────────────────────────────────────

fn run_opencode(project: Option<String>, fmt: ListFormat) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (project, fmt);
        anyhow::bail!(
            "'path list opencode' requires a native environment (SQLite + git2 not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager = toolpath_opencode::OpencodeConvo::new();
        let metas = manager
            .io()
            .list_session_metadata(project.as_deref())
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        match fmt {
            ListFormat::Json => {
                let items: Vec<serde_json::Value> = metas
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "project_id": m.project_id,
                            "directory": m.directory,
                            "title": m.title,
                            "version": m.version,
                            "started_at": m.started_at.map(|t| t.to_rfc3339()),
                            "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                            "message_count": m.message_count,
                            "first_user_message": m.first_user_message,
                            "summary_additions": m.summary_additions,
                            "summary_deletions": m.summary_deletions,
                            "summary_files": m.summary_files,
                        })
                    })
                    .collect();
                let output = serde_json::json!({
                    "source": "opencode",
                    "project": project,
                    "sessions": items,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            ListFormat::Tsv => {
                for m in &metas {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        sanitize_tsv(&m.id),
                        m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
                        m.message_count,
                        sanitize_tsv(&m.directory.to_string_lossy()),
                        sanitize_tsv(m.first_user_message.as_deref().unwrap_or(m.title.as_str()),),
                    );
                }
            }
            ListFormat::Pretty => {
                if metas.is_empty() {
                    println!("No opencode sessions found in the database.");
                } else {
                    println!("opencode sessions:");
                    println!();
                    for m in &metas {
                        let date = m
                            .last_activity
                            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let id_short: String =
                            m.id.trim_start_matches("ses_").chars().take(10).collect();
                        let prompt = m
                            .first_user_message
                            .as_deref()
                            .map(|s| truncate(s, 60))
                            .unwrap_or_default();
                        let dir = m.directory.to_string_lossy();
                        println!(
                            "  {} {:>4} msgs  {}  {}  {}",
                            id_short, m.message_count, date, dir, prompt
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

// ── Cursor ──────────────────────────────────────────────────────────────────

fn run_cursor(project: Option<String>, fmt: ListFormat) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (project, fmt);
        anyhow::bail!(
            "'path list cursor' requires a native environment (SQLite not available under wasm)"
        );
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let manager = toolpath_cursor::CursorConvo::new();
        let mut metas = manager
            .io()
            .list_session_metadata()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Workspace filter — match against `workspace_path` by
        // canonical equality. Drop composers whose workspace doesn't
        // match (or that have no workspace, since those are remote/
        // untitled).
        if let Some(filter) = &project {
            let want = std::fs::canonicalize(filter).unwrap_or_else(|_| PathBuf::from(filter));
            metas.retain(|m| {
                m.workspace_path
                    .as_ref()
                    .map(|w| std::fs::canonicalize(w).unwrap_or_else(|_| w.clone()) == want)
                    .unwrap_or(false)
            });
        }

        match fmt {
            ListFormat::Json => {
                let items: Vec<serde_json::Value> = metas
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "name": m.name,
                            "subtitle": m.subtitle,
                            "started_at": m.started_at.map(|t| t.to_rfc3339()),
                            "last_activity": m.last_activity.map(|t| t.to_rfc3339()),
                            "unified_mode": m.unified_mode,
                            "workspace_path": m.workspace_path,
                            "message_count": m.message_count,
                            "first_user_message": m.first_user_message,
                        })
                    })
                    .collect();
                let output = serde_json::json!({
                    "source": "cursor",
                    "project": project,
                    "sessions": items,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            ListFormat::Tsv => {
                for m in &metas {
                    let ws = m
                        .workspace_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let title = m
                        .first_user_message
                        .as_deref()
                        .or(m.name.as_deref())
                        .unwrap_or("");
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        sanitize_tsv(&m.id),
                        m.last_activity.map(|t| t.to_rfc3339()).unwrap_or_default(),
                        m.message_count,
                        sanitize_tsv(&ws),
                        sanitize_tsv(title),
                    );
                }
            }
            ListFormat::Pretty => {
                if metas.is_empty() {
                    println!("No Cursor composers found in state.vscdb.");
                } else {
                    println!("Cursor composers:");
                    println!();
                    for m in &metas {
                        let date = m
                            .last_activity
                            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let id_short: String = m.id.chars().take(8).collect();
                        let title = m
                            .first_user_message
                            .as_deref()
                            .or(m.name.as_deref())
                            .map(|s| truncate(s, 60))
                            .unwrap_or_default();
                        let ws = m
                            .workspace_path
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "<no workspace>".to_string());
                        println!(
                            "  {} {:>4} msgs  {}  {}  {}",
                            id_short, m.message_count, date, ws, title
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

// ── Pi ──────────────────────────────────────────────────────────────────────

fn run_pi(project: Option<String>, base: Option<PathBuf>, fmt: ListFormat) -> Result<()> {
    let manager = if let Some(path) = base {
        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&path);
        toolpath_pi::PiConvo::with_resolver(resolver)
    } else {
        toolpath_pi::PiConvo::new()
    };

    match (project, fmt) {
        (None, ListFormat::Tsv) => list_pi_sessions_all(&manager, ListFormat::Tsv),
        (None, ListFormat::Json) => list_pi_sessions_all(&manager, ListFormat::Json),
        (None, ListFormat::Pretty) => list_pi_projects(&manager, ListFormat::Pretty),
        (Some(p), f) => list_pi_sessions(&manager, &p, f),
    }
}

fn list_pi_projects(manager: &toolpath_pi::PiConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = projects
                .iter()
                .map(|p| serde_json::json!({ "path": p }))
                .collect();
            let output = serde_json::json!({
                "source": "pi",
                "projects": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for p in &projects {
                println!("{}", sanitize_tsv(p));
            }
        }
        ListFormat::Pretty => {
            if projects.is_empty() {
                println!(
                    "No Pi projects found. Sessions dir: {:?}",
                    manager.resolver().sessions_dir()
                );
            } else {
                println!("Pi projects:");
                println!();
                for p in &projects {
                    println!("  {}", p);
                }
            }
        }
    }
    Ok(())
}

fn list_pi_sessions(
    manager: &toolpath_pi::PiConvo,
    project_path: &str,
    fmt: ListFormat,
) -> Result<()> {
    let sessions = manager
        .list_sessions(project_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = sessions
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "project_path": project_path,
                        "timestamp": m.timestamp,
                        "entry_count": m.entry_count,
                        "file_path": m.file_path,
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "pi",
                "project": project_path,
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for m in &sessions {
                emit_pi_tsv(project_path, m);
            }
        }
        ListFormat::Pretty => {
            if sessions.is_empty() {
                println!("No sessions found for project: {}", project_path);
            } else {
                println!("Sessions for {}:", project_path);
                println!();
                for m in &sessions {
                    println!("  {}\t{}\t{} entries", m.id, m.timestamp, m.entry_count);
                }
            }
        }
    }
    Ok(())
}

fn list_pi_sessions_all(manager: &toolpath_pi::PiConvo, fmt: ListFormat) -> Result<()> {
    let projects = manager
        .list_projects()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let mut all: Vec<(String, toolpath_pi::SessionMeta)> = Vec::new();
    for project in &projects {
        match manager.list_sessions(project) {
            Ok(metas) => {
                for m in metas {
                    all.push((project.clone(), m));
                }
            }
            Err(_) => continue,
        }
    }
    all.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));

    match fmt {
        ListFormat::Json => {
            let items: Vec<serde_json::Value> = all
                .iter()
                .map(|(project, m)| {
                    serde_json::json!({
                        "id": m.id,
                        "project_path": project,
                        "timestamp": m.timestamp,
                        "entry_count": m.entry_count,
                        "file_path": m.file_path,
                        "first_user_message": m.first_user_message,
                    })
                })
                .collect();
            let output = serde_json::json!({
                "source": "pi",
                "sessions": items,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        ListFormat::Tsv => {
            for (project, m) in &all {
                emit_pi_tsv(project, m);
            }
        }
        ListFormat::Pretty => unreachable!("pretty falls back to project listing"),
    }
    Ok(())
}

fn emit_pi_tsv(project: &str, m: &toolpath_pi::SessionMeta) {
    println!(
        "{}\t{}\t{}\t{}\t{}",
        sanitize_tsv(project),
        sanitize_tsv(&m.id),
        sanitize_tsv(&m.timestamp),
        m.entry_count,
        m.first_user_message
            .as_deref()
            .map(sanitize_tsv)
            .unwrap_or_default(),
    );
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Replace tabs and newlines in a TSV field so the column structure stays
/// intact. fzf reads line-by-line and splits on tabs — anything embedded
/// would shift the field layout.
fn sanitize_tsv(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;

    fn init_temp_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        (dir, repo)
    }

    fn create_commit(
        repo: &git2::Repository,
        message: &str,
        file_name: &str,
        content: &str,
        parent: Option<&git2::Commit>,
    ) -> git2::Oid {
        let mut index = repo.index().unwrap();
        let file_path = repo.workdir().unwrap().join(file_name);
        std::fs::write(&file_path, content).unwrap();
        index.add_path(std::path::Path::new(file_name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let parents: Vec<&git2::Commit> = parent.into_iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn test_run_git_pretty() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial commit", "file.txt", "hello", None);

        let result = run_git(
            dir.path().to_path_buf(),
            "origin".to_string(),
            ListFormat::Pretty,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_json() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial commit", "file.txt", "hello", None);

        let result = run_git(
            dir.path().to_path_buf(),
            "origin".to_string(),
            ListFormat::Json,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_tsv() {
        let (dir, repo) = init_temp_repo();
        create_commit(&repo, "initial commit", "file.txt", "hello", None);

        let result = run_git(
            dir.path().to_path_buf(),
            "origin".to_string(),
            ListFormat::Tsv,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_git_invalid_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_git(
            dir.path().to_path_buf(),
            "origin".to_string(),
            ListFormat::Pretty,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("hello world, this is a long string", 15);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 15);
    }

    #[test]
    fn test_truncate_multibyte() {
        let result = truncate("日本語のテスト文字列です", 8);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 8);
    }

    #[test]
    fn test_sanitize_tsv_replaces_tabs_and_newlines() {
        assert_eq!(sanitize_tsv("a\tb\nc\rd"), "a b c d");
    }

    #[test]
    fn test_resolve_format_explicit_wins() {
        let f = resolve_format(Some(ListFormat::Json), true);
        assert!(matches!(f, ListFormat::Json));
    }

    #[test]
    fn test_resolve_format_json_alias() {
        let f = resolve_format(None, true);
        assert!(matches!(f, ListFormat::Json));
    }

    fn setup_claude_manager() -> (tempfile::TempDir, toolpath_claude::ClaudeConvo) {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let entry1 = r#"{"type":"user","uuid":"uuid-1","timestamp":"2024-01-01T00:00:00Z","cwd":"/test/project","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"type":"assistant","uuid":"uuid-2","timestamp":"2024-01-01T00:01:00Z","message":{"role":"assistant","content":"Hi there"}}"#;
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
    fn test_list_claude_projects_pretty() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_projects(&manager, ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_projects_json() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_projects(&manager, ListFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_projects_tsv() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_projects(&manager, ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_projects_empty() {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let projects_dir = claude_dir.join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);

        let result = list_claude_projects(&manager, ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_pretty() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions(&manager, "/test/project", ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_json() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions(&manager, "/test/project", ListFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_tsv() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions(&manager, "/test/project", ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_all_tsv() {
        let (_temp, manager) = setup_claude_manager();
        let result = list_claude_sessions_all(&manager, ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_claude_sessions_empty() {
        let temp = tempfile::tempdir().unwrap();
        let claude_dir = temp.path().join(".claude");
        let projects_dir = claude_dir.join("projects/-empty-project");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let resolver = toolpath_claude::PathResolver::new().with_claude_dir(&claude_dir);
        let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);

        let result = list_claude_sessions(&manager, "/empty/project", ListFormat::Pretty);
        assert!(result.is_ok());
    }

    fn setup_pi_manager() -> (tempfile::TempDir, toolpath_pi::PiConvo) {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join(".pi/agent/sessions");
        let project_dir = sessions_dir.join("--test-project--");
        std::fs::create_dir_all(&project_dir).unwrap();

        let header = r#"{"type":"session","version":3,"id":"sess-1","timestamp":"2026-04-16T10:00:00Z","cwd":"/test/project"}"#;
        let msg1 = r#"{"type":"message","id":"m1","timestamp":"2026-04-16T10:00:01Z","message":{"role":"user","content":"Hello","timestamp":1744797601000}}"#;
        let msg2 = r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-04-16T10:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}],"api":"a","provider":"anthropic","model":"claude-x","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":1744797602000}}"#;
        std::fs::write(
            project_dir.join("2026-04-16_sess-1.jsonl"),
            [header, msg1, msg2].join("\n"),
        )
        .unwrap();

        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&sessions_dir);
        let manager = toolpath_pi::PiConvo::with_resolver(resolver);
        (temp, manager)
    }

    #[test]
    fn test_list_pi_projects_pretty() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_projects(&manager, ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_projects_json() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_projects(&manager, ListFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_projects_empty() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_dir = temp.path().join(".pi/agent/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let resolver = toolpath_pi::PathResolver::new().with_sessions_dir(&sessions_dir);
        let manager = toolpath_pi::PiConvo::with_resolver(resolver);

        let result = list_pi_projects(&manager, ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_sessions() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_sessions(&manager, "/test/project", ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_sessions_tsv() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_sessions(&manager, "/test/project", ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_sessions_all_tsv() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_sessions_all(&manager, ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_pi_nonexistent_project() {
        let (_temp, manager) = setup_pi_manager();
        let result = list_pi_sessions(&manager, "/does/not/exist", ListFormat::Pretty);
        assert!(result.is_err());
    }

    fn setup_gemini_manager() -> (tempfile::TempDir, toolpath_gemini::GeminiConvo) {
        let temp = tempfile::tempdir().unwrap();
        let gemini = temp.path().join(".gemini");
        let session_dir = gemini.join("tmp/myrepo/chats/session-uuid");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("main.json"),
            r#"{"sessionId":"s","projectHash":"","startTime":"2026-04-17T10:00:00Z","lastUpdated":"2026-04-17T10:10:00Z","directories":["/abs/myrepo"],"messages":[
  {"id":"u1","timestamp":"2026-04-17T10:00:00Z","type":"user","content":[{"text":"Hello"}]}
]}"#,
        )
        .unwrap();
        let resolver = toolpath_gemini::PathResolver::new().with_gemini_dir(&gemini);
        (temp, toolpath_gemini::GeminiConvo::with_resolver(resolver))
    }

    #[test]
    fn test_list_gemini_projects_pretty() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_projects(&mgr, ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_projects_json() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_projects(&mgr, ListFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_sessions_pretty() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_sessions(&mgr, "/abs/myrepo", ListFormat::Pretty);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_sessions_json() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_sessions(&mgr, "/abs/myrepo", ListFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_sessions_tsv() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_sessions(&mgr, "/abs/myrepo", ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_sessions_all_tsv() {
        let (_t, mgr) = setup_gemini_manager();
        let result = list_gemini_sessions_all(&mgr, ListFormat::Tsv);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_gemini_sessions_empty() {
        let temp = tempfile::tempdir().unwrap();
        let gemini = temp.path().join(".gemini");
        std::fs::create_dir_all(gemini.join("tmp/empty")).unwrap();
        let resolver = toolpath_gemini::PathResolver::new().with_gemini_dir(&gemini);
        let mgr = toolpath_gemini::GeminiConvo::with_resolver(resolver);
        let result = list_gemini_sessions(&mgr, "/nowhere", ListFormat::Pretty);
        assert!(result.is_ok());
    }
}
