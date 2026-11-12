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
    build_copilot_session_with_id(path, project_dir, None)
}

/// [`build_copilot_session`] with the session id given; `None` derives it from the
/// document ([`toolpath_copilot::project::session_id_for`]: the same document, the same id).
fn build_copilot_session_with_id(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
    id: Option<String>,
) -> Result<toolpath_copilot::Session> {
    use toolpath_convo::ConversationProjector;
    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;
    let cwd_str = project_dir.to_string_lossy().to_string();

    let mut view = toolpath_convo::extract_conversation(path);
    // The id comes from the document, so exporting it twice names one session; root
    // the session at the resume directory.
    view.id = id.unwrap_or_else(|| toolpath_copilot::project::session_id_for(&view.id));
    let base = view.base.get_or_insert_with(Default::default);
    base.working_dir = Some(cwd_str);

    toolpath_copilot::CopilotProjector::new()
        .project(&view)
        .map_err(|e| anyhow::anyhow!("Projection failed: {}", e))
}

/// Project `path` into a GitHub Copilot CLI session under `project_dir`
/// (writing `~/.copilot/session-state/<id>/` + a `session-store.db` row) and
/// return the session id. The id is derived from the document, so projecting it again
/// names the same session. Never clobbers: a session already there with identical content is
/// reused (nothing is written); one that has diverged is left alone and the document is written
/// under a fresh random id, with a note.
pub(crate) fn project_copilot(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let mut session = build_copilot_session(path, project_dir)?;
    match existing_session(&session)? {
        Existing::Absent => {}
        Existing::Identical => {
            eprintln!(
                "Copilot session {} already exists with this content; reusing it.",
                session.id
            );
            return Ok(session.id);
        }
        Existing::Diverged => {
            let fresh = uuid::Uuid::new_v4().to_string();
            eprintln!(
                "Copilot session {} already exists with different content; leaving it alone \
                 and writing this one as {fresh}.",
                session.id
            );
            session = build_copilot_session_with_id(path, project_dir, Some(fresh))?;
        }
    }
    write_into_copilot_project(&session)?;
    Ok(session.id)
}

/// What Copilot already has under a session's id.
#[derive(Debug, PartialEq, Eq)]
enum Existing {
    Absent,
    Identical,
    Diverged,
}

/// The `events.jsonl` text [`write_into_copilot_project`] writes for `session`.
fn events_text(session: &toolpath_copilot::Session) -> Result<String> {
    let mut lines: Vec<String> = Vec::with_capacity(session.lines.len());
    for line in &session.lines {
        lines.push(serde_json::to_string(line)?);
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// Compare what is on disk (state directory) and in `session-store.db` for `session.id` with
/// what would be written. A session-store row with no state directory counts as diverged: the
/// id is taken by something we did not write.
fn existing_session(session: &toolpath_copilot::Session) -> Result<Existing> {
    let resolver = toolpath_copilot::PathResolver::new();
    let state_dir = resolver
        .session_state_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.copilot/session-state: {}", e))?;
    let events = state_dir.join(&session.id).join("events.jsonl");
    if events.exists() {
        let on_disk = std::fs::read_to_string(&events)
            .with_context(|| format!("read {}", events.display()))?;
        return Ok(if on_disk == events_text(session)? {
            Existing::Identical
        } else {
            Existing::Diverged
        });
    }
    let registered = match resolver.session_store_db() {
        Ok(db) if db.exists() => rusqlite::Connection::open(&db)
            .and_then(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    [&session.id],
                    |r| r.get::<_, i64>(0),
                )
            })
            .map(|n| n > 0)
            .unwrap_or(false),
        _ => false,
    };
    Ok(if registered {
        Existing::Diverged
    } else {
        Existing::Absent
    })
}

fn write_into_copilot_project(session: &toolpath_copilot::Session) -> Result<()> {
    let resolver = toolpath_copilot::PathResolver::new();
    let state_dir = resolver
        .session_state_dir()
        .map_err(|e| anyhow::anyhow!("Cannot resolve ~/.copilot/session-state: {}", e))?;
    let sess_dir = state_dir.join(&session.id);
    std::fs::create_dir_all(&sess_dir).with_context(|| format!("create {}", sess_dir.display()))?;

    // events.jsonl
    let events_path = sess_dir.join("events.jsonl");
    std::fs::write(&events_path, events_text(session)?)
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

/// The instant, in the shape Copilot CLI writes and validates in
/// `workspace.yaml` (`2026-10-15T13:50:59.997Z`). The chrono default
/// (`…59.997483+00:00`) fails its datetime check: copilot 1.0.61 logs
/// `Failed to load workspace … created_at: Invalid datetime` and resumes
/// without the workspace state.
fn copilot_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn copilot_workspace_yaml(session: &toolpath_copilot::Session) -> String {
    let ws = session.workspace.clone().unwrap_or_default();
    let now = copilot_now();
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
    let now = copilot_now();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `body` with `COPILOT_HOME` at a fresh temp dir and a project dir.
    fn with_copilot_home(body: impl FnOnce(&std::path::Path, &std::path::Path)) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("copilot");
        std::fs::create_dir_all(&home).unwrap();
        let project = temp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("COPILOT_HOME");
        unsafe { std::env::set_var("COPILOT_HOME", &home) };
        body(&home, &project);
        unsafe {
            match prior {
                Some(v) => std::env::set_var("COPILOT_HOME", v),
                None => std::env::remove_var("COPILOT_HOME"),
            }
        }
    }

    #[test]
    fn project_copilot_names_one_session_for_one_document_and_reuses_it() {
        use crate::projection::test_support::make_convo_path;
        with_copilot_home(|home, project| {
            let path = make_convo_path("claude-code://copilot-reuse-test");
            let first = project_copilot(&path, project).unwrap();
            let events = home.join("session-state").join(&first).join("events.jsonl");
            let before = std::fs::read_to_string(&events).unwrap();
            let second = project_copilot(&path, project).unwrap();
            assert_eq!(first, second, "the same document names the same session");
            assert_eq!(std::fs::read_to_string(&events).unwrap(), before);
            assert_eq!(
                std::fs::read_dir(home.join("session-state"))
                    .unwrap()
                    .count(),
                1,
                "no second session directory"
            );
        });
    }

    #[test]
    fn project_copilot_never_overwrites_a_diverged_session() {
        use crate::projection::test_support::make_convo_path;
        with_copilot_home(|home, project| {
            let path = make_convo_path("claude-code://copilot-clobber-test");
            let first = project_copilot(&path, project).unwrap();
            let events = home.join("session-state").join(&first).join("events.jsonl");
            let mut text = std::fs::read_to_string(&events).unwrap();
            text.push_str("{\"local\":\"divergence\"}\n");
            std::fs::write(&events, &text).unwrap();

            let second = project_copilot(&path, project).unwrap();
            assert_ne!(first, second, "a diverged session is not reused");
            assert_eq!(
                std::fs::read_to_string(&events).unwrap(),
                text,
                "the diverged session is untouched"
            );
            assert!(
                home.join("session-state")
                    .join(&second)
                    .join("events.jsonl")
                    .exists(),
                "the document is written under the fresh id"
            );
        });
    }

    #[test]
    fn workspace_timestamps_are_millisecond_utc_z() {
        // `2026-10-15T13:50:59.997Z`: 24 chars, `Z`, three fractional digits —
        // the only shape copilot's workspace.yaml validator accepts.
        let now = copilot_now();
        assert_eq!(now.len(), 24, "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(&now[19..20], ".", "{now}");
        assert!(now[20..23].chars().all(|c| c.is_ascii_digit()), "{now}");
        assert!(!now.contains("+00:00"), "{now}");
    }
}
