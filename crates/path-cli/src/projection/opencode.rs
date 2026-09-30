//! Projection of a document into rows of the opencode database.

use anyhow::{Context, Result};

/// Project `path` into an opencode session under `project_dir` and return
/// the resulting session id.
pub(crate) fn project_opencode(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let session = build_opencode_session(path, Some(project_dir))?;
    let id = session.id.clone();
    write_into_opencode_db(&session, project_dir)?;
    Ok(id)
}

pub(crate) fn build_opencode_session(
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

pub(crate) fn write_into_opencode_db(
    session: &toolpath_opencode::Session,
    project_dir: &std::path::Path,
) -> Result<()> {
    use toolpath_opencode::PathResolver;

    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;

    let resolver = PathResolver::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::make_convo_path;

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

        let _g = crate::config::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("HOME");
        let prior_xdg = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("XDG_DATA_HOME");
        }
        let result = project_opencode(&path, &cwd);
        unsafe {
            match prior_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prior_xdg {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

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
}
