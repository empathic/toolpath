//! Projection of a document into a GitHub Copilot CLI session.

use anyhow::{Context, Result};

/// Build a Copilot [`Session`](toolpath_copilot::Session) from `path`, rooted
/// at `project_dir` with a fresh session id.
///
/// **Preview / ✅ verified.** The emitted `events.jsonl` shape loads and
/// resumes in the real `copilot --resume` (copilot 1.0.67/1.0.68); see
/// `docs/agents/formats/copilot-cli/writing-compatible.md`.
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
pub(crate) fn project_copilot(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let session = build_copilot_session(path, project_dir)?;
    write_into_copilot_project(&session)?;
    Ok(session.id)
}

fn write_into_copilot_project(session: &toolpath_copilot::Session) -> Result<()> {
    let resolver = toolpath_copilot::PathResolver::new();
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

fn copilot_first_user_message(session: &toolpath_copilot::Session) -> String {
    session.first_user_text().unwrap_or_default()
}
