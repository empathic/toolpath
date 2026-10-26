//! Read-only SQLite access to Cursor's global `state.vscdb`.
//!
//! Cursor uses SQLite WAL mode and writes continuously from the
//! foreground process. We open with `SQLITE_OPEN_READ_ONLY` (URI form
//! `?mode=ro`) so we never interfere with a live Cursor process; WAL
//! readers still observe a consistent snapshot as of transaction start.

use crate::error::{CursorError, Result};
use crate::types::{Bubble, ComposerData, ComposerHead, ComposerHeaders};
use rusqlite::{Connection, OpenFlags, params};
use std::path::{Path, PathBuf};

/// Prefix for the per-composer metadata row.
pub const COMPOSER_DATA_PREFIX: &str = "composerData:";
/// Prefix for the per-bubble row.
pub const BUBBLE_PREFIX: &str = "bubbleId:";
/// Prefix for content-addressed file blobs.
pub const CONTENT_PREFIX: &str = "composer.content.";
/// `cursorDiskKV` key prefix of Cursor 3.x model-history blobs; the rest of
/// the key is the SHA-256 (hex) of the blob, which is one AI SDK message.
pub const AGENT_BLOB_PREFIX: &str = "agentKv:blob:";
/// Cross-workspace composer index key (in `ItemTable`). Cursor 3.24
/// migrates it into the [`HEADERS_TABLE`] table at launch and deletes
/// the key.
pub const HEADERS_KEY: &str = "composer.composerHeaders";
/// The composer index as a table (Cursor 3.24+; `cursorDiskKV`
/// `composer.composerHeaders.migratedToTable = 1`): one row per head,
/// the head's JSON in `value`.
pub const HEADERS_TABLE: &str = "composerHeaders";

/// Thin wrapper around a rusqlite connection opened read-only against
/// Cursor's `state.vscdb`.
pub struct DbReader {
    conn: Connection,
    path: PathBuf,
}

impl DbReader {
    /// Open Cursor's global state database read-only.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(CursorError::DatabaseNotFound(path.to_path_buf()));
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every composer head: the rows of the `composerHeaders` table
    /// (Cursor 3.24+), most recent first, then any head in the legacy
    /// `ItemTable.composer.composerHeaders` blob the table does not
    /// already hold (a head written before Cursor next migrates it).
    /// Empty when neither exists.
    pub fn read_composer_headers(&self) -> Result<ComposerHeaders> {
        let mut headers = self.read_headers_table()?;
        let legacy = self.read_headers_blob()?;
        for head in legacy.all_composers {
            if !headers
                .all_composers
                .iter()
                .any(|h| h.composer_id == head.composer_id)
            {
                headers.all_composers.push(head);
            }
        }
        Ok(headers)
    }

    /// The `composerHeaders` table's heads, most recent first; empty
    /// when the table does not exist (Cursor before 3.24).
    fn read_headers_table(&self) -> Result<ComposerHeaders> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![HEADERS_TABLE],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(ComposerHeaders::default());
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT composerId, value FROM {HEADERS_TABLE} ORDER BY recency DESC, composerId"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut all_composers = Vec::new();
        for row in rows {
            let (id, value) = row?;
            let Some(value) = value else { continue };
            let head: ComposerHead =
                serde_json::from_str(&value).map_err(|e| CursorError::MalformedPayload {
                    what: format!("{HEADERS_TABLE}:{id}"),
                    detail: e.to_string(),
                })?;
            all_composers.push(head);
        }
        Ok(ComposerHeaders { all_composers })
    }

    /// `ItemTable.composer.composerHeaders`, or empty when the key is
    /// missing.
    fn read_headers_blob(&self) -> Result<ComposerHeaders> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = ?1",
                params![HEADERS_KEY],
                |r| r.get(0),
            )
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        match raw {
            Some(s) => serde_json::from_str(&s).map_err(CursorError::from),
            None => Ok(ComposerHeaders::default()),
        }
    }

    /// All composer ids known to the DB, in [`Self::read_composer_headers`]
    /// order. Composers with no
    /// bubbles are included — call [`Self::composer_has_bubbles`] to filter
    /// drafts.
    pub fn list_composer_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .read_composer_headers()?
            .all_composers
            .into_iter()
            .map(|h| h.composer_id)
            .collect())
    }

    /// Look up one header by composer id, when present.
    pub fn read_composer_head(&self, composer_id: &str) -> Result<Option<ComposerHead>> {
        Ok(self
            .read_composer_headers()?
            .all_composers
            .into_iter()
            .find(|h| h.composer_id == composer_id))
    }

    /// Read `cursorDiskKV.composerData:<composer_id>`.
    pub fn read_composer_data(&self, composer_id: &str) -> Result<Option<ComposerData>> {
        let key = format!("{COMPOSER_DATA_PREFIX}{composer_id}");
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        match raw {
            Some(s) => {
                serde_json::from_str(&s)
                    .map(Some)
                    .map_err(|e| CursorError::MalformedPayload {
                        what: format!("composerData:{composer_id}"),
                        detail: e.to_string(),
                    })
            }
            None => Ok(None),
        }
    }

    /// Read a single bubble by composer + bubble id. `None` when the
    /// row is absent (drafts referenced by header but never saved
    /// fall in this case).
    pub fn read_bubble(&self, composer_id: &str, bubble_id: &str) -> Result<Option<Bubble>> {
        let key = format!("{BUBBLE_PREFIX}{composer_id}:{bubble_id}");
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        match raw {
            Some(s) => match serde_json::from_str::<Bubble>(&s) {
                Ok(b) => Ok(Some(b)),
                Err(e) => {
                    eprintln!("Warning: bubble {composer_id}:{bubble_id} malformed: {e}; skipping");
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    /// All bubble ids stored for a composer. The order from this
    /// function is *not* meaningful — combine with the composer's
    /// `fullConversationHeadersOnly` to order them as the UI renders.
    pub fn list_bubble_ids_for_composer(&self, composer_id: &str) -> Result<Vec<String>> {
        let pattern = format!("{BUBBLE_PREFIX}{composer_id}:%");
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM cursorDiskKV WHERE key LIKE ?1")?;
        let rows = stmt
            .query_map(params![pattern], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let prefix_len = BUBBLE_PREFIX.len() + composer_id.len() + 1; // include ':'
        Ok(rows
            .into_iter()
            .filter_map(|k| {
                if k.len() > prefix_len {
                    Some(k[prefix_len..].to_string())
                } else {
                    None
                }
            })
            .collect())
    }

    /// Cheap "does this composer have any rendered bubbles?" check.
    pub fn composer_has_bubbles(&self, composer_id: &str) -> Result<bool> {
        let pattern = format!("{BUBBLE_PREFIX}{composer_id}:%");
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM cursorDiskKV WHERE key LIKE ?1",
            params![pattern],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    /// Read a content-addressed file blob. `hash` is the substring
    /// after `composer.content.`. UTF-8 decoded; non-text blobs are
    /// returned as `Some(<lossy>)`.
    pub fn read_content_blob(&self, hash: &str) -> Result<Option<String>> {
        let key = format!("{CONTENT_PREFIX}{hash}");
        let raw = self.read_value_bytes(&key)?;
        Ok(raw.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Read the raw bytes of one `cursorDiskKV` row, regardless of
    /// whether SQLite has stored it as TEXT or BLOB. (`cursor.content.*`
    /// rows we've inspected are TEXT when ASCII and BLOB otherwise;
    /// SQLite's typed accessors error on a mismatch, so we use the
    /// untyped `ValueRef` and coerce.)
    fn read_value_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM cursorDiskKV WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => {
                let vref = row.get_ref(0)?;
                let bytes = match vref {
                    rusqlite::types::ValueRef::Null => Vec::new(),
                    rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => {
                        b.to_vec()
                    }
                    rusqlite::types::ValueRef::Integer(n) => n.to_string().into_bytes(),
                    rusqlite::types::ValueRef::Real(f) => f.to_string().into_bytes(),
                };
                Ok(Some(bytes))
            }
        }
    }

    /// Compose a complete session: header (when present), data, and
    /// the bubble bodies in `fullConversationHeadersOnly` order
    /// (filtered to bubbles that actually exist on disk). Bubbles
    /// listed in the header manifest but missing from `cursorDiskKV`
    /// are silently skipped — they're rendered as placeholders in the
    /// UI but carry no content.
    pub fn load_session(&self, composer_id: &str) -> Result<crate::types::CursorSession> {
        let data = self
            .read_composer_data(composer_id)?
            .ok_or_else(|| CursorError::ComposerNotFound(composer_id.to_string()))?;
        let head = self.read_composer_head(composer_id)?;

        // Load bubbles in header order. Then sweep any leftover
        // bubbles that exist on disk but aren't in the manifest (we've
        // seen this when Cursor wrote the bubble before flushing the
        // composerData blob) and append them ordered by `createdAt`.
        let mut bubbles: Vec<Bubble> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for h in &data.full_conversation_headers_only {
            if let Some(b) = self.read_bubble(composer_id, &h.bubble_id)? {
                seen_ids.insert(b.bubble_id.clone());
                bubbles.push(b);
            }
        }
        let on_disk = self.list_bubble_ids_for_composer(composer_id)?;
        let mut leftovers: Vec<Bubble> = on_disk
            .iter()
            .filter(|bid| !seen_ids.contains(bid.as_str()))
            .filter_map(|bid| self.read_bubble(composer_id, bid).ok().flatten())
            .collect();
        leftovers.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        bubbles.extend(leftovers);

        // Load content blobs referenced by tool results.
        let mut content_blobs = std::collections::HashMap::new();
        for b in &bubbles {
            let Some(tf) = &b.tool_former_data else {
                continue;
            };
            let Ok(Some(result)) = tf.parse_result() else {
                continue;
            };
            for field in ["beforeContentId", "afterContentId"] {
                let Some(raw) = result.get(field).and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(hash) = raw.strip_prefix(CONTENT_PREFIX) else {
                    continue;
                };
                if content_blobs.contains_key(hash) {
                    continue;
                }
                if let Some(body) = self.read_content_blob(hash)? {
                    content_blobs.insert(hash.to_string(), body);
                }
            }
        }

        // Load the model-history blobs `conversationState` lists (Cursor 3.24).
        let mut agent_blobs = std::collections::HashMap::new();
        for hash in data.model_history_hashes() {
            if let Some(body) = self.read_value_bytes(&format!("{AGENT_BLOB_PREFIX}{hash}"))? {
                agent_blobs.insert(hash, String::from_utf8_lossy(&body).into_owned());
            }
        }

        Ok(crate::types::CursorSession {
            head,
            data,
            bubbles,
            content_blobs,
            agent_blobs,
            transcript_path: None,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{BUBBLE_TYPE_ASSISTANT, BUBBLE_TYPE_USER};
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    pub(crate) fn fixture_db(setup_sql: &str) -> NamedTempFile {
        let f = NamedTempFile::new().unwrap();
        let conn = Connection::open(f.path()).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
            CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
            "#,
        )
        .unwrap();
        conn.execute_batch(setup_sql).unwrap();
        f
    }

    /// One composer ("c1") with a user bubble, an assistant text
    /// bubble, and an `edit_file_v2` tool bubble. Two content blobs
    /// (empty before, populated after).
    pub(crate) const BASIC_FIXTURE: &str = r#"
        INSERT INTO ItemTable (key, value) VALUES
          ('composer.composerHeaders',
           '{"allComposers":[{"type":"head","composerId":"c1","name":"T","createdAt":1000,"lastUpdatedAt":2000,"unifiedMode":"agent","workspaceIdentifier":{"id":"ws1","uri":{"$mid":1,"fsPath":"/p","external":"file:///p","path":"/p","scheme":"file"}}}]}');
        INSERT INTO cursorDiskKV (key, value) VALUES
          ('composerData:c1',
           '{"_v":16,"composerId":"c1","name":"T","createdAt":1000,"lastUpdatedAt":2000,"isAgentic":true,"unifiedMode":"agent","agentBackend":"cursor-agent","modelConfig":{"modelName":"default","maxMode":false,"selectedModels":[{"modelId":"default","parameters":[]}]},"fullConversationHeadersOnly":[{"bubbleId":"u1","type":1},{"bubbleId":"a1","type":2},{"bubbleId":"t1","type":2}]}'),
          ('bubbleId:c1:u1',
           '{"_v":3,"type":1,"bubbleId":"u1","createdAt":"2026-06-01T00:00:00.000Z","text":"hello","conversationState":"~"}'),
          ('bubbleId:c1:a1',
           '{"_v":3,"type":2,"bubbleId":"a1","createdAt":"2026-06-01T00:00:01.000Z","text":"hi back","modelInfo":{"modelName":"claude-opus-4-7"},"tokenCount":{"inputTokens":10,"outputTokens":5}}'),
          ('bubbleId:c1:t1',
           '{"_v":3,"type":2,"bubbleId":"t1","createdAt":"2026-06-01T00:00:02.000Z","text":"","capabilityType":15,"toolFormerData":{"tool":38,"toolIndex":0,"modelCallId":"","toolCallId":"tool_a","status":"completed","name":"edit_file_v2","params":"{\"relativeWorkspacePath\":\"/p/x.rs\"}","result":"{\"beforeContentId\":\"composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\",\"afterContentId\":\"composer.content.ABC\"}"}}'),
          ('composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', ''),
          ('composer.content.ABC', 'fn main() {}');
    "#;

    /// The Cursor 3.24 shape: the head lives in the `composerHeaders`
    /// table (schema as Cursor 3.24.12 creates it), the ItemTable key
    /// is gone, and `cursorDiskKV` records the migration.
    pub(crate) const HEADERS_TABLE_3_24: &str = r#"
        CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER, recency INTEGER, checkpointAt INTEGER, subagentTypeName TEXT, value TEXT);
        CREATE INDEX idx_composerHeaders_0 ON composerHeaders (workspaceId, isSubagent, isArchived, recency);
        CREATE INDEX idx_composerHeaders_1 ON composerHeaders (recency, composerId);
        INSERT INTO composerHeaders (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, recency, value)
          SELECT json_extract(h.value, '$.composerId'), json_extract(h.value, '$.workspaceIdentifier.id'),
                 json_extract(h.value, '$.createdAt'), json_extract(h.value, '$.lastUpdatedAt'), 0, 0,
                 json_extract(h.value, '$.lastUpdatedAt'), h.value
          FROM ItemTable, json_each(ItemTable.value, '$.allComposers') AS h
          WHERE ItemTable.key = 'composer.composerHeaders';
        DELETE FROM ItemTable WHERE key = 'composer.composerHeaders';
        INSERT INTO cursorDiskKV (key, value) VALUES ('composer.composerHeaders.migratedToTable', '1');
    "#;

    #[test]
    fn reads_heads_from_the_3_24_table() {
        let f = fixture_db(&format!("{BASIC_FIXTURE}{HEADERS_TABLE_3_24}"));
        let r = DbReader::open(f.path()).unwrap();
        assert_eq!(r.list_composer_ids().unwrap(), vec!["c1"]);
        let head = r.read_composer_head("c1").unwrap().unwrap();
        assert_eq!(head.name.as_deref(), Some("T"));
        let s = r.load_session("c1").unwrap();
        assert_eq!(s.bubbles.len(), 3);
        assert_eq!(s.head.unwrap().composer_id, "c1");
    }

    #[test]
    fn table_heads_come_first_and_the_legacy_blob_fills_in() {
        // A head toolpath wrote to the blob after Cursor migrated (c2),
        // plus a stale blob copy of a migrated head (c1): table wins.
        let setup = format!(
            r#"{BASIC_FIXTURE}{HEADERS_TABLE_3_24}
            INSERT INTO ItemTable (key, value) VALUES ('composer.composerHeaders',
              '{{"allComposers":[{{"composerId":"c2","name":"new"}},{{"composerId":"c1","name":"stale"}}]}}');"#
        );
        let f = fixture_db(&setup);
        let r = DbReader::open(f.path()).unwrap();
        assert_eq!(r.list_composer_ids().unwrap(), vec!["c1", "c2"]);
        let c1 = r.read_composer_head("c1").unwrap().unwrap();
        assert_eq!(c1.name.as_deref(), Some("T"));
    }

    #[test]
    fn no_table_and_no_blob_is_empty() {
        let f = fixture_db("");
        let r = DbReader::open(f.path()).unwrap();
        assert!(r.list_composer_ids().unwrap().is_empty());
    }

    #[test]
    fn opens_and_lists_composers() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let ids = r.list_composer_ids().unwrap();
        assert_eq!(ids, vec!["c1"]);
    }

    #[test]
    fn reads_composer_data() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let cd = r.read_composer_data("c1").unwrap().unwrap();
        assert_eq!(cd.v, 16);
        assert_eq!(cd.full_conversation_headers_only.len(), 3);
        assert_eq!(cd.agent_backend.as_deref(), Some("cursor-agent"));
    }

    #[test]
    fn reads_bubble_and_misses_gracefully() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let b = r.read_bubble("c1", "u1").unwrap().unwrap();
        assert_eq!(b.kind, BUBBLE_TYPE_USER);
        assert_eq!(b.text, "hello");

        assert!(r.read_bubble("c1", "missing").unwrap().is_none());
    }

    #[test]
    fn lists_bubble_ids_and_has_bubbles() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let mut ids = r.list_bubble_ids_for_composer("c1").unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a1", "t1", "u1"]);
        assert!(r.composer_has_bubbles("c1").unwrap());
        assert!(!r.composer_has_bubbles("c-empty").unwrap());
    }

    #[test]
    fn reads_content_blob() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let empty = r
            .read_content_blob("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
            .unwrap()
            .unwrap();
        assert_eq!(empty, "");
        let body = r.read_content_blob("ABC").unwrap().unwrap();
        assert_eq!(body, "fn main() {}");
        assert!(r.read_content_blob("ZZZ").unwrap().is_none());
    }

    #[test]
    fn load_session_orders_bubbles_and_fetches_blobs() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let s = r.load_session("c1").unwrap();
        assert_eq!(s.bubbles.len(), 3);
        assert_eq!(s.bubbles[0].kind, BUBBLE_TYPE_USER);
        assert_eq!(s.bubbles[1].kind, BUBBLE_TYPE_ASSISTANT);
        assert_eq!(s.bubbles[2].kind, BUBBLE_TYPE_ASSISTANT);
        // Both before + after blobs cached.
        assert_eq!(s.content_blobs.len(), 2);
        assert_eq!(s.content_blob("ABC"), Some("fn main() {}"));
    }

    #[test]
    fn load_session_reads_the_model_history_blobs_and_its_system_text() {
        use sha2::{Digest, Sha256};
        let body = r#"{"role":"system","content":"be terse"}"#;
        let user = r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#;
        let (mut proto, mut rows) = (Vec::new(), Vec::new());
        for b in [body, user] {
            let digest: [u8; 32] = Sha256::digest(b.as_bytes()).into();
            crate::proto::put_bytes(&mut proto, 1, &digest);
            rows.push(format!(
                "('{AGENT_BLOB_PREFIX}{}', '{b}')",
                crate::project::hex_lower(&digest)
            ));
        }
        let hash = crate::project::hex_lower(&Sha256::digest(body.as_bytes()));
        let state = format!("~{}", crate::project::base64_std(&proto));
        let f = fixture_db(&format!(
            "{BASIC_FIXTURE}
            UPDATE cursorDiskKV SET value = json_set(value, '$.conversationState', '{state}')
              WHERE key = 'composerData:c1';
            INSERT INTO cursorDiskKV (key, value) VALUES {};",
            rows.join(", ")
        ));
        let s = DbReader::open(f.path())
            .unwrap()
            .load_session("c1")
            .unwrap();
        assert_eq!(s.agent_blobs.len(), 2);
        assert_eq!(s.agent_blobs.get(&hash).map(String::as_str), Some(body));
        let view = crate::session_to_view(&s);
        assert_eq!(view.turns[0].role, toolpath_convo::Role::System);
        assert_eq!(view.turns[0].text, "be terse");
        assert_eq!(
            view.turns[1].parent_id.as_deref(),
            Some(view.turns[0].id.as_str())
        );
    }

    #[test]
    fn load_session_missing_composer_errors() {
        let f = fixture_db(BASIC_FIXTURE);
        let r = DbReader::open(f.path()).unwrap();
        let err = r.load_session("nope").unwrap_err();
        assert!(matches!(err, CursorError::ComposerNotFound(_)));
    }

    #[test]
    fn malformed_composer_data_surfaces_error() {
        let setup =
            r#"INSERT INTO cursorDiskKV (key, value) VALUES ('composerData:bad', '{not json}');"#;
        let f = fixture_db(setup);
        let r = DbReader::open(f.path()).unwrap();
        let err = r.read_composer_data("bad").unwrap_err();
        assert!(matches!(err, CursorError::MalformedPayload { .. }));
    }

    #[test]
    fn load_session_picks_up_orphaned_bubble() {
        // composerData lists only one bubble; the second exists on
        // disk but isn't in fullConversationHeadersOnly. The loader
        // appends it ordered by createdAt.
        let setup = r#"
            INSERT INTO cursorDiskKV (key, value) VALUES
              ('composerData:c2', '{"_v":16,"composerId":"c2","fullConversationHeadersOnly":[{"bubbleId":"u1","type":1}]}'),
              ('bubbleId:c2:u1', '{"_v":3,"type":1,"bubbleId":"u1","createdAt":"2026-06-01T00:00:00.000Z","text":"first"}'),
              ('bubbleId:c2:u2', '{"_v":3,"type":1,"bubbleId":"u2","createdAt":"2026-06-01T00:00:01.000Z","text":"second"}');
        "#;
        let f = fixture_db(setup);
        let r = DbReader::open(f.path()).unwrap();
        let s = r.load_session("c2").unwrap();
        assert_eq!(s.bubbles.len(), 2);
        assert_eq!(s.bubbles[0].bubble_id, "u1");
        assert_eq!(s.bubbles[1].bubble_id, "u2");
    }
}
