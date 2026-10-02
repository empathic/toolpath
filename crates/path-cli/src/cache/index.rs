//! The document index at `$CONFIG_DIR/`[`INDEX_FILE_NAME`]: the facts
//! a listing reads without parsing the documents.
//!
//! Every row derives from one cached document. The documents directory
//! is the source of truth: the index can be deleted at any time, and a
//! file with another schema version is emptied on open. Each document's
//! row carries the stamp of the file it was read from. A reader calls
//! [`Index::reindex_stale`] before it reads rows. The rows it reads
//! are then the rows of the files as they are.
//!
//! The cache module also records a document when it writes one
//! ([`record_written`]) and deletes the rows when it removes one
//! ([`forget_removed`]). That saves the parse; a reader does not
//! depend on it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path as FsPath;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use toolpath::v1::Graph;

use super::{CacheEntry, SessionSummary};
use crate::artifact::ArtifactType;
use crate::config::INDEX_FILE_NAME;

/// The version of `schema.sql` this build writes, kept in
/// `PRAGMA user_version`. A change to `schema.sql` needs a new value.
const SCHEMA_VERSION: i64 = 1;

/// The SHA-256 of `schema.sql` at [`SCHEMA_VERSION`]. A test compares
/// it with the file, so a change to the schema fails the test until
/// both constants have a new value.
#[cfg(test)]
const SCHEMA_SHA256: &str = "8f8678c5fcda8f93c9948e82c60ba3a037170a9c3137f2810ada6bf7c3097e46";

/// How long a connection waits on a locked index before it errors.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`set_wal_journal_mode`] waits before it tries again.
const WAL_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// How many stale documents [`Index::reindex_stale`] parses and records
/// in one transaction. A caller that stops early keeps the batches
/// that were committed.
const REINDEX_BATCH_DOCUMENTS: usize = 64;

/// The tables of the index.
const SCHEMA: &str = include_str!("schema.sql");

/// The mtime and size of a document file, as the index records them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    /// Nanoseconds since the Unix epoch.
    mtime_ns: i64,
    size: u64,
}

impl FileStamp {
    fn of(document: &CacheEntry) -> Self {
        let mtime_ns = document
            .modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|since_epoch| i64::try_from(since_epoch.as_nanos()).ok())
            .unwrap_or(0);
        Self {
            mtime_ns,
            size: document.bytes,
        }
    }
}

/// The rows of one document.
#[derive(Debug, PartialEq, Eq)]
struct DocumentRows {
    /// The cache ID prefix before the first `-`: `claude` for
    /// `claude-abc123`. An ID with no `-` is its own source.
    source: String,
    /// The `sessions` row; `None` when the document is no agent
    /// session.
    session: Option<SessionSummary>,
}

/// A document that [`Index::reindex_stale`] could not read. The index
/// holds no rows for it.
#[derive(Debug)]
pub(crate) struct UnreadableDocument {
    pub(crate) cache_id: String,
    pub(crate) error: anyhow::Error,
}

/// Selects the documents the index does not hold at their current
/// stamp: a document with no recorded stamp, or with another one.
fn select_stale<'a>(
    documents: &'a [CacheEntry],
    recorded: &BTreeMap<String, FileStamp>,
) -> Vec<&'a CacheEntry> {
    documents
        .iter()
        .filter(|document| recorded.get(&document.id) != Some(&FileStamp::of(document)))
        .collect()
}

/// Extracts the rows of `doc`, the document cached at `cache_id`. The
/// document has a session row when the source of the cache ID is an
/// agent harness and the document is one path with an agent step.
fn extract_rows(cache_id: &str, doc: &Graph) -> DocumentRows {
    let source = cache_id
        .split_once('-')
        .map_or(cache_id, |(source, _)| source);
    let session = ArtifactType::parse(source)
        .filter(|artifact_type| artifact_type.harness().is_some())
        .and_then(|harness| {
            doc.single_path()
                .and_then(|path| SessionSummary::from_path(harness, cache_id, path))
        });
    DocumentRows {
        source: source.to_string(),
        session,
    }
}

/// An open connection to the index.
pub(crate) struct Index {
    conn: Connection,
}

impl Index {
    /// Opens the index under `config_dir`, creating it when absent.
    /// A file that SQLite cannot read is an error that names the file:
    /// the index does not replace it.
    pub(crate) fn open(config_dir: &FsPath) -> Result<Self> {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("create {}", config_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(config_dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = config_dir.join(INDEX_FILE_NAME);
        let mut conn = Connection::open(&path).map_err(|e| describe_open_error(&path, e.into()))?;
        // The mode must be set before the first write creates the -wal
        // and -shm files. SQLite gives them the mode of the database
        // file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", path.display()))?;
        }
        prepare_connection(&mut conn).map_err(|e| describe_open_error(&path, e))?;
        Ok(Self { conn })
    }

    /// Brings the rows of `documents` up to date with their files.
    /// Parses each document whose file stamp is not the recorded one
    /// and replaces its rows; a document with the recorded stamp is
    /// not read. Returns the documents it could not read.
    ///
    /// The stamp of a document is the one its listing entry holds, taken
    /// before the read. A document rewritten after the listing gets
    /// rows under the earlier stamp and is parsed again on the next
    /// call.
    ///
    /// The stale documents are recorded in batches of
    /// [`REINDEX_BATCH_DOCUMENTS`], in the order of `documents`, one
    /// transaction per batch. When the call fails or the process stops,
    /// the committed batches stay and the next call parses the rest.
    pub(crate) fn reindex_stale(
        &mut self,
        documents: &[CacheEntry],
    ) -> Result<Vec<UnreadableDocument>> {
        let recorded = self.read_stamps()?;
        let stale = select_stale(documents, &recorded);
        let mut unreadable = Vec::new();
        for batch in stale.chunks(REINDEX_BATCH_DOCUMENTS) {
            // A worker must drop its parsed document before it takes the
            // next one. Memory then holds one document per worker.
            let extracted: Vec<(&CacheEntry, Result<DocumentRows>)> = batch
                .par_iter()
                .map(|document| (*document, read_rows(document)))
                .collect();

            let tx = self.conn.transaction()?;
            for (document, rows) in extracted {
                match rows {
                    Ok(rows) => write_rows(&tx, &document.id, FileStamp::of(document), &rows)?,
                    Err(error) => {
                        delete_rows(&tx, &document.id)?;
                        unreadable.push(UnreadableDocument {
                            cache_id: document.id.clone(),
                            error,
                        });
                    }
                }
            }
            tx.commit()?;
        }
        Ok(unreadable)
    }

    /// Reads the session rows of the documents `cache_ids`, in cache ID
    /// order. `dir_exists` is `false`: the index does not check
    /// directories.
    pub(crate) fn read_sessions(&self, cache_ids: &HashSet<&str>) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.cache_id, d.source, s.dir, s.title, s.started_at, s.last_activity
             FROM sessions s JOIN documents d USING (cache_id)
             ORDER BY s.cache_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next()? {
            let cache_id: String = row.get(0)?;
            if !cache_ids.contains(cache_id.as_str()) {
                continue;
            }
            let source: String = row.get(1)?;
            let harness = ArtifactType::parse(&source)
                .with_context(|| format!("session {cache_id}: unknown source {source:?}"))?;
            let started_at: Option<String> = row.get(4)?;
            let last_activity: Option<String> = row.get(5)?;
            sessions.push(SessionSummary {
                harness,
                cache_id,
                dir: row.get(2)?,
                dir_exists: false,
                title: row.get(3)?,
                started_at: started_at.as_deref().map(parse_time).transpose()?,
                last_activity: last_activity.as_deref().map(parse_time).transpose()?,
            });
        }
        Ok(sessions)
    }

    /// Reads the stamp each document was recorded with, by cache ID.
    fn read_stamps(&self) -> Result<BTreeMap<String, FileStamp>> {
        let mut stmt = self
            .conn
            .prepare("SELECT cache_id, file_mtime, file_size FROM documents")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                FileStamp {
                    mtime_ns: row.get(1)?,
                    size: row.get(2)?,
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}

/// Records `doc`, the document that `cache::write_cached` wrote to the
/// file `document` lists, in the index under `config_dir`. Replaces
/// the rows the document had.
pub(crate) fn record_written(
    config_dir: &FsPath,
    document: &CacheEntry,
    doc: &Graph,
) -> Result<()> {
    let rows = extract_rows(&document.id, doc);
    let mut index = Index::open(config_dir)?;
    let tx = index.conn.transaction()?;
    write_rows(&tx, &document.id, FileStamp::of(document), &rows)?;
    tx.commit()?;
    Ok(())
}

/// Deletes the rows of `cache_id`, the document that
/// `cache::remove_cached` removed, from the index under `config_dir`.
/// A document the index does not hold is not an error.
pub(crate) fn forget_removed(config_dir: &FsPath, cache_id: &str) -> Result<()> {
    let index = Index::open(config_dir)?;
    delete_rows(&index.conn, cache_id)?;
    Ok(())
}

/// Reads and parses the file of `document` and extracts its rows.
fn read_rows(document: &CacheEntry) -> Result<DocumentRows> {
    let json = std::fs::read_to_string(&document.path)
        .with_context(|| format!("read {}", document.path.display()))?;
    let doc =
        Graph::from_json(&json).with_context(|| format!("parse {}", document.path.display()))?;
    Ok(extract_rows(&document.id, &doc))
}

/// Replaces every row of `cache_id` with `rows`, recorded at `stamp`.
fn write_rows(
    conn: &Connection,
    cache_id: &str,
    stamp: FileStamp,
    rows: &DocumentRows,
) -> rusqlite::Result<()> {
    delete_rows(conn, cache_id)?;
    conn.execute(
        "INSERT INTO documents (cache_id, source, file_mtime, file_size, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            cache_id,
            rows.source,
            stamp.mtime_ns,
            stamp.size,
            format_time(Utc::now())
        ],
    )?;
    if let Some(session) = &rows.session {
        conn.execute(
            "INSERT INTO sessions (cache_id, dir, title, started_at, last_activity)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                cache_id,
                session.dir,
                session.title,
                session.started_at.map(format_time),
                session.last_activity.map(format_time),
            ],
        )?;
    }
    Ok(())
}

/// Deletes every row of `cache_id`. The `sessions` row goes with the
/// `documents` row by the foreign key.
fn delete_rows(conn: &Connection, cache_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM documents WHERE cache_id = ?1",
        params![cache_id],
    )?;
    Ok(())
}

/// Sets the connection options, then creates the tables when the file
/// is new and recreates them when its `user_version` is not
/// [`SCHEMA_VERSION`]. The version check runs under the write lock:
/// two openers of a new file do not both create.
fn prepare_connection(conn: &mut Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    set_wal_journal_mode(conn)?;
    conn.pragma_update(None, "foreign_keys", true)?;

    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        let tables: Vec<String> = tx
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for table in tables {
            tx.execute_batch(&format!("DROP TABLE \"{}\"", table.replace('"', "\"\"")))?;
        }
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

/// Puts the file in WAL journal mode, and errors when SQLite keeps
/// another mode.
///
/// SQLite does not wait the busy timeout on this statement: when
/// another connection holds the file, it returns busy at once. Two
/// openers of a new file meet here, so a busy result is tried again
/// until [`BUSY_TIMEOUT`] has passed.
fn set_wal_journal_mode(conn: &Connection) -> Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        let mode = conn
            .pragma_update_and_check(None, "journal_mode", "wal", |row| row.get::<_, String>(0));
        match mode {
            Ok(mode) if mode == "wal" => return Ok(()),
            Ok(mode) => bail!("the journal mode is {mode:?}, and the index needs \"wal\""),
            Err(error)
                if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(WAL_RETRY_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Describes a failure to open the index at `path`. When SQLite
/// cannot read the file, the message says that the file is safe to
/// delete.
fn describe_open_error(path: &FsPath, error: anyhow::Error) -> anyhow::Error {
    let unreadable = matches!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    );
    if unreadable {
        error.context(format!(
            "the document index {} is not a SQLite database that this build can read. \
             It holds only derived data: delete the file and run the command again",
            path.display()
        ))
    } else {
        error.context(format!("open the document index {}", path.display()))
    }
}

fn format_time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .with_context(|| format!("parse time {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{TURNS, derive};
    use super::*;
    use std::path::PathBuf;

    /// The document of an agent session titled `title`.
    fn session_document(title: &str) -> Graph {
        Graph::from_path(derive(Some(title), &TURNS))
    }

    /// The listing entry of the file at `path`, as `list_cached` gives
    /// it.
    fn list_entry(cache_id: &str, path: PathBuf) -> CacheEntry {
        let meta = std::fs::metadata(&path).unwrap();
        CacheEntry {
            id: cache_id.to_string(),
            path,
            bytes: meta.len(),
            modified: meta.modified().unwrap(),
        }
    }

    /// Writes `text` as the document file of `cache_id` in `dir`.
    fn write_file(dir: &FsPath, cache_id: &str, text: &str) -> CacheEntry {
        let path = dir.join(format!("{cache_id}.json"));
        std::fs::write(&path, text).unwrap();
        list_entry(cache_id, path)
    }

    fn write_document(dir: &FsPath, cache_id: &str, doc: &Graph) -> CacheEntry {
        write_file(dir, cache_id, &doc.to_json_pretty().unwrap())
    }

    /// A listing entry with stamp `n` and no file.
    fn stamped_entry(cache_id: &str, n: u64) -> CacheEntry {
        CacheEntry {
            id: cache_id.to_string(),
            path: PathBuf::new(),
            bytes: n,
            modified: UNIX_EPOCH + Duration::from_nanos(n),
        }
    }

    fn titles(sessions: &[SessionSummary]) -> Vec<&str> {
        sessions.iter().map(|s| s.title.as_str()).collect()
    }

    fn ids(documents: &[CacheEntry]) -> HashSet<&str> {
        documents
            .iter()
            .map(|document| document.id.as_str())
            .collect()
    }

    /// Runs `f` with the config directory pinned to a fresh temporary
    /// directory, which `f` receives. For a test of the cache
    /// functions, which read the config directory from the
    /// environment.
    fn with_config_dir<F: FnOnce(&FsPath) -> R, R>(f: F) -> R {
        use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};
        let temp = tempfile::tempdir().unwrap();
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(CONFIG_DIR_ENV, temp.path());
        }
        let result = f(temp.path());
        unsafe {
            std::env::remove_var(CONFIG_DIR_ENV);
        }
        result
    }

    #[test]
    fn write_cached_records_the_document_at_the_stamp_of_its_file() {
        with_config_dir(|dir| {
            let written =
                crate::cache::write_cached("claude-x", &session_document("t"), false).unwrap();
            assert!(written.index_error.is_none());

            let documents = crate::cache::list_cached().unwrap();
            let index = Index::open(dir).unwrap();
            assert_eq!(
                index.read_stamps().unwrap().get("claude-x"),
                Some(&FileStamp::of(&documents[0]))
            );
            assert_eq!(
                titles(&index.read_sessions(&ids(&documents)).unwrap()),
                ["t"]
            );
        });
    }

    #[test]
    fn remove_cached_deletes_the_rows() {
        with_config_dir(|dir| {
            let _ = crate::cache::write_cached("claude-x", &session_document("t"), false).unwrap();
            let index_error = crate::cache::remove_cached("claude-x").unwrap();
            assert!(index_error.is_none());
            assert!(Index::open(dir).unwrap().read_stamps().unwrap().is_empty());
        });
    }

    #[test]
    fn a_write_and_a_remove_succeed_and_return_the_failure_of_an_unreadable_index() {
        with_config_dir(|dir| {
            std::fs::write(dir.join(INDEX_FILE_NAME), "not a database. ".repeat(16)).unwrap();

            let written =
                crate::cache::write_cached("claude-x", &session_document("t"), false).unwrap();
            assert!(written.index_error.is_some());
            assert!(written.path.exists());

            let index_error = crate::cache::remove_cached("claude-x").unwrap();
            assert!(index_error.is_some());
            assert!(!written.path.exists());
        });
    }

    #[test]
    fn reindex_stale_replaces_rows_recorded_under_the_stamp_of_another_file() {
        let dir = tempfile::tempdir().unwrap();
        let first_doc = session_document("first");
        let first = write_document(dir.path(), "claude-x", &first_doc);
        let second = [write_document(
            dir.path(),
            "claude-x",
            &session_document("second title"),
        )];
        // A writer whose file was replaced records its rows last.
        record_written(dir.path(), &first, &first_doc).unwrap();

        let mut index = Index::open(dir.path()).unwrap();
        assert_eq!(
            titles(&index.read_sessions(&ids(&second)).unwrap()),
            ["first"]
        );
        index.reindex_stale(&second).unwrap();
        assert_eq!(
            titles(&index.read_sessions(&ids(&second)).unwrap()),
            ["second title"]
        );
    }

    #[test]
    fn forget_removed_accepts_a_document_the_index_does_not_hold() {
        let dir = tempfile::tempdir().unwrap();
        forget_removed(dir.path(), "claude-never").unwrap();
    }

    #[test]
    fn select_stale_takes_a_document_with_no_stamp_or_another_stamp() {
        let documents = [
            stamped_entry("claude-current", 1),
            stamped_entry("claude-new", 2),
            stamped_entry("claude-changed", 3),
        ];
        let recorded = BTreeMap::from([
            (
                "claude-current".to_string(),
                FileStamp::of(&stamped_entry("claude-current", 1)),
            ),
            (
                "claude-changed".to_string(),
                FileStamp::of(&stamped_entry("claude-changed", 4)),
            ),
        ]);
        let stale: Vec<&str> = select_stale(&documents, &recorded)
            .into_iter()
            .map(|document| document.id.as_str())
            .collect();
        assert_eq!(stale, ["claude-new", "claude-changed"]);
    }

    #[test]
    fn extract_rows_gives_a_session_document_a_session_row() {
        let rows = extract_rows("claude-x", &session_document("Parser fix"));
        assert_eq!(
            rows,
            DocumentRows {
                source: "claude".to_string(),
                session: Some(SessionSummary {
                    harness: ArtifactType::Claude,
                    cache_id: "claude-x".to_string(),
                    dir: "/work/project".to_string(),
                    dir_exists: false,
                    title: "Parser fix".to_string(),
                    started_at: Some("2026-09-23T10:00:00Z".parse().unwrap()),
                    last_activity: Some("2026-09-23T10:30:00Z".parse().unwrap()),
                }),
            }
        );
    }

    #[test]
    fn extract_rows_gives_no_session_row_without_an_agent_step_or_a_harness() {
        let no_agent = Graph::from_path(derive(None, &TURNS[..2]));
        assert_eq!(extract_rows("claude-x", &no_agent).session, None);

        let doc = session_document("t");
        assert_eq!(
            extract_rows("git-main", &doc),
            DocumentRows {
                source: "git".to_string(),
                session: None,
            }
        );
        assert_eq!(
            extract_rows("pathbase-o-r-uuid", &doc),
            DocumentRows {
                source: "pathbase".to_string(),
                session: None,
            }
        );
    }

    #[test]
    fn reindex_stale_records_a_document_the_index_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let doc = session_document("Parser fix");
        let documents = [write_document(dir.path(), "claude-x", &doc)];
        let mut index = Index::open(dir.path()).unwrap();
        assert!(index.read_sessions(&ids(&documents)).unwrap().is_empty());

        let unreadable = index.reindex_stale(&documents).unwrap();
        assert!(unreadable.is_empty());
        assert_eq!(
            index.read_sessions(&ids(&documents)).unwrap(),
            [extract_rows("claude-x", &doc).session.unwrap()]
        );
    }

    #[test]
    fn reindex_stale_does_not_read_a_document_with_the_recorded_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let documents = [write_document(
            dir.path(),
            "claude-x",
            &session_document("first"),
        )];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&documents).unwrap();

        // A second parse would put the title of the file back.
        index
            .conn
            .execute("UPDATE sessions SET title = 'from the index'", [])
            .unwrap();
        index.reindex_stale(&documents).unwrap();
        assert_eq!(
            titles(&index.read_sessions(&ids(&documents)).unwrap()),
            ["from the index"]
        );
    }

    #[test]
    fn reindex_stale_parses_a_rewritten_document_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = [write_document(
            dir.path(),
            "claude-x",
            &session_document("first"),
        )];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&first).unwrap();

        let second = [write_document(
            dir.path(),
            "claude-x",
            &session_document("second title"),
        )];
        index.reindex_stale(&second).unwrap();
        assert_eq!(
            titles(&index.read_sessions(&ids(&second)).unwrap()),
            ["second title"]
        );
    }

    #[test]
    fn reindex_stale_reports_an_unreadable_document_and_deletes_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let good = [write_document(
            dir.path(),
            "claude-x",
            &session_document("first"),
        )];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&good).unwrap();

        let bad = [write_file(dir.path(), "claude-x", "not a document")];
        let unreadable = index.reindex_stale(&bad).unwrap();
        assert_eq!(unreadable.len(), 1);
        assert_eq!(unreadable[0].cache_id, "claude-x");
        assert!(index.read_sessions(&ids(&bad)).unwrap().is_empty());
        assert!(index.read_stamps().unwrap().is_empty());
    }

    #[test]
    fn reindex_stale_keeps_the_batches_before_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let doc = session_document("t");
        let mut documents: Vec<CacheEntry> = (0..=REINDEX_BATCH_DOCUMENTS)
            .map(|n| write_document(dir.path(), &format!("claude-{n:03}"), &doc))
            .collect();
        // SQLite cannot hold this size, so the write of the last
        // document fails. It is alone in the second batch.
        documents[REINDEX_BATCH_DOCUMENTS].bytes = u64::MAX;

        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&documents).unwrap_err();
        assert_eq!(index.read_stamps().unwrap().len(), REINDEX_BATCH_DOCUMENTS);
    }

    #[test]
    fn read_sessions_gives_only_the_named_documents_in_cache_id_order() {
        let dir = tempfile::tempdir().unwrap();
        let documents = [
            write_document(dir.path(), "claude-b", &session_document("b")),
            write_document(dir.path(), "claude-c", &session_document("c")),
            write_document(dir.path(), "claude-a", &session_document("a")),
        ];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&documents).unwrap();

        let named = HashSet::from(["claude-b", "claude-a"]);
        assert_eq!(titles(&index.read_sessions(&named).unwrap()), ["a", "b"]);
    }

    #[test]
    fn open_keeps_the_rows_of_a_file_at_the_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let documents = [write_document(
            dir.path(),
            "claude-x",
            &session_document("t"),
        )];
        Index::open(dir.path())
            .unwrap()
            .reindex_stale(&documents)
            .unwrap();
        let index = Index::open(dir.path()).unwrap();
        assert_eq!(index.read_sessions(&ids(&documents)).unwrap().len(), 1);
    }

    #[test]
    fn a_change_to_the_schema_needs_a_new_schema_version() {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(SCHEMA.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            digest, SCHEMA_SHA256,
            "schema.sql changed. Give SCHEMA_VERSION a new value, then set SCHEMA_SHA256 to the \
             digest on the left"
        );
    }

    #[test]
    fn open_empties_a_file_of_another_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let documents = [write_document(
            dir.path(),
            "claude-x",
            &session_document("t"),
        )];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&documents).unwrap();
        index
            .conn
            .execute_batch("CREATE TABLE stray (x INTEGER); PRAGMA user_version = 99;")
            .unwrap();
        drop(index);

        let index = Index::open(dir.path()).unwrap();
        assert!(index.read_sessions(&ids(&documents)).unwrap().is_empty());
        assert!(index.read_stamps().unwrap().is_empty());
        let version: i64 = index
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let stray: i64 = index
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'stray'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stray, 0);
    }

    #[test]
    fn open_fails_on_a_file_that_is_no_database_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE_NAME);
        let text = "not a database, and longer than one SQLite header: ".repeat(4);
        std::fs::write(&path, &text).unwrap();

        let error = Index::open(dir.path()).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("delete the file"), "{message}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn concurrent_openers_of_a_new_file_all_record() {
        let dir = tempfile::tempdir().unwrap();
        let documents: Vec<CacheEntry> = (0..8)
            .map(|n| {
                write_document(
                    dir.path(),
                    &format!("claude-{n}"),
                    &session_document(&format!("t{n}")),
                )
            })
            .collect();
        std::thread::scope(|s| {
            for document in &documents {
                let dir = dir.path();
                s.spawn(move || {
                    let mut index = Index::open(dir).unwrap();
                    index.reindex_stale(std::slice::from_ref(document)).unwrap();
                });
            }
        });
        let index = Index::open(dir.path()).unwrap();
        assert_eq!(index.read_sessions(&ids(&documents)).unwrap().len(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn the_index_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let documents = [write_document(
            dir.path(),
            "claude-x",
            &session_document("t"),
        )];
        let mut index = Index::open(dir.path()).unwrap();
        index.reindex_stale(&documents).unwrap();
        for name in [
            INDEX_FILE_NAME.to_string(),
            format!("{INDEX_FILE_NAME}-wal"),
        ] {
            let mode = std::fs::metadata(dir.path().join(&name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
    }
}
