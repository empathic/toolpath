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

    let mut history_count = 0_usize;
    for (hash, body) in &session.agent_blobs {
        let key = format!("{}{}", toolpath_cursor::reader::AGENT_BLOB_PREFIX, hash);
        upsert_cursor_kv(&tx, &key, body)?;
        history_count += 1;
    }

    tx.commit()?;

    eprintln!(
        "Exported Cursor composer {} ({} bubbles, {} blobs, {} history messages) → {}",
        session.data.composer_id,
        bubble_count,
        blob_count,
        history_count,
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
    upsert_cursor_composer_header_row(tx, &new_head)?;
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

/// Cursor 3.24+ keeps heads in the `composerHeaders` table and reads
/// them from there; write our head as a row too when the table exists.
/// The ItemTable blob above stays: Cursor 3.24 migrates it at launch
/// (seen in the 2026-10-15 resume matrix), so a DB from either side of
/// the migration lists the composer.
fn upsert_cursor_composer_header_row(
    tx: &rusqlite::Transaction<'_>,
    head: &toolpath_cursor::ComposerHead,
) -> Result<()> {
    use toolpath_cursor::reader::HEADERS_TABLE;

    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        rusqlite::params![HEADERS_TABLE],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let workspace_id = head.workspace_identifier.as_ref().map(|w| w.id.clone());
    let recency = head.last_updated_at.or(head.created_at);
    tx.execute(
        &format!(
            "INSERT OR REPLACE INTO {HEADERS_TABLE} \
             (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, recency, value) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)"
        ),
        rusqlite::params![
            head.composer_id,
            workspace_id,
            head.created_at,
            head.last_updated_at,
            head.is_archived,
            recency,
            serde_json::to_string(head)?,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_SCHEMA: &str = "
        CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
        CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);";
    /// As Cursor 3.24.12 creates it.
    const HEADERS_TABLE_3_24: &str = "
        CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, subagentTypeName TEXT, value TEXT);
        INSERT INTO cursorDiskKV (key, value) VALUES ('composer.composerHeaders.migratedToTable', '1');";

    fn fixture_session() -> toolpath_cursor::CursorSession {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-fixtures/cursor/convo.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// Write the fixture's head into a DB built from `schema`, then list
    /// heads back through the reader.
    fn write_head_then_list(schema: &str) -> (tempfile::NamedTempFile, Vec<String>) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let mut conn = rusqlite::Connection::open(f.path()).unwrap();
        conn.execute_batch(schema).unwrap();
        let tx = conn.transaction().unwrap();
        upsert_cursor_composer_header(&tx, &fixture_session()).unwrap();
        tx.commit().unwrap();
        drop(conn);
        let ids = toolpath_cursor::DbReader::open(f.path())
            .unwrap()
            .list_composer_ids()
            .unwrap();
        (f, ids)
    }

    #[test]
    fn head_lands_in_the_3_24_table_and_the_blob() {
        let (f, ids) = write_head_then_list(&format!("{BASE_SCHEMA}{HEADERS_TABLE_3_24}"));
        let id = fixture_session().data.composer_id;
        assert_eq!(ids, vec![id.clone()]);
        let conn = rusqlite::Connection::open(f.path()).unwrap();
        let (ws, recency, value): (String, i64, String) = conn
            .query_row(
                "SELECT workspaceId, recency, value FROM composerHeaders WHERE composerId = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(ws, "37d08546371c642419dba431e57bcbde");
        assert_eq!(recency, 1780338804591);
        let head: toolpath_cursor::ComposerHead = serde_json::from_str(&value).unwrap();
        assert_eq!(head.composer_id, id);
        // The legacy key stays: Cursor 3.24 migrates it at launch.
        let blob: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ItemTable WHERE key = 'composer.composerHeaders'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(blob, 1);
    }

    #[test]
    fn head_lands_in_the_blob_before_3_24() {
        let (_f, ids) = write_head_then_list(BASE_SCHEMA);
        assert_eq!(ids, vec![fixture_session().data.composer_id]);
    }
}
