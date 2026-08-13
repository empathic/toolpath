//! `path export <target>` — emit toolpath documents into external formats.
//!
//! - `export claude` projects a Path document into Claude Code's JSONL
//!   format (either into `~/.claude/projects/<sanitized>/<session>.jsonl`
//!   for a resumable session, or to a file / stdout).
//! - `export gemini` projects a Path document into Gemini CLI's chat-file
//!   layout under `~/.gemini/tmp/<slot>/chats/`, ready for
//!   `gemini --resume <uuid>`.
//! - `export pi` projects a Path document into Pi's JSONL session
//!   format under `~/.pi/agent/sessions/--<encoded-cwd>--/<id>.jsonl`.
//! - `export codex` projects a Path document into Codex CLI's rollout
//!   JSONL under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`,
//!   loadable via `codex resume <session-uuid>`.
//! - `export opencode` projects a Path document into rows in opencode's
//!   `~/.local/share/opencode/opencode.db` so the session is visible to
//!   the `opencode` CLI; or to a JSON file / stdout.
//! - `export pathbase` uploads the document to a Pathbase server.

#[cfg(not(target_os = "emscripten"))]
use anyhow::Context;
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[cfg(not(target_os = "emscripten"))]
use crate::cache::cache_ref;
use crate::config::Config;
#[cfg(not(target_os = "emscripten"))]
use crate::providers;
use crate::remote::RepoSpec;

#[derive(Subcommand, Debug)]
pub enum ExportTarget {
    /// Project a toolpath document into a Claude Code session
    Claude {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL into
        /// `~/.claude/projects/<sanitized>/<session>.jsonl` so `claude -r <id>`
        /// can resume it. Defaults to cwd when no `--output` is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,

        /// Overwrite the session file if this session id already exists in
        /// the target project. Without it the export refuses rather than
        /// clobbering local history.
        #[arg(long)]
        force: bool,
    },
    /// Project a toolpath document into a Gemini CLI session
    Gemini {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the chat
        /// files into `~/.gemini/tmp/<slot>/chats/` so `gemini --resume
        /// <uuid>` from the same directory can resume it. Defaults to
        /// cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the main chat file to this path. Sub-agents (if any)
        /// land in a sibling `<session-uuid>/` directory next to it.
        /// Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Pi (pi.dev) session
    Pi {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL
        /// into `~/.pi/agent/sessions/--<encoded-cwd>--/<session>.jsonl`
        /// under the encoding scheme Pi uses for project directories.
        /// Defaults to cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Codex CLI session
    Codex {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the JSONL
        /// into `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (Codex's
        /// date-bucketed layout). The session_meta line carries this
        /// path as `cwd`, so `codex resume <session-uuid>` invoked from
        /// the same directory will resolve it correctly. Defaults to
        /// cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output JSONL to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into an opencode session
    Opencode {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, inserts session,
        /// message, and part rows into `~/.local/share/opencode/opencode.db`
        /// so the `opencode` CLI sees the session in its picker.
        /// Defaults to cwd when neither --project nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `Session` (with messages and parts)
        /// as pretty JSON to this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a GitHub Copilot CLI session
    Copilot {
        /// Input: cache id (e.g. `copilot-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target project directory. With this flag, writes the session into
        /// `~/.copilot/session-state/<id>/` (+ a `session-store.db` row) with
        /// this directory as the session cwd, so `copilot --resume <id>` can
        /// load it. Only ever INSERTs a fresh id — never touches existing
        /// sessions.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `events.jsonl` to this file. Mutually
        /// exclusive with --project. With neither, prints it to stdout.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Project a toolpath document into a Cursor (IDE) composer
    Cursor {
        /// Input: cache id (e.g. `cursor-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Target workspace folder. With this flag, writes
        /// `composerData:`, `bubbleId:` and `composer.content.*` rows
        /// into `<user-data>/User/globalStorage/state.vscdb` so the
        /// composer appears in Cursor.app's chat sidebar when that
        /// folder is open. Defaults to cwd when neither --project
        /// nor --output is given.
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Output the projected `CursorSession` as pretty JSON to
        /// this file. Mutually exclusive with --project.
        #[arg(short, long, conflicts_with = "project")]
        output: Option<PathBuf>,
    },
    /// Upload a toolpath document to Pathbase.
    ///
    /// Default behavior depends on whether you're logged in:
    /// - Logged in (default): writes an unlisted graph under your
    ///   `pathstash` repo. Listable from your account; not publicly visible.
    /// - Not logged in: falls through to the public anonymous endpoint.
    ///   Anon graphs are not listable.
    ///
    /// Use `--repo`/`--name`/`--public` to override the pathstash default
    /// when authenticated. Use `--anon` to force the anonymous endpoint
    /// even when credentials are present.
    Pathbase {
        /// Input: cache id (e.g. `claude-abc`) or path to a toolpath JSON file
        #[arg(short, long)]
        input: String,

        /// Pathbase server URL (defaults to the stored session's server)
        #[arg(long)]
        url: Option<String>,

        /// Force the anonymous endpoint, ignoring any stored credentials
        #[arg(long, conflicts_with_all = ["repo", "public"])]
        anon: bool,

        /// Target a specific repo as `owner/name` instead of `<you>/pathstash`
        #[arg(long, value_parser = crate::remote::parse_repo_spec)]
        repo: Option<RepoSpec>,

        /// Human-readable display label for the uploaded graph
        /// (defaults to the toolpath document id). Free-form; not used
        /// in the URL — graphs are addressed by UUID server-side.
        #[arg(long, alias = "slug")]
        name: Option<String>,

        /// Mark the uploaded graph public (default: unlisted, addressable only by UUID)
        #[arg(long)]
        public: bool,
    },
}

pub fn run(target: ExportTarget, config: &Config) -> Result<()> {
    match target {
        ExportTarget::Claude {
            input,
            project,
            output,
            force,
        } => run_claude(input, project, output, force, config),
        ExportTarget::Gemini {
            input,
            project,
            output,
        } => run_gemini(input, project, output, config),
        ExportTarget::Pi {
            input,
            project,
            output,
        } => run_pi(input, project, output, config),
        ExportTarget::Codex {
            input,
            project,
            output,
        } => run_codex(input, project, output, config),
        ExportTarget::Opencode {
            input,
            project,
            output,
        } => run_opencode(input, project, output, config),
        ExportTarget::Copilot {
            input,
            project,
            output,
        } => run_copilot(input, project, output, config),
        ExportTarget::Cursor {
            input,
            project,
            output,
        } => run_cursor(input, project, output, config),
        ExportTarget::Pathbase {
            input,
            url,
            anon,
            repo,
            name,
            public,
        } => run_pathbase(
            PathbaseExportArgs {
                input,
                url,
                anon,
                repo,
                name,
                public,
            },
            config,
        ),
    }
}

#[derive(Debug)]
struct PathbaseExportArgs {
    input: String,
    url: Option<String>,
    anon: bool,
    repo: Option<RepoSpec>,
    name: Option<String>,
    public: bool,
}

/// Pathbase upload knobs that don't depend on where the body came from.
/// Identical to [`PathbaseExportArgs`] minus the `input` field — the body
/// is supplied by the caller (read from cache, derived in memory, …).
#[cfg(not(target_os = "emscripten"))]
#[derive(Debug)]
pub(crate) struct PathbaseUploadArgs {
    pub(crate) url: Option<String>,
    pub(crate) anon: bool,
    pub(crate) repo: Option<RepoSpec>,
    pub(crate) name: Option<String>,
    pub(crate) public: bool,
}

// ── pub(crate) project_<harness> wrappers ────────────────────────────
//
// These compose the private build + write helpers below and return the
// projected session id. They are called by `path resume`; the existing
// `run_<harness>` functions are untouched.

/// Outcome of projecting a Path into a Claude project directory.
#[cfg(not(target_os = "emscripten"))]
pub(crate) enum ClaudeProjection {
    /// The session file was written.
    Written { session_id: String },
    /// A session with this id already exists in the target project; nothing
    /// was written. Resuming the local copy is the least destructive move —
    /// it may be newer than the shared document.
    AlreadyLocal { session_id: String },
}

/// Project `path` into a Claude session under `project_dir`.
///
/// Never overwrites: if the session already exists locally the projection is
/// skipped and `AlreadyLocal` is returned (callers that want to clobber go
/// through `p export claude --force`).
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_claude(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<ClaudeProjection> {
    let conv = build_claude_conversation(path)?;
    if claude_session_file(&conv.session_id, project_dir, config)?.is_some() {
        return Ok(ClaudeProjection::AlreadyLocal {
            session_id: conv.session_id,
        });
    }
    let jsonl = serialize_jsonl(&conv)?;
    write_into_claude_project(&conv, &jsonl, project_dir, false, config)?;
    Ok(ClaudeProjection::Written {
        session_id: conv.session_id,
    })
}

/// Path of the session file for `session_id` under `project_dir`'s Claude
/// project directory, if it exists.
#[cfg(not(target_os = "emscripten"))]
fn claude_session_file(
    session_id: &str,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<Option<PathBuf>> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let resolver = providers::claude_resolver(config);
    let claude_project_dir = resolver
        .project_dir(&project_dir.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;
    let candidate = claude_project_dir.join(format!("{}.jsonl", session_id));
    Ok(candidate.exists().then_some(candidate))
}

/// Project `path` into a Gemini session under `project_dir` and return
/// the resulting session UUID.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_gemini(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let project_hash = toolpath_gemini::paths::project_hash(&project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.clone());
    let conv = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if conv.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID");
    }
    write_into_gemini_project(&conv, &project_path, config)?;
    Ok(conv.session_uuid)
}

/// Project `path` into a Codex session and return the resulting session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_codex(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd_str);
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_codex_project(&session, config)?;
    Ok(session.id)
}

/// Build a Copilot [`Session`](toolpath_copilot::Session) from `path`, rooted
/// at `project_dir` with a fresh session id.
///
/// **Preview / ✅ verified.** The emitted `events.jsonl` shape loads and
/// resumes in the real `copilot --resume` (copilot 1.0.67/1.0.68); see
/// `docs/agents/formats/copilot-cli/writing-compatible.md`.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn build_copilot_session(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<toolpath_copilot::Session> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let mut view = toolpath_convo::extract_conversation(path);
    // Fresh session id so we never clobber an existing Copilot session; root
    // the session at the resume directory.
    view.id = uuid::Uuid::new_v4().to_string();
    let base = view.base.get_or_insert_with(Default::default);
    base.working_dir = Some(cwd_str);

    toolpath_copilot::CopilotProjector::new()
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

/// Project `path` into a GitHub Copilot CLI session under `project_dir`
/// (writing `~/.copilot/session-state/<id>/` + a `session-store.db` row) and
/// return the freshly-generated session id. Only ever INSERTs a new id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_copilot(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    let session = build_copilot_session(path, project_dir)?;
    write_into_copilot_project(&session, config)?;
    Ok(session.id)
}

/// `path p export copilot` — project a document into a Copilot session on
/// disk (`--project`), to a file (`--output`), or to stdout (neither).
fn run_copilot(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export copilot' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(config, &input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let id = project_copilot(&path, &project_dir, config)?;
                eprintln!();
                eprintln!("Resume with:");
                eprintln!("  copilot --resume {id}");
            }
            (None, out) => {
                // No target dir: root the session at cwd and emit events.jsonl
                // to the file (or stdout) without touching ~/.copilot.
                let cwd = std::env::current_dir().context("resolve current directory")?;
                let session = build_copilot_session(&path, &cwd)?;
                let mut jsonl = String::new();
                for line in &session.lines {
                    jsonl.push_str(&serde_json::to_string(line)?);
                    jsonl.push('\n');
                }
                match out {
                    Some(out_path) => {
                        std::fs::write(&out_path, &jsonl)
                            .with_context(|| format!("write {}", out_path.display()))?;
                        eprintln!(
                            "Wrote {} events to {}",
                            session.lines.len(),
                            out_path.display()
                        );
                    }
                    None => print!("{jsonl}"),
                }
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn write_into_copilot_project(session: &toolpath_copilot::Session, config: &Config) -> Result<()> {
    let resolver = providers::copilot_resolver(config);
    let state_dir = resolver
        .session_state_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.copilot/session-state: {}", e))?;
    let sess_dir = state_dir.join(&session.id);
    std::fs::create_dir_all(&sess_dir).with_context(|| format!("create {}", sess_dir.display()))?;

    // events.jsonl
    let mut lines: Vec<String> = Vec::with_capacity(session.lines.len());
    for line in &session.lines {
        lines.push(serde_json::to_string(line)?);
    }
    let events_path = sess_dir.join("events.jsonl");
    std::fs::write(&events_path, format!("{}\n", lines.join("\n")))
        .with_context(|| format!("write {}", events_path.display()))?;

    // workspace.yaml
    std::fs::write(
        sess_dir.join("workspace.yaml"),
        copilot_workspace_yaml(session),
    )
    .with_context(|| "write workspace.yaml")?;

    // session-store.db `sessions` row — the resume picker reads this index.
    let db_path = resolver
        .session_store_db()
        .map_err(|e| anyhow::anyhow!("Cannot resolve session-store.db: {}", e))?;
    let registration = register_copilot_session(&db_path, session);

    eprintln!(
        "Exported Copilot session {} ({} events) → {}",
        session.id,
        session.lines.len(),
        events_path.display()
    );
    match registration {
        Ok(true) => eprintln!("  registered in {}", db_path.display()),
        Ok(false) => eprintln!(
            "  warning: {} not found — `copilot --resume` won't see this session",
            db_path.display()
        ),
        Err(e) => eprintln!(
            "  warning: failed to register in session-store.db: {} — `copilot --resume` may not see this session",
            e
        ),
    }
    eprintln!();
    eprintln!("⚠️  Preview: resume into Copilot CLI is unverified — the synthesized");
    eprintln!("    session may not load in `copilot --resume`.");
    eprintln!(
        "Loadable via:  path p import copilot --session {}",
        session.id
    );
    eprintln!("Resume with:   copilot --resume {}", session.id);
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn copilot_workspace_yaml(session: &toolpath_copilot::Session) -> String {
    let ws = session.workspace.clone().unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let mut out = String::new();
    out.push_str(&format!("id: {}\n", session.id));
    if let Some(cwd) = &ws.git_root {
        out.push_str(&format!("cwd: {cwd}\n"));
        out.push_str(&format!("git_root: {cwd}\n"));
    }
    if let Some(repo) = &ws.repository {
        out.push_str(&format!("repository: {repo}\n"));
        out.push_str("host_type: github\n");
    }
    if let Some(branch) = &ws.branch {
        out.push_str(&format!("branch: {branch}\n"));
    }
    out.push_str("client_name: toolpath\n");
    out.push_str("user_named: false\n");
    out.push_str(&format!("created_at: {now}\n"));
    out.push_str(&format!("updated_at: {now}\n"));
    out
}

/// Insert a row into `session-store.db`'s `sessions` table (observed schema).
/// Returns `Ok(false)` when the DB doesn't exist yet.
#[cfg(not(target_os = "emscripten"))]
fn register_copilot_session(
    db_path: &std::path::Path,
    session: &toolpath_copilot::Session,
) -> std::result::Result<bool, rusqlite::Error> {
    if !db_path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open(db_path)?;
    let ws = session.workspace.clone().unwrap_or_default();
    let cwd = ws.git_root.unwrap_or_default();
    let summary = copilot_first_user_message(session);
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO sessions
            (id, cwd, repository, host_type, branch, summary, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            session.id,
            cwd,
            ws.repository,
            "github",
            ws.branch,
            summary,
            now,
            now,
        ],
    )?;
    Ok(true)
}

#[cfg(not(target_os = "emscripten"))]
fn copilot_first_user_message(session: &toolpath_copilot::Session) -> String {
    session.first_user_text().unwrap_or_default()
}

/// Project `path` into an opencode session under `project_dir` and return
/// the resulting session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_opencode(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    let session = build_opencode_session(path, Some(project_dir))?;
    let id = session.id.clone();
    write_into_opencode_db(&session, project_dir, config)?;
    Ok(id)
}

/// Project `path` into a Pi session under `project_dir` and return the
/// resulting session id.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_pi(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd_str.clone());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;
    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id");
    }
    write_into_pi_project(&session, &cwd_str, config)?;
    Ok(session.header.id)
}

fn run_claude(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    force: bool,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, force, config);
        anyhow::bail!("'path export claude' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(config, &input)?;
        let conversation = build_claude_conversation(&path)?;
        let jsonl = serialize_jsonl(&conversation)?;

        match (project, output) {
            (Some(project_dir), None) => {
                let out_path =
                    write_into_claude_project(&conversation, &jsonl, &project_dir, force, config)?;
                let session_id = &conversation.session_id;
                eprintln!(
                    "Exported session {} ({} entries) → {}",
                    session_id,
                    conversation.preamble.len() + conversation.entries.len(),
                    out_path.display()
                );
                eprintln!();
                eprintln!("Resume with:");
                eprintln!("  cd {} && claude -r {}", project_dir.display(), session_id);
            }
            (None, Some(out_path)) => {
                std::fs::write(&out_path, &jsonl)
                    .with_context(|| format!("write {}", out_path.display()))?;
                eprintln!("Wrote {} bytes to {}", jsonl.len(), out_path.display());
            }
            (None, None) => {
                println!("{}", jsonl);
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }

        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn load_path_doc(config: &Config, input: &str) -> Result<toolpath::v1::Path> {
    let file = cache_ref(config, input)?;
    let json = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    let doc = toolpath::v1::Graph::from_json(&json)
        .map_err(|e| anyhow::anyhow!("Failed to parse toolpath document: {}", e))?;
    doc.into_single_path().ok_or_else(|| {
        anyhow::anyhow!(
            "expected a single-path graph; the source graph holds zero or multiple paths"
        )
    })
}

#[cfg(not(target_os = "emscripten"))]
fn build_claude_conversation(path: &toolpath::v1::Path) -> Result<toolpath_claude::Conversation> {
    use toolpath_convo::ConversationProjector;
    let view = toolpath_convo::extract_conversation(path);
    let projector = toolpath_claude::ClaudeProjector;
    projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

#[cfg(not(target_os = "emscripten"))]
fn serialize_jsonl(conv: &toolpath_claude::Conversation) -> Result<String> {
    let mut lines = Vec::with_capacity(conv.preamble.len() + conv.entries.len());
    for raw in &conv.preamble {
        lines.push(serde_json::to_string(raw)?);
    }
    for entry in &conv.entries {
        lines.push(serde_json::to_string(entry)?);
    }
    // Trailing newline matters: Claude Code appends to this file on resume,
    // and without it the first appended entry lands on the last line.
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

#[cfg(not(target_os = "emscripten"))]
fn write_into_claude_project(
    conv: &toolpath_claude::Conversation,
    jsonl: &str,
    project_dir: &std::path::Path,
    force: bool,
    config: &Config,
) -> Result<PathBuf> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let project_path = project_dir.to_string_lossy();

    let resolver = providers::claude_resolver(config);
    let claude_project_dir = resolver
        .project_dir(&project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Claude project dir: {}", e))?;

    std::fs::create_dir_all(&claude_project_dir)
        .with_context(|| format!("create {}", claude_project_dir.display()))?;

    let session_id = &conv.session_id;
    let out_path = claude_project_dir.join(format!("{}.jsonl", session_id));
    if !force && out_path.exists() {
        anyhow::bail!(
            "Session {} already exists in this project ({}). Resume it directly with \
             `claude -r {}`, or pass --force to overwrite the local session file.",
            session_id,
            out_path.display(),
            session_id
        );
    }
    std::fs::write(&out_path, jsonl).with_context(|| format!("write {}", out_path.display()))?;
    Ok(out_path)
}

// ── Gemini ────────────────────────────────────────────────────────────

fn run_gemini(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export gemini' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve the cwd-or-arg up front. With --output, this only
        // affects the projector's `projectHash` / `directories` payload
        // (so the emitted ChatFile carries values matching whichever
        // directory `gemini` would later be invoked from).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let project_path = project_dir.to_string_lossy().to_string();

        let conversation = build_gemini_conversation(config, &input, &project_path)?;

        match (project, output) {
            (Some(_), None) => write_into_gemini_project(&conversation, &project_path, config)?,
            (None, Some(out_path)) => write_to_output_path(&conversation, &out_path)?,
            (None, None) => write_to_stdout(&conversation)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_gemini_conversation(
    config: &Config,
    input: &str,
    project_path: &str,
) -> Result<toolpath_gemini::types::Conversation> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(config, input)?;
    let view = toolpath_convo::extract_conversation(&path);

    // The projector bakes `projectHash` and `directories` into the
    // emitted ChatFile so they match what Gemini computes when invoked
    // from the same cwd.
    let project_hash = toolpath_gemini::paths::project_hash(project_path);
    let projector = toolpath_gemini::project::GeminiProjector::new()
        .with_project_hash(project_hash)
        .with_project_path(project_path.to_string());
    let conversation = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if conversation.session_uuid.is_empty() {
        anyhow::bail!("Projected conversation has no session UUID — cannot place it on disk");
    }
    Ok(conversation)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.gemini/tmp/<slot>/chats/`.
#[cfg(not(target_os = "emscripten"))]
fn write_into_gemini_project(
    conversation: &toolpath_gemini::types::Conversation,
    project_path: &str,
    config: &Config,
) -> Result<()> {
    let resolver = providers::require_gemini_resolver(config)?;
    let chats_dir = resolver
        .chats_dir(project_path)
        .map_err(|e| anyhow::anyhow!("Cannot resolve Gemini chats dir: {}", e))?;
    std::fs::create_dir_all(&chats_dir)
        .with_context(|| format!("create {}", chats_dir.display()))?;

    // Drop a `.project_root` marker so `list_project_dirs` and any
    // tooling that walks `tmp/` can pick us up even without a
    // `projects.json` entry.
    if let Some(slot_dir) = chats_dir.parent() {
        let marker = slot_dir.join(".project_root");
        if !marker.exists() {
            let _ = std::fs::write(&marker, format!("{}\n", project_path));
        }
    }

    let main_stem = gemini_main_stem(conversation);
    let main_path = chats_dir.join(format!("{}.json", main_stem));
    let written = write_main_and_subs(conversation, &main_path)?;

    print_summary(conversation, &written, &chats_dir);
    eprintln!();
    eprintln!("Resume with:");
    eprintln!(
        "  cd {} && gemini --resume {}",
        project_path, conversation.session_uuid
    );
    Ok(())
}

/// `--output` mode: write the main chat file to the caller-specified
/// path; sub-agents (if any) land in a sibling `<session-uuid>/` dir.
#[cfg(not(target_os = "emscripten"))]
fn write_to_output_path(
    conversation: &toolpath_gemini::types::Conversation,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let written = write_main_and_subs(conversation, out_path)?;

    let parent: PathBuf = out_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    print_summary(conversation, &written, &parent);
    Ok(())
}

/// Stdout mode: pretty-print the main ChatFile JSON. Sub-agents — which
/// don't have a single-document representation in Gemini's format — are
/// dropped with a warning so the caller knows about the loss.
#[cfg(not(target_os = "emscripten"))]
fn write_to_stdout(conversation: &toolpath_gemini::types::Conversation) -> Result<()> {
    let json = serde_json::to_string_pretty(&conversation.main)?;
    println!("{}", json);
    if !conversation.sub_agents.is_empty() {
        let n = conversation.sub_agents.len();
        eprintln!(
            "warning: {} sub-agent chat{} not emitted on stdout — Gemini's format \
             stores each sub-agent in a separate file. Use --output or --project \
             to preserve them.",
            n,
            if n == 1 { "" } else { "s" },
        );
    }
    Ok(())
}

/// Write `conversation.main` to `main_path` and any sub-agents to a
/// sibling `<main_dir>/<session-uuid>/<stem>.json`. Returns every path
/// written, in order.
#[cfg(not(target_os = "emscripten"))]
fn write_main_and_subs(
    conversation: &toolpath_gemini::types::Conversation,
    main_path: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    std::fs::write(main_path, serde_json::to_string_pretty(&conversation.main)?)
        .with_context(|| format!("write {}", main_path.display()))?;
    let mut written: Vec<PathBuf> = vec![main_path.to_path_buf()];

    if !conversation.sub_agents.is_empty() {
        let parent = main_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let sub_dir = parent.join(&conversation.session_uuid);
        std::fs::create_dir_all(&sub_dir)
            .with_context(|| format!("create {}", sub_dir.display()))?;
        for (i, sub) in conversation.sub_agents.iter().enumerate() {
            let stem = if sub.session_id.is_empty() {
                format!("subagent-{}", i)
            } else {
                sub.session_id.clone()
            };
            let sub_path = sub_dir.join(format!("{}.json", stem));
            std::fs::write(&sub_path, serde_json::to_string_pretty(sub)?)
                .with_context(|| format!("write {}", sub_path.display()))?;
            written.push(sub_path);
        }
    }
    Ok(written)
}

#[cfg(not(target_os = "emscripten"))]
fn print_summary(
    conversation: &toolpath_gemini::types::Conversation,
    written: &[PathBuf],
    location: &std::path::Path,
) {
    let total_messages = conversation.main.messages.len()
        + conversation
            .sub_agents
            .iter()
            .map(|s| s.messages.len())
            .sum::<usize>();
    let sub_n = conversation.sub_agents.len();
    eprintln!(
        "Exported Gemini session {} ({} messages across main + {} sub-agent{}) → {}",
        conversation.session_uuid,
        total_messages,
        sub_n,
        if sub_n == 1 { "" } else { "s" },
        location.display()
    );
    for path in written {
        eprintln!("  wrote {}", path.display());
    }
}

/// On-disk stem for a Gemini main chat file:
/// `session-<YYYY-MM-DDTHH-MM>-<first8-of-uuid>`.
///
/// The `session-` prefix is mandatory — Gemini CLI's `--list-sessions`
/// filters on it before opening any file, so a file without it is
/// invisible to `--resume`. The short suffix matches Gemini's own
/// naming convention. Falls back to `session-<uuid>` if no timestamp is
/// available on the projected conversation.
#[cfg(not(target_os = "emscripten"))]
fn gemini_main_stem(convo: &toolpath_gemini::types::Conversation) -> String {
    let short: String = convo.session_uuid.chars().take(8).collect();
    let ts = convo
        .started_at
        .or(convo.last_activity)
        .or(convo.main.start_time)
        .or(convo.main.last_updated);
    match ts {
        Some(t) => format!("session-{}-{}", t.format("%Y-%m-%dT%H-%M"), short),
        None => format!("session-{}", convo.session_uuid),
    }
}

// ── Pi ────────────────────────────────────────────────────────────────

fn run_pi(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export pi' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve cwd up front; with --output it only feeds the
        // SessionHeader.cwd (so a downstream Pi reading the file from
        // a different directory still sees the originating cwd).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let cwd_str = project_dir.to_string_lossy().to_string();

        let session = build_pi_session(config, &input, &cwd_str)?;

        match (project, output) {
            (Some(_), None) => write_into_pi_project(&session, &cwd_str, config)?,
            (None, Some(out_path)) => write_pi_to_output_path(&session, &out_path)?,
            (None, None) => write_pi_to_stdout(&session)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_pi_session(config: &Config, input: &str, cwd: &str) -> Result<toolpath_pi::PiSession> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(config, input)?;
    let view = toolpath_convo::extract_conversation(&path);

    let projector = toolpath_pi::project::PiProjector::new().with_cwd(cwd.to_string());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if session.header.id.is_empty() {
        anyhow::bail!("Projected session has no id — cannot place it on disk");
    }
    Ok(session)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.pi/agent/sessions/--<encoded-cwd>--/<session>.jsonl`.
#[cfg(not(target_os = "emscripten"))]
fn write_into_pi_project(
    session: &toolpath_pi::PiSession,
    cwd: &str,
    config: &Config,
) -> Result<()> {
    let resolver = providers::pi_resolver(config);
    let project_dir = resolver.project_dir(cwd);
    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("create {}", project_dir.display()))?;

    let stem = pi_session_stem(session);
    let out_path = project_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_pi_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    let entry_count = session.entries.len().saturating_sub(1); // minus header
    eprintln!(
        "Exported Pi session {} ({} entries) → {}",
        session.header.id,
        entry_count,
        out_path.display()
    );
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!(
        "  path import pi --session {} --project {}",
        session.header.id, cwd
    );
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  pi --session {}", session.header.id);
    Ok(())
}

/// `--output` mode: write JSONL to the caller-specified path.
#[cfg(not(target_os = "emscripten"))]
fn write_pi_to_output_path(
    session: &toolpath_pi::PiSession,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serialize_pi_jsonl(session)?;
    std::fs::write(out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!("Wrote {} bytes to {}", bytes.len(), out_path.display());
    Ok(())
}

/// Stdout mode.
#[cfg(not(target_os = "emscripten"))]
fn write_pi_to_stdout(session: &toolpath_pi::PiSession) -> Result<()> {
    let bytes = serialize_pi_jsonl(session)?;
    print!("{}", bytes);
    Ok(())
}

/// Stem for a Pi session JSONL filename. Pi's own files use
/// `<date>_<uuid>.jsonl`; for projected sessions the session id is
/// already unique enough, so we use it directly with a safe-character
/// fallback for unusual UUIDs.
#[cfg(not(target_os = "emscripten"))]
fn pi_session_stem(session: &toolpath_pi::PiSession) -> String {
    // Strip any dashes-replaced-as-underscores oddities; for plain
    // UUIDs / short ids, the value is already filename-safe.
    session
        .header
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Serialize a `PiSession` to its JSONL on-disk shape: header line
/// followed by one entry per line. Returns the joined string with a
/// trailing newline (Pi's reader is happy with or without it; we add
/// one to match the convention real Pi sessions use).
#[cfg(not(target_os = "emscripten"))]
fn serialize_pi_jsonl(session: &toolpath_pi::PiSession) -> Result<String> {
    let mut lines: Vec<String> = Vec::with_capacity(session.entries.len());
    for entry in &session.entries {
        lines.push(serde_json::to_string(entry)?);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

// ── Codex ─────────────────────────────────────────────────────────────

fn run_codex(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export codex' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        // Resolve cwd up front; with --output it only feeds the
        // SessionMeta.cwd / TurnContext.cwd payload (so a `codex
        // resume` invocation from a different directory still sees the
        // originating cwd recorded in the rollout).
        let project_dir = match project.as_ref() {
            Some(p) => std::fs::canonicalize(p)
                .with_context(|| format!("resolve project path {}", p.display()))?,
            None => std::env::current_dir()?,
        };
        let cwd_str = project_dir.to_string_lossy().to_string();

        let session = build_codex_session(config, &input, &cwd_str)?;

        match (project, output) {
            (Some(_), None) => write_into_codex_project(&session, config)?,
            (None, Some(out_path)) => write_codex_to_output_path(&session, &out_path)?,
            (None, None) => write_codex_to_stdout(&session)?,
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_codex_session(config: &Config, input: &str, cwd: &str) -> Result<toolpath_codex::Session> {
    use toolpath_convo::ConversationProjector;

    let path = load_path_doc(config, input)?;
    let view = toolpath_convo::extract_conversation(&path);

    let projector = toolpath_codex::project::CodexProjector::new().with_cwd(cwd.to_string());
    let session = projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))?;

    if session.id.is_empty() {
        anyhow::bail!("Projected session has no id — cannot place it on disk");
    }
    Ok(session)
}

/// `--project` mode: write the resume-ready layout under
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The date partitioning
/// matches Codex's own filing convention; the timestamp prefix is
/// derived from the session's first event time.
#[cfg(not(target_os = "emscripten"))]
fn write_into_codex_project(session: &toolpath_codex::Session, config: &Config) -> Result<()> {
    let session_ts = codex_session_timestamp(session)?;
    let resolver = providers::codex_resolver(config);
    let sessions_root = resolver
        .sessions_root()
        .map_err(|e| anyhow::anyhow!("Cannot resolve Codex sessions dir: {}", e))?;

    // sessions/YYYY/MM/DD/
    let date_dir = sessions_root
        .join(session_ts.format("%Y").to_string())
        .join(session_ts.format("%m").to_string())
        .join(session_ts.format("%d").to_string());
    std::fs::create_dir_all(&date_dir).with_context(|| format!("create {}", date_dir.display()))?;

    let stem = codex_rollout_stem(session, &session_ts);
    let out_path = date_dir.join(format!("{}.jsonl", stem));
    let bytes = serialize_codex_jsonl(session)?;
    std::fs::write(&out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;

    // `codex resume` reads from state_5.sqlite, not the filesystem;
    // without a thread row the rollout file is invisible.
    let codex_dir = resolver
        .codex_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.codex dir: {}", e))?;
    let registration = register_codex_thread(&codex_dir, session, &out_path, &session_ts);

    eprintln!(
        "Exported Codex session {} ({} lines) → {}",
        session.id,
        session.lines.len(),
        out_path.display()
    );
    match registration {
        Ok(true) => eprintln!("  registered in {}/state_5.sqlite", codex_dir.display()),
        Ok(false) => eprintln!(
            "  warning: state_5.sqlite not found at {} — `codex resume` won't see this session",
            codex_dir.display()
        ),
        Err(e) => eprintln!(
            "  warning: failed to register thread in state_5.sqlite: {} — `codex resume` may not see this session",
            e
        ),
    }
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!("  path import codex --session {}", session.id);
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  codex resume {}", session.id);
    Ok(())
}

/// Schema-fragile: targets the `state_5.sqlite` shape. Returns
/// `Ok(false)` when the DB doesn't exist.
#[cfg(not(target_os = "emscripten"))]
fn register_codex_thread(
    codex_dir: &std::path::Path,
    session: &toolpath_codex::Session,
    rollout_path: &std::path::Path,
    session_ts: &chrono::DateTime<chrono::Utc>,
) -> std::result::Result<bool, rusqlite::Error> {
    let db_path = codex_dir.join("state_5.sqlite");
    if !db_path.exists() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open(&db_path)?;

    let created_at = session_ts.timestamp();
    let created_at_ms = session_ts.timestamp_millis();
    let (cwd, model_provider, cli_version) = match session.meta() {
        Some(m) => (
            m.cwd.to_string_lossy().to_string(),
            m.model_provider.clone().unwrap_or_else(|| "openai".into()),
            m.cli_version,
        ),
        None => ("/".to_string(), "openai".to_string(), String::new()),
    };
    let first_user_message = first_user_message_text(session);
    let title = first_user_message.chars().take(200).collect::<String>();
    let has_user_event: i64 = if first_user_message.is_empty() { 0 } else { 1 };
    let sandbox_policy_json = serde_json::json!({
        "type": "workspace-write",
        "writable_roots": [],
        "network_access": false,
        "exclude_tmpdir_env_var": false,
        "exclude_slash_tmp": false,
    })
    .to_string();

    conn.execute(
        "INSERT OR REPLACE INTO threads (
            id, rollout_path, created_at, updated_at, source, model_provider,
            cwd, title, sandbox_policy, approval_mode, tokens_used, has_user_event,
            archived, cli_version, first_user_message, memory_mode,
            created_at_ms, updated_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, 'cli', ?5,
            ?6, ?7, ?8, 'on-request', 0, ?9,
            0, ?10, ?11, 'enabled',
            ?12, ?13
         )",
        rusqlite::params![
            session.id,
            rollout_path.to_string_lossy(),
            created_at,
            created_at,
            model_provider,
            cwd,
            title,
            sandbox_policy_json,
            has_user_event,
            cli_version,
            first_user_message,
            created_at_ms,
            created_at_ms,
        ],
    )?;
    Ok(true)
}

#[cfg(not(target_os = "emscripten"))]
fn first_user_message_text(session: &toolpath_codex::Session) -> String {
    use toolpath_codex::types::{ResponseItem, RolloutItem};
    for line in &session.lines {
        if let RolloutItem::ResponseItem(ResponseItem::Message(m)) = line.item()
            && m.role == "user"
        {
            let t = m.text();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

/// `--output` mode: write JSONL to the caller-specified path.
#[cfg(not(target_os = "emscripten"))]
fn write_codex_to_output_path(
    session: &toolpath_codex::Session,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serialize_codex_jsonl(session)?;
    std::fs::write(out_path, &bytes).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!("Wrote {} bytes to {}", bytes.len(), out_path.display());
    Ok(())
}

/// Stdout mode.
#[cfg(not(target_os = "emscripten"))]
fn write_codex_to_stdout(session: &toolpath_codex::Session) -> Result<()> {
    let bytes = serialize_codex_jsonl(session)?;
    print!("{}", bytes);
    Ok(())
}

/// Pull the session's intended timestamp out of its session_meta line.
/// Falls back to the wall clock at projection time when the metadata
/// is missing or unparseable — Codex's directory layout requires SOME
/// date to file under.
#[cfg(not(target_os = "emscripten"))]
fn codex_session_timestamp(
    session: &toolpath_codex::Session,
) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Some(meta) = session.meta()
        && let Ok(dt) = meta.timestamp.parse::<chrono::DateTime<chrono::Utc>>()
    {
        return Ok(dt);
    }
    Ok(chrono::Utc::now())
}

/// Filename stem matching Codex's own naming:
/// `rollout-YYYY-MM-DDThh-mm-ss-<session-uuid>`.
#[cfg(not(target_os = "emscripten"))]
fn codex_rollout_stem(
    session: &toolpath_codex::Session,
    ts: &chrono::DateTime<chrono::Utc>,
) -> String {
    let stamp = ts.format("%Y-%m-%dT%H-%M-%S").to_string();
    let uuid_safe: String = session
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("rollout-{}-{}", stamp, uuid_safe)
}

/// Serialize a Codex `Session` to JSONL — one [`RolloutLine`] per line,
/// trailing newline included. This matches the on-disk shape Codex's
/// rollout recorder writes.
#[cfg(not(target_os = "emscripten"))]
fn serialize_codex_jsonl(session: &toolpath_codex::Session) -> Result<String> {
    let mut lines: Vec<String> = Vec::with_capacity(session.lines.len());
    for line in &session.lines {
        lines.push(serde_json::to_string(line)?);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

// ── Opencode ──────────────────────────────────────────────────────────

fn run_opencode(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export opencode' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(config, &input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let session = build_opencode_session(&path, Some(&project_dir))?;
                write_into_opencode_db(&session, &project_dir, config)?;
            }
            (None, Some(out_path)) => {
                let session = build_opencode_session(&path, None)?;
                write_opencode_to_output_path(&session, &out_path)?;
            }
            (None, None) => {
                let cwd = std::env::current_dir().ok();
                let session = build_opencode_session(&path, cwd.as_deref())?;
                write_opencode_to_stdout(&session)?;
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn build_opencode_session(
    path: &toolpath::v1::Path,
    project_dir: Option<&std::path::Path>,
) -> Result<toolpath_opencode::Session> {
    use toolpath_convo::ConversationProjector;
    use toolpath_opencode::project::OpencodeProjector;

    let view = toolpath_convo::extract_conversation(path);
    let mut projector = OpencodeProjector::new();
    if let Some(dir) = project_dir {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        projector = projector.with_directory(canonical);
    }
    projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

#[cfg(not(target_os = "emscripten"))]
fn write_into_opencode_db(
    session: &toolpath_opencode::Session,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<()> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;

    let resolver = providers::opencode_resolver(config);
    let db_path = resolver
        .db_path()
        .map_err(|e| anyhow::anyhow!("Cannot resolve opencode db path: {}", e))?;
    if !db_path.exists() {
        anyhow::bail!(
            "opencode database not found at {} — has opencode been run on this machine?",
            db_path.display()
        );
    }

    let mut conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("open {}", db_path.display()))?;
    let tx = conn.transaction()?;

    ensure_opencode_project(&tx, &session.project_id, &project_dir, session.time_created)?;
    insert_opencode_session(&tx, session)?;
    let mut message_count = 0_usize;
    let mut part_count = 0_usize;
    for message in &session.messages {
        insert_opencode_message(&tx, message)?;
        message_count += 1;
        for part in &message.parts {
            insert_opencode_part(&tx, part)?;
            part_count += 1;
        }
    }
    tx.commit()?;

    eprintln!(
        "Exported opencode session {} ({} messages, {} parts) → {}",
        session.id,
        message_count,
        part_count,
        db_path.display()
    );
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!("  path import opencode --session {}", session.id);
    eprintln!();
    eprintln!("Open conversation with:");
    eprintln!("  opencode --session {}", session.id);
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn ensure_opencode_project(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    worktree: &std::path::Path,
    time_now: i64,
) -> Result<()> {
    use rusqlite::OptionalExtension;
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM project WHERE id = ?1",
            rusqlite::params![project_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO project (id, worktree, vcs, name, time_created, time_updated, sandboxes)
         VALUES (?1, ?2, 'git', NULL, ?3, ?3, '[]')",
        rusqlite::params![project_id, worktree.to_string_lossy(), time_now],
    )?;
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn insert_opencode_session(
    tx: &rusqlite::Transaction<'_>,
    session: &toolpath_opencode::Session,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO session
            (id, project_id, workspace_id, parent_id, slug, directory, title,
             version, share_url, summary_additions, summary_deletions, summary_files,
             time_created, time_updated, time_compacting, time_archived)
         VALUES
            (?1, ?2, ?3, ?4, ?5, ?6, ?7,
             ?8, ?9, ?10, ?11, ?12,
             ?13, ?14, ?15, ?16)",
        rusqlite::params![
            session.id,
            session.project_id,
            session.workspace_id,
            session.parent_id,
            session.slug,
            session.directory.to_string_lossy(),
            session.title,
            session.version,
            session.share_url,
            session.summary_additions,
            session.summary_deletions,
            session.summary_files,
            session.time_created,
            session.time_updated,
            session.time_compacting,
            session.time_archived,
        ],
    )?;
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn insert_opencode_message(
    tx: &rusqlite::Transaction<'_>,
    message: &toolpath_opencode::Message,
) -> Result<()> {
    let data = serde_json::to_string(&message.data)
        .with_context(|| format!("serialize message {}", message.id))?;
    tx.execute(
        "INSERT OR REPLACE INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            message.id,
            message.session_id,
            message.time_created,
            message.time_updated,
            data,
        ],
    )?;
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn insert_opencode_part(
    tx: &rusqlite::Transaction<'_>,
    part: &toolpath_opencode::Part,
) -> Result<()> {
    let data =
        serde_json::to_string(&part.data).with_context(|| format!("serialize part {}", part.id))?;
    tx.execute(
        "INSERT OR REPLACE INTO part
            (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            part.id,
            part.message_id,
            part.session_id,
            part.time_created,
            part.time_updated,
            data,
        ],
    )?;
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_opencode_to_output_path(
    session: &toolpath_opencode::Session,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(out_path, &json).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!(
        "Wrote {} bytes to {} ({} messages)",
        json.len(),
        out_path.display(),
        session.messages.len()
    );
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_opencode_to_stdout(session: &toolpath_opencode::Session) -> Result<()> {
    let json = serde_json::to_string_pretty(session)?;
    println!("{}", json);
    Ok(())
}

// ── Cursor ────────────────────────────────────────────────────────────

fn run_cursor(
    input: String,
    project: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &Config,
) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (input, project, output, config);
        anyhow::bail!("'path export cursor' requires a native environment");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        let path = load_path_doc(config, &input)?;
        match (project, output) {
            (Some(project_dir), None) => {
                let session = build_cursor_session(&path, Some(&project_dir), config)?;
                write_into_cursor_db(&session, &project_dir, config)?;
            }
            (None, Some(out_path)) => {
                let session = build_cursor_session(&path, None, config)?;
                write_cursor_to_output_path(&session, &out_path)?;
            }
            (None, None) => {
                let cwd = std::env::current_dir().ok();
                let session = build_cursor_session(&path, cwd.as_deref(), config)?;
                write_cursor_to_stdout(&session)?;
            }
            (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        }
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn project_cursor(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<String> {
    let session = build_cursor_session(path, Some(project_dir), config)?;
    let id = session.data.composer_id.clone();
    write_into_cursor_db(&session, project_dir, config)?;
    Ok(id)
}

#[cfg(not(target_os = "emscripten"))]
fn build_cursor_session(
    path: &toolpath::v1::Path,
    project_dir: Option<&std::path::Path>,
    config: &Config,
) -> Result<toolpath_cursor::CursorSession> {
    use toolpath_convo::ConversationProjector;
    use toolpath_cursor::CursorProjector;

    let view = toolpath_convo::extract_conversation(path);
    let mut projector = CursorProjector::new();
    if let Some(dir) = project_dir {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        // Cursor filters sidebar composers by `workspaceIdentifier.id`.
        // Reuse the existing id when present, otherwise pre-create a
        // workspaceStorage entry so Cursor adopts ours on next open.
        let resolver = providers::cursor_resolver(config);
        if let Ok(ensured) =
            resolver.ensure_workspace_storage_entry(&canonical, stable_workspace_id_for)
        {
            projector = projector.with_workspace_id(ensured.id);
            if ensured.created {
                eprintln!(
                    "note: created workspaceStorage entry for {} so Cursor recognizes the projected composer",
                    canonical.display()
                );
            }
        }
        projector = projector.with_workspace_path(canonical);
    }
    projector
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

#[cfg(not(target_os = "emscripten"))]
fn stable_workspace_id_for(folder: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(folder.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

#[cfg(not(target_os = "emscripten"))]
fn write_into_cursor_db(
    session: &toolpath_cursor::CursorSession,
    project_dir: &std::path::Path,
    config: &Config,
) -> Result<()> {
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;

    let resolver = providers::cursor_resolver(config);
    let db_path = resolver
        .db_path()
        .map_err(|e| anyhow::anyhow!("Cannot resolve Cursor state.vscdb path: {}", e))?;
    if !db_path.exists() {
        anyhow::bail!(
            "Cursor state.vscdb not found at {} — has Cursor.app been run on this machine?",
            db_path.display()
        );
    }

    let mut conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("open {}", db_path.display()))?;
    let tx = conn.transaction()?;

    upsert_cursor_composer_header(&tx, session)?;
    upsert_cursor_kv(
        &tx,
        &format!(
            "{}{}",
            toolpath_cursor::reader::COMPOSER_DATA_PREFIX,
            session.data.composer_id
        ),
        &serde_json::to_string(&session.data)?,
    )?;
    tx.commit()?;
    let tx = conn.transaction()?;

    let mut bubble_count = 0_usize;
    for bubble in &session.bubbles {
        let key = format!(
            "{}{}:{}",
            toolpath_cursor::reader::BUBBLE_PREFIX,
            session.data.composer_id,
            bubble.bubble_id
        );
        upsert_cursor_kv(&tx, &key, &serde_json::to_string(bubble)?)?;
        bubble_count += 1;
    }

    let mut blob_count = 0_usize;
    for (hash, body) in &session.content_blobs {
        let composer_key = format!("{}{}", toolpath_cursor::reader::CONTENT_PREFIX, hash);
        upsert_cursor_kv(&tx, &composer_key, body)?;
        blob_count += 1;
    }

    tx.commit()?;

    eprintln!(
        "Exported Cursor composer {} ({} bubbles, {} blobs) → {}",
        session.data.composer_id,
        bubble_count,
        blob_count,
        db_path.display()
    );
    eprintln!();
    eprintln!("Loadable via:");
    eprintln!(
        "  path import cursor --session {}",
        session.data.composer_id
    );
    eprintln!();
    eprintln!("Open the workspace in Cursor.app:");
    for line in cursor_open_hints(&project_dir) {
        eprintln!("  {line}");
    }
    Ok(())
}

fn cursor_open_hints(workspace: &std::path::Path) -> Vec<String> {
    let ws = workspace.display().to_string();
    let cursor_on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .any(|d| d.join("cursor").is_file());
    if cursor_on_path {
        return vec![format!("cursor {ws}")];
    }
    #[cfg(target_os = "macos")]
    {
        vec![format!("open -a Cursor {ws}")]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![format!("xdg-open {ws}")]
    }
    #[cfg(not(unix))]
    {
        vec![format!("cursor {ws}")]
    }
}

#[cfg(not(target_os = "emscripten"))]
fn upsert_cursor_kv(tx: &rusqlite::Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Merge our new composer head into `ItemTable.composer.composerHeaders`.
/// Cursor's sidebar enumerates composers from this blob; without an
/// entry our composer wouldn't show up.
#[cfg(not(target_os = "emscripten"))]
fn upsert_cursor_composer_header(
    tx: &rusqlite::Transaction<'_>,
    session: &toolpath_cursor::CursorSession,
) -> Result<()> {
    use rusqlite::OptionalExtension;
    use toolpath_cursor::reader::HEADERS_KEY;

    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            rusqlite::params![HEADERS_KEY],
            |r| r.get(0),
        )
        .optional()?;

    let mut headers: toolpath_cursor::ComposerHeaders = match raw {
        Some(s) => serde_json::from_str(&s).unwrap_or_default(),
        None => toolpath_cursor::ComposerHeaders::default(),
    };

    let Some(new_head) = session.head.clone() else {
        anyhow::bail!(
            "Projected Cursor session has no head (composer would be invisible in Cursor.app)"
        );
    };

    let composer_id = session.data.composer_id.clone();
    headers
        .all_composers
        .retain(|h| h.composer_id != composer_id);
    headers.all_composers.insert(0, new_head);

    let payload = serde_json::to_string(&headers)?;
    tx.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
        rusqlite::params![HEADERS_KEY, payload],
    )?;
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_cursor_to_output_path(
    session: &toolpath_cursor::CursorSession,
    out_path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(out_path, &json).with_context(|| format!("write {}", out_path.display()))?;
    eprintln!(
        "Wrote {} bytes to {} ({} bubbles)",
        json.len(),
        out_path.display(),
        session.bubbles.len()
    );
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
fn write_cursor_to_stdout(session: &toolpath_cursor::CursorSession) -> Result<()> {
    let json = serde_json::to_string_pretty(session)?;
    println!("{}", json);
    Ok(())
}

// ── Pathbase ──────────────────────────────────────────────────────────

fn run_pathbase(args: PathbaseExportArgs, config: &Config) -> Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        let _ = (args, config);
        anyhow::bail!("'path export pathbase' requires a native environment with network access");
    }

    #[cfg(not(target_os = "emscripten"))]
    {
        use crate::cmd_pathbase::preflight_auth;

        let file = cache_ref(config, &args.input)?;
        let body = std::fs::read_to_string(&file)
            .with_context(|| format!("Failed to read {}", file.display()))?;
        let upload = PathbaseUploadArgs {
            url: args.url,
            anon: args.anon,
            repo: args.repo,
            name: args.name,
            public: args.public,
        };
        let base_url = resolve_upload_base_url(config, &upload);
        let needs_auth = upload.repo.is_some() || upload.public || upload.name.is_some();
        let auth = preflight_auth(config, &base_url, upload.anon, needs_auth)?;
        let summary_source = file.display().to_string();
        run_pathbase_inner(auth, base_url, upload, &body, &summary_source)
    }
}

/// Resolve the upload target URL from the CLI flag, the stored session,
/// or the default. Mirrors the order used inside `run_pathbase_inner` so
/// `cmd_share`'s pre-flight resolution agrees with the eventual upload.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn resolve_upload_base_url(config: &Config, args: &PathbaseUploadArgs) -> String {
    use crate::cmd_pathbase::{credentials_path, load_session, resolve_url};

    if let Some(u) = &args.url {
        return resolve_url(config, Some(u.clone()));
    }
    if let Ok(path) = credentials_path(config)
        && let Ok(Some(s)) = load_session(&path)
    {
        return s.url;
    }
    resolve_url(config, None)
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn run_pathbase_inner(
    auth: crate::cmd_pathbase::AuthMode,
    base_url: String,
    args: PathbaseUploadArgs,
    body: &str,
    summary_source: &str,
) -> Result<()> {
    use crate::cmd_pathbase::{AuthMode, anon_graphs_post, graphs_post, repos_post};
    use pathbase_client::types::Visibility;

    // Validate locally so we give a clean error rather than relying on
    // the server to reject malformed payloads.
    let doc = toolpath::v1::Graph::from_json(body)
        .map_err(|e| anyhow::anyhow!("Invalid toolpath document: {}", e))?;

    let (token, username) = match auth {
        AuthMode::Anon => {
            let resp = anon_graphs_post(&base_url, body)?;
            let printable = if resp.url.starts_with("http://") || resp.url.starts_with("https://") {
                resp.url.clone()
            } else if resp.url.starts_with('/') {
                format!("{base_url}{}", resp.url)
            } else {
                format!("{base_url}/{}", resp.url)
            };
            // Summary first on stderr, then the URL on stdout — the
            // share URL is the primary product, so it's the last line
            // the user (or a script piping the output) sees.
            eprintln!(
                "Uploaded {} → anon graph {} ({} bytes)",
                summary_source,
                resp.id,
                body.len()
            );
            println!("{printable}");
            return Ok(());
        }
        AuthMode::Authed { token, username } => (token, username),
    };

    let (owner, repo) = match args.repo {
        Some(spec) => (spec.owner, spec.name),
        None => {
            // Pathstash default: own the repo "pathstash" under the username
            // we resolved during preflight. Create it on demand.
            repos_post(&base_url, &token, &username, "pathstash")?;
            (username, "pathstash".to_string())
        }
    };

    let name = args.name.or_else(|| Some(derive_name(&doc)));
    let created = graphs_post(
        &base_url,
        &token,
        &owner,
        &repo,
        name.as_deref(),
        body,
        args.public,
    )?;

    // The visibility we surface is what the server actually applied,
    // not what we requested. If server-side policy ever clamps the
    // request, we render the form the graph can actually be reached at.
    let requested = if args.public {
        Visibility::Public
    } else {
        Visibility::Unlisted
    };
    if created.visibility != requested {
        eprintln!(
            "note: requested visibility={requested} but server applied visibility={}",
            created.visibility
        );
    }
    // Summary first on stderr, URL last on stdout — same ordering as
    // the anon path so the share URL is consistently the final line.
    eprintln!(
        "Uploaded {} → {}/{}/graphs/{} ({}, {} bytes)",
        summary_source,
        owner,
        repo,
        created.id,
        created.visibility,
        body.len()
    );
    println!("{}", created.url);
    Ok(())
}

/// Default display label for a graph uploaded via `export pathbase`.
///
/// Sanitize the inner id (Path id / Graph id) into `[a-z0-9_-]`-only
/// form, lower-cased — close to a URL slug, even though the server
/// addresses graphs by UUID and never reads this back from the wire.
/// Fallback (id sanitizes to empty — non-ascii, all punctuation, …):
/// hash the canonical JSON and use a short hex prefix so re-uploads
/// of the same content produce the same display label.
#[cfg(not(target_os = "emscripten"))]
fn derive_name(doc: &toolpath::v1::Graph) -> String {
    let raw = match doc.single_path() {
        Some(p) => p.path.id.as_str(),
        None => doc.graph.id.as_str(),
    };
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if !trimmed.is_empty() {
        return trimmed;
    }

    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(doc).unwrap_or_default();
    let hex = format!("{:x}", Sha256::digest(&bytes));
    format!("path-{}", &hex[..12])
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, Step, StepIdentity, StructuralChange};

    /// A `Config` rooted at a test's temp home. The export path builds
    /// every resolver from it, so no test needs to touch `$HOME`.
    fn config_with_home(home: &std::path::Path) -> Config {
        Config {
            home: Some(home.to_path_buf()),
            ..Config::default()
        }
    }

    /// A `Config` for the opencode export. The resolver reads
    /// `$XDG_DATA_HOME` internally and that read wins against the home,
    /// so the data root is injected too.
    fn config_with_opencode_home(home: &std::path::Path) -> Config {
        Config {
            home: Some(home.to_path_buf()),
            xdg_data_home: Some(home.join(".local/share")),
            ..Config::default()
        }
    }

    fn make_path_doc() -> toolpath::v1::Graph {
        let artifact_key = "agent://claude/test-session";

        let init_step = Step {
            step: StepIdentity {
                id: "step-001".to_string(),
                parents: vec![],
                actor: "tool:claude-code".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.init".to_string(),
                            extra: HashMap::new(),
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let append_step = Step {
            step: StepIdentity {
                id: "step-002".to_string(),
                parents: vec!["step-001".to_string()],
                actor: "human:user".to_string(),
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                let mut extra = HashMap::new();
                extra.insert("role".to_string(), serde_json::json!("user"));
                extra.insert("text".to_string(), serde_json::json!("Hello"));
                m.insert(
                    artifact_key.to_string(),
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };

        let path = toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head: "step-002".to_string(),
                graph_ref: None,
            },
            steps: vec![init_step, append_step],
            meta: None,
        };

        toolpath::v1::Graph::from_path(path)
    }

    #[test]
    fn claude_output_to_file() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let output_path = temp.path().join("out.jsonl");

        let doc = make_path_doc();
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        run_claude(
            input_path.to_string_lossy().to_string(),
            None,
            Some(output_path.clone()),
            false,
            &Config::default(),
        )
        .unwrap();

        let out = std::fs::read_to_string(&output_path).unwrap();
        assert!(!out.is_empty());
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }

    #[test]
    fn claude_rejects_multi_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let make_path = |id: &str| toolpath::v1::Path {
            path: PathIdentity {
                id: id.into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![Step {
                step: StepIdentity {
                    id: "s1".into(),
                    parents: vec![],
                    actor: "human:x".into(),
                    timestamp: "2024-01-01T00:00:00Z".into(),
                },
                change: HashMap::new(),
                meta: None,
            }],
            meta: None,
        };
        let multi = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p1"))),
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p2"))),
            ],
            meta: None,
        };
        std::fs::write(&input_path, serde_json::to_string(&multi).unwrap()).unwrap();

        let err = run_claude(
            input_path.to_string_lossy().to_string(),
            None,
            None,
            false,
            &Config::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("single-path graph"));
    }

    #[test]
    fn claude_invalid_json_errors() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(&input_path, "not json").unwrap();
        let err = run_claude(
            input_path.to_string_lossy().to_string(),
            None,
            None,
            false,
            &Config::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("parse") || err.to_string().contains("Failed"));
    }

    #[test]
    fn gemini_writes_resume_ready_layout() {
        // End-to-end: a path doc whose conversation.append carries a
        // single user turn projects to a flat `session-*.json` file
        // under <slot>/chats/, with `kind: "main"` and the inner
        // `sessionId` matching the source UUID. Layout matches what
        // Gemini CLI's `--resume <uuid>` needs to find the session.
        use toolpath_gemini::{GeminiConvo, PathResolver};

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Build a minimal Path doc whose artifact key encodes the
        // session UUID we want to land in `sessionId`. The shared-derive
        // wire shape is `<provider>://<session-id>`, which extract
        // parses to populate `view.id` and `view.provider_id`.
        let session_uuid = "11111111-2222-3333-4444-555555555555";
        let artifact = format!("gemini-cli://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello from export"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let doc = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        run_gemini(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
            &config_with_home(&fake_home),
        )
        .expect("export gemini");

        // The file landed at chats/session-*.json (flat, prefixed).
        let canon_project = std::fs::canonicalize(&project_dir).unwrap();
        let resolver = PathResolver::new(&fake_home);
        let chats_dir = resolver.chats_dir(canon_project.to_str().unwrap()).unwrap();

        let session_files: Vec<PathBuf> = std::fs::read_dir(&chats_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|s| s.starts_with("session-") && s.ends_with(".json"))
            })
            .collect();
        assert_eq!(session_files.len(), 1, "expected one session-*.json");

        let raw = std::fs::read_to_string(&session_files[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["sessionId"].as_str(), Some(session_uuid));
        assert_eq!(parsed["kind"].as_str(), Some("main"));

        // `--resume <uuid>` reads via inner sessionId, so verify the
        // library's resolver (which mirrors that lookup) finds it.
        let convo = GeminiConvo::with_resolver(resolver);
        let loaded = convo
            .read_conversation(canon_project.to_str().unwrap(), session_uuid)
            .expect("read back via uuid");
        assert_eq!(loaded.main.messages.len(), 1);
        assert_eq!(loaded.main.messages[0].content.text(), "Hello from export");
    }

    #[test]
    fn gemini_rejects_multi_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let make_path = |id: &str| toolpath::v1::Path {
            path: PathIdentity {
                id: id.into(),
                base: None,
                head: "s1".into(),
                graph_ref: None,
            },
            steps: vec![Step {
                step: StepIdentity {
                    id: "s1".into(),
                    parents: vec![],
                    actor: "human:x".into(),
                    timestamp: "2024-01-01T00:00:00Z".into(),
                },
                change: HashMap::new(),
                meta: None,
            }],
            meta: None,
        };
        let multi = toolpath::v1::Graph {
            graph: toolpath::v1::GraphIdentity { id: "g".into() },
            paths: vec![
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p1"))),
                toolpath::v1::PathOrRef::Path(Box::new(make_path("p2"))),
            ],
            meta: None,
        };
        std::fs::write(&input_path, serde_json::to_string(&multi).unwrap()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_gemini(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
            &Config::default(),
        )
        .expect_err("should reject multi-path graph");
        assert!(err.to_string().contains("single-path graph"));
    }

    #[test]
    fn gemini_output_to_file_writes_main_at_path() {
        // `--output FILE` writes the main ChatFile to FILE. Sub-agents
        // (none in this fixture) would land in `<FILE_PARENT>/<uuid>/`.
        use toolpath_gemini::ChatFile;

        let temp = tempfile::tempdir().unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let out_path = temp.path().join("out").join("session.json");

        let session_uuid = "33333333-4444-5555-6666-777777777777";
        let artifact = format!("gemini-cli://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello via output"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let doc = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();

        // `--output` is mutually exclusive with `--project`, so leave
        // project None. The projector still uses cwd to compute
        // `projectHash` and `directories` on the emitted ChatFile.
        run_gemini(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
            &Config::default(),
        )
        .expect("export gemini --output");

        // Parent dir is created automatically.
        assert!(out_path.exists(), "main file at output path missing");
        // The file is a valid Gemini ChatFile and carries the source UUID.
        let raw = std::fs::read_to_string(&out_path).unwrap();
        let parsed: ChatFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.session_id, session_uuid);
        assert_eq!(parsed.kind.as_deref(), Some("main"));
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].content.text(), "Hello via output");

        // No sub-agents in this fixture → no sibling UUID dir.
        assert!(!out_path.parent().unwrap().join(session_uuid).exists());
    }

    #[test]
    fn gemini_project_and_output_mutually_exclusive() {
        // clap's `conflicts_with` enforces this at parse time, but the
        // function itself also panics via `unreachable!` if both are
        // somehow passed. We can't easily exercise that without going
        // through clap; this test lives mostly as a reminder that the
        // contract holds — when both are provided, clap rejects the
        // invocation before `run_gemini` runs at all.
        use clap::Parser;
        #[derive(Parser, Debug)]
        struct Cli {
            #[command(subcommand)]
            cmd: ExportTarget,
        }
        let parsed = Cli::try_parse_from([
            "test",
            "gemini",
            "--input",
            "x",
            "--project",
            "/tmp/p",
            "--output",
            "/tmp/o.json",
        ]);
        assert!(
            parsed.is_err(),
            "clap must reject simultaneous --project and --output"
        );
    }

    #[test]
    fn pi_writes_resume_ready_layout() {
        // Build a Path doc with a single user turn, run `path export
        // pi --project DIR`, and confirm the JSONL lands at the
        // expected `~/.pi/agent/sessions/--<encoded>--/<id>.jsonl`
        // location and re-parses through Pi's own reader.
        use toolpath_pi::{PathResolver, PiConvo};

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_uuid = "pi-session-test-1";
        let artifact = format!("pi://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("Hello pi"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_pi(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
            &config_with_home(&fake_home),
        )
        .expect("export pi");

        let canon_project = std::fs::canonicalize(&project_dir).unwrap();
        let resolver = PathResolver::new().with_home(&fake_home);
        let project_dir_path = resolver.project_dir(canon_project.to_str().unwrap());
        let expected = project_dir_path.join(format!("{}.jsonl", session_uuid));
        assert!(expected.exists(), "expected JSONL at {:?}", expected);

        let convo = PiConvo::with_resolver(resolver);
        let session = convo
            .read_session(canon_project.to_str().unwrap(), session_uuid)
            .expect("Pi reader accepts our output");
        assert_eq!(session.header.id, session_uuid);
        assert_eq!(session.header.cwd, canon_project.to_string_lossy());

        let user_texts: Vec<String> = session
            .entries
            .iter()
            .filter_map(|e| match e {
                toolpath_pi::Entry::Message {
                    message: toolpath_pi::AgentMessage::User { content, .. },
                    ..
                } => match content {
                    toolpath_pi::types::MessageContent::Text(s) => Some(s.clone()),
                    toolpath_pi::types::MessageContent::Blocks(blocks) => Some(
                        blocks
                            .iter()
                            .filter_map(|b| match b {
                                toolpath_pi::ContentBlock::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                },
                _ => None,
            })
            .collect();
        assert_eq!(user_texts, vec!["Hello pi".to_string()]);
    }

    #[test]
    fn pi_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_pi(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
            &Config::default(),
        )
        .expect_err("should reject empty graph");
        assert!(err.to_string().contains("single-path"));
    }

    #[test]
    fn pi_output_to_file_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let out_path = temp.path().join("out").join("pi.jsonl");

        let session_uuid = "pi-out-test";
        let artifact = format!("pi://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hi"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-17T15:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_pi(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
            &Config::default(),
        )
        .expect("export pi --output");

        assert!(out_path.exists(), "output file missing");
        let body = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert!(lines.len() >= 2);
        assert!(lines[0].contains("\"type\":\"session\""));
        assert!(lines[1].contains("\"role\":\"user\""));

        let session = toolpath_pi::reader::read_session_from_file(&out_path)
            .expect("Pi reader accepts the JSONL");
        assert_eq!(session.header.id, session_uuid);
    }

    #[test]
    fn codex_output_to_file_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let out_path = temp.path().join("out").join("codex.jsonl");

        let session_uuid = "019dabc6-8fef-7681-a054-b5bb75fcb97d";
        let artifact = format!("codex://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hello codex"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-04-20T16:00:00Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_codex(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
            &Config::default(),
        )
        .expect("export codex --output");

        assert!(out_path.exists(), "output file missing");
        let body = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "got {} lines: {:?}", lines.len(), lines);
        assert!(lines[0].contains("\"type\":\"session_meta\""));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["payload"]["id"].as_str(),
            Some(session_uuid)
        );
        assert!(lines[1].contains("\"type\":\"turn_context\""));
        assert!(lines[2].contains("\"type\":\"response_item\""));
        assert!(lines[3].contains("\"type\":\"event_msg\""));
        assert!(lines[3].contains("\"type\":\"user_message\""));

        let reread = toolpath_codex::RolloutReader::read_session(&out_path)
            .expect("Codex reader accepts the JSONL");
        assert_eq!(reread.id, session_uuid);
        assert_eq!(reread.lines.len(), 4);
    }

    #[test]
    fn codex_writes_into_dated_sessions_dir_with_project() {
        // `--project DIR` mode writes to
        // `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` under the
        // injected home.
        use toolpath_codex::PathResolver;

        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_uuid = "019dabc6-aaaa-bbbb-cccc-ddddeeeefff0";
        let artifact = format!("codex://{}", session_uuid);
        let mut extra = HashMap::new();
        extra.insert("role".into(), serde_json::json!("user"));
        extra.insert("text".into(), serde_json::json!("hi codex via project"));
        let step = Step {
            step: StepIdentity {
                id: "step-001".into(),
                parents: vec![],
                actor: "human:alex".into(),
                timestamp: "2026-05-15T10:30:00.000Z".into(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact,
                    ArtifactChange {
                        raw: None,
                        structural: Some(StructuralChange {
                            change_type: "conversation.append".into(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        let graph = toolpath::v1::Graph::from_path(toolpath::v1::Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head: "step-001".into(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        });
        let input_path = temp.path().join("doc.json");
        std::fs::write(&input_path, graph.to_json().unwrap()).unwrap();

        run_codex(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
            &config_with_home(&fake_home),
        )
        .expect("export codex --project");

        let resolver = PathResolver::new().with_home(&fake_home);
        let dated_dir = resolver
            .sessions_root()
            .unwrap()
            .join("2026")
            .join("05")
            .join("15");
        assert!(
            dated_dir.exists(),
            "expected dated sessions dir at {}",
            dated_dir.display()
        );
        let rollout_files: Vec<PathBuf> = std::fs::read_dir(&dated_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|s| s.starts_with("rollout-") && s.ends_with(".jsonl"))
            })
            .collect();
        assert_eq!(rollout_files.len(), 1, "expected one rollout-*.jsonl");

        let name = rollout_files[0]
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap();
        assert!(name.contains("2026-05-15T10-30-00"), "got name: {}", name);
        assert!(
            name.contains("019dabc6"),
            "filename should embed the uuid prefix; got {}",
            name
        );

        let reread =
            toolpath_codex::RolloutReader::read_session(&rollout_files[0]).expect("read back");
        assert_eq!(reread.id, session_uuid);
    }

    #[test]
    fn codex_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let err = run_codex(
            input_path.to_string_lossy().to_string(),
            Some(project),
            None,
            &Config::default(),
        )
        .expect_err("should reject empty graph");
        assert!(err.to_string().contains("single-path"));
    }

    #[test]
    fn pathbase_repo_flag_requires_login() {
        // With explicit --repo (i.e. an authenticated upload), missing
        // credentials must surface the "Not logged in" error rather than
        // silently falling through to the anonymous endpoint.
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        let config = Config {
            toolpath_config_dir: Some(temp.path().to_path_buf()),
            ..Config::default()
        };
        let err = run_pathbase(
            PathbaseExportArgs {
                input: input_path.to_string_lossy().to_string(),
                url: Some("http://127.0.0.1:1".to_string()),
                anon: false,
                repo: Some(RepoSpec {
                    owner: "alex".to_string(),
                    name: "pathstash".to_string(),
                }),
                name: None,
                public: false,
            },
            &config,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Not logged in"),
            "expected `Not logged in` error, got: {err}"
        );
    }

    #[test]
    fn derive_name_uses_path_id() {
        let doc = make_path_doc();
        assert_eq!(derive_name(&doc), "test-path");
    }

    #[test]
    fn derive_name_sanitizes_non_url_safe_chars() {
        use toolpath::v1::{Graph, Path, PathIdentity};
        let doc = Graph::from_path(Path {
            path: PathIdentity {
                id: "claude/Path 42!".into(),
                base: None,
                head: "h".into(),
                graph_ref: None,
            },
            steps: vec![],
            meta: None,
        });
        assert_eq!(derive_name(&doc), "claude-path-42");
    }

    #[test]
    fn derive_name_falls_back_to_content_hash_when_id_empties_out() {
        // An id consisting entirely of non-url-safe characters sanitizes to
        // empty; we fall back to a deterministic content-hash label.
        use toolpath::v1::{Graph, Path, PathIdentity};
        let doc = Graph::from_path(Path {
            path: PathIdentity {
                id: "✨🚀🦀".into(),
                base: None,
                head: "h".into(),
                graph_ref: None,
            },
            steps: vec![],
            meta: None,
        });
        let s1 = derive_name(&doc);
        let s2 = derive_name(&doc);
        assert_eq!(s1, s2, "fallback name must be deterministic across calls");
        assert!(s1.starts_with("path-"), "got {s1}");
        assert_eq!(s1.len(), "path-".len() + 12, "got {s1}");
        assert!(
            s1.chars().skip(5).all(|c| c.is_ascii_hexdigit()),
            "got {s1}"
        );
    }

    #[test]
    fn derive_name_fallback_differs_across_documents() {
        use toolpath::v1::{Graph, Path, PathIdentity};
        let mk = |head: &str| {
            Graph::from_path(Path {
                path: PathIdentity {
                    id: "—".into(), // sanitizes to empty for both
                    base: None,
                    head: head.into(),
                    graph_ref: None,
                },
                steps: vec![],
                meta: None,
            })
        };
        assert_ne!(derive_name(&mk("a")), derive_name(&mk("b")));
    }
    #[test]
    fn opencode_output_to_file_writes_session_json() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let out_path = temp.path().join("session.json");

        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        run_opencode(
            input_path.to_string_lossy().to_string(),
            None,
            Some(out_path.clone()),
            &Config::default(),
        )
        .unwrap();

        assert!(out_path.exists());
        let body = std::fs::read_to_string(&out_path).unwrap();
        let session: toolpath_opencode::Session = serde_json::from_str(&body).unwrap();
        assert!(session.id.starts_with("ses_"));
        assert_eq!(session.messages.len(), 1);
        assert!(matches!(
            session.messages[0].data,
            toolpath_opencode::MessageData::User(_)
        ));
    }

    #[test]
    fn opencode_writes_into_db_with_project() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let project_dir = temp.path().join("myproj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let data_dir = fake_home.join(".local/share/opencode");
        std::fs::create_dir_all(&data_dir).unwrap();

        let conn = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
              icon_url text, icon_color text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let input_path = temp.path().join("input.json");
        std::fs::write(
            &input_path,
            serde_json::to_string(&make_path_doc()).unwrap(),
        )
        .unwrap();

        run_opencode(
            input_path.to_string_lossy().to_string(),
            Some(project_dir.clone()),
            None,
            &config_with_opencode_home(&fake_home),
        )
        .expect("export opencode --project");

        let conn = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        let session_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(session_count, 1);
        let message_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
            .unwrap();
        assert!(message_count >= 1);
        let part_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM part", [], |r| r.get(0))
            .unwrap();
        assert!(part_count >= 1);
    }

    #[test]
    fn opencode_rejects_non_single_path_graph() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.json");
        let empty_graph = serde_json::json!({
            "graph": { "id": "g1" },
            "paths": [],
        });
        std::fs::write(&input_path, empty_graph.to_string()).unwrap();

        let err = run_opencode(
            input_path.to_string_lossy().to_string(),
            None,
            None,
            &Config::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("single-path"));
    }

    // ── project_<harness> wrapper tests ──────────────────────────────

    /// Build a minimal `toolpath::v1::Path` with a single `conversation.append`
    /// step using the given `artifact_key` (e.g. `"claude-code://my-session"`).
    /// The projectors read `view.id` from the first `<provider>://<id>` artifact
    /// key they see, so this gives them a non-empty session id to work with.
    fn make_convo_path(artifact_key: &str) -> toolpath::v1::Path {
        let mut extra = HashMap::new();
        extra.insert("role".to_string(), serde_json::json!("user"));
        extra.insert("text".to_string(), serde_json::json!("hello"));
        let step = toolpath::v1::Step {
            step: toolpath::v1::StepIdentity {
                id: "s1".to_string(),
                parents: vec![],
                actor: "human:test".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            change: {
                let mut m = HashMap::new();
                m.insert(
                    artifact_key.to_string(),
                    toolpath::v1::ArtifactChange {
                        raw: None,
                        structural: Some(toolpath::v1::StructuralChange {
                            change_type: "conversation.append".to_string(),
                            extra,
                        }),
                    },
                );
                m
            },
            meta: None,
        };
        toolpath::v1::Path {
            path: toolpath::v1::PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head: "s1".to_string(),
                graph_ref: None,
            },
            steps: vec![step],
            meta: None,
        }
    }

    #[test]
    fn project_claude_returns_session_id_and_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        // Use a deterministic session id embedded in the artifact key.
        let session_id = "claude-wrapper-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));

        let result = project_claude(&path, &cwd, &config_with_home(&fake_home));

        let returned_id = match result.expect("project_claude should succeed") {
            ClaudeProjection::Written { session_id } => session_id,
            ClaudeProjection::AlreadyLocal { .. } => panic!("fresh project dir must be Written"),
        };
        assert_eq!(returned_id, session_id);

        let claude_projects = fake_home.join(".claude/projects");
        assert!(
            claude_projects.exists(),
            "claude projects dir missing under HOME"
        );
    }

    #[test]
    fn project_claude_never_overwrites_an_existing_session() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "claude-clobber-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));

        let config = config_with_home(&fake_home);
        let first = project_claude(&path, &cwd, &config);
        // Simulate local divergence: the session gained content after the
        // first projection.
        let session_file = claude_session_file(session_id, &cwd, &config)
            .unwrap()
            .expect("first projection must have written the session file");
        let mut contents = std::fs::read_to_string(&session_file).unwrap();
        contents.push_str("{\"local\":\"divergence\"}\n");
        std::fs::write(&session_file, &contents).unwrap();

        let second = project_claude(&path, &cwd, &config);

        assert!(matches!(
            first.expect("first projection should succeed"),
            ClaudeProjection::Written { .. }
        ));
        match second.expect("second projection should succeed") {
            ClaudeProjection::AlreadyLocal { session_id: id } => assert_eq!(id, session_id),
            ClaudeProjection::Written { .. } => panic!("existing session must not be re-projected"),
        }
        assert_eq!(
            std::fs::read_to_string(&session_file).unwrap(),
            contents,
            "existing session file must be untouched"
        );
    }

    #[test]
    fn export_claude_refuses_existing_session_without_force() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "claude-force-test-session";
        let path = make_convo_path(&format!("claude-code://{}", session_id));
        let input_path = temp.path().join("input.json");
        let doc = toolpath::v1::Graph::from_path(path);
        std::fs::write(&input_path, serde_json::to_string(&doc).unwrap()).unwrap();
        let input = input_path.to_string_lossy().to_string();

        let config = config_with_home(&fake_home);
        let first = run_claude(input.clone(), Some(cwd.clone()), None, false, &config);
        let second = run_claude(input.clone(), Some(cwd.clone()), None, false, &config);
        let forced = run_claude(input, Some(cwd.clone()), None, true, &config);

        first.expect("first export should succeed");
        let err = second.expect_err("re-export without --force must fail");
        assert!(
            err.to_string().contains("--force"),
            "unhelpful error: {err}"
        );
        forced.expect("re-export with --force should succeed");
    }

    #[test]
    fn project_gemini_returns_session_id_and_writes_chat_file() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_uuid = "11111111-2222-3333-4444-aaaaaaaaaaaa";
        let path = make_convo_path(&format!("gemini-cli://{}", session_uuid));

        let result = project_gemini(&path, &cwd, &config_with_home(&fake_home));

        let returned_id = result.expect("project_gemini should succeed");
        assert_eq!(returned_id, session_uuid);

        let gemini_tmp = fake_home.join(".gemini/tmp");
        assert!(gemini_tmp.exists(), "gemini tmp dir missing under HOME");
    }

    #[test]
    fn project_codex_returns_session_id_and_writes_rollout() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_uuid = "019dabc6-cccc-dddd-eeee-ffffffffffff";
        let path = make_convo_path(&format!("codex://{}", session_uuid));

        let result = project_codex(&path, &cwd, &config_with_home(&fake_home));

        let returned_id = result.expect("project_codex should succeed");
        assert_eq!(returned_id, session_uuid);

        let codex_sessions = fake_home.join(".codex/sessions");
        assert!(codex_sessions.exists(), "codex sessions dir missing");
    }

    #[test]
    fn project_opencode_returns_session_id_and_inserts_row() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        // Bootstrap the opencode DB (no public schema helper exists; inline
        // the same DDL used in the existing opencode_writes_into_db_with_project test).
        let data_dir = fake_home.join(".local/share/opencode");
        std::fs::create_dir_all(&data_dir).unwrap();
        let db_path = data_dir.join("opencode.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE project (
                  id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
                  icon_url text, icon_color text,
                  time_created integer NOT NULL, time_updated integer NOT NULL,
                  time_initialized integer, sandboxes text NOT NULL, commands text
                );
                CREATE TABLE session (
                  id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
                  slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
                  version text NOT NULL, share_url text,
                  summary_additions integer, summary_deletions integer,
                  summary_files integer, summary_diffs text, revert text, permission text,
                  time_created integer NOT NULL, time_updated integer NOT NULL,
                  time_compacting integer, time_archived integer, workspace_id text
                );
                CREATE TABLE message (
                  id text PRIMARY KEY, session_id text NOT NULL,
                  time_created integer NOT NULL, time_updated integer NOT NULL,
                  data text NOT NULL
                );
                CREATE TABLE part (
                  id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
                  time_created integer NOT NULL, time_updated integer NOT NULL,
                  data text NOT NULL
                );
                "#,
            )
            .unwrap();
        }

        // opencode session ids are derived from view.id via mint_session_id,
        // which adds the `ses_` prefix if not already present.
        let path = make_convo_path("opencode://ses_wrapper-test");

        let result = project_opencode(&path, &cwd, &config_with_opencode_home(&fake_home));

        let returned_id = result.expect("project_opencode should succeed");
        assert_eq!(returned_id, "ses_wrapper-test");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session WHERE id = ?1",
                [&returned_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "expected one session row with id {returned_id}");
    }

    #[test]
    fn project_pi_returns_session_id_and_writes_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let cwd = temp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let session_id = "pi-wrapper-test-session";
        let path = make_convo_path(&format!("pi://{}", session_id));

        let result = project_pi(&path, &cwd, &config_with_home(&fake_home));

        let returned_id = result.expect("project_pi should succeed");
        assert_eq!(returned_id, session_id);

        let pi_sessions = fake_home.join(".pi/agent/sessions");
        assert!(pi_sessions.exists(), "pi sessions dir missing");
    }
}
