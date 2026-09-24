//! Projection of a document into the Cursor state database.

use anyhow::{Context, Result};

pub(crate) fn project_cursor(
    path: &toolpath::v1::Path,
    project_dir: &std::path::Path,
) -> Result<String> {
    let session = build_cursor_session(path, Some(project_dir))?;
    let id = session.data.composer_id.clone();
    write_into_cursor_db(&session, project_dir)?;
    Ok(id)
}

pub(crate) fn build_cursor_session(
    path: &toolpath::v1::Path,
    project_dir: Option<&std::path::Path>,
) -> Result<toolpath_cursor::CursorSession> {
    use toolpath_convo::ConversationProjector;
    use toolpath_cursor::{CursorProjector, PathResolver};

    let view = toolpath_convo::extract_conversation(path);
    let mut projector = CursorProjector::new();
    if let Some(dir) = project_dir {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        // Cursor filters sidebar composers by `workspaceIdentifier.id`.
        // Reuse the existing id when present, otherwise pre-create a
        // workspaceStorage entry so Cursor adopts ours on next open.
        let resolver = PathResolver::new();
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

fn stable_workspace_id_for(folder: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(folder.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

pub(crate) fn write_into_cursor_db(
    session: &toolpath_cursor::CursorSession,
    project_dir: &std::path::Path,
) -> Result<()> {
    use toolpath_cursor::PathResolver;

    let project_dir = std::fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project path {}", project_dir.display()))?;

    let resolver = PathResolver::new();
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
