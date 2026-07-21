//! SQLite step index over the cache files at `$CONFIG_DIR/index.db`.
//!
//! The cache files stay canonical; this database is a disposable
//! accelerator holding every wrapped step as a row (`json` — exactly what
//! `wrap_graph` emits, compact) plus generated columns for the hot fields
//! the pushdown predicates filter on. Delete the file and the next query
//! rebuilds it lazily; bump [`SCHEMA_VERSION`] and it rebuilds itself.
//!
//! Freshness is stat-level: each indexed doc records its file's
//! `(mtime_ns, size)`, and a query serves rows only when a fresh stat
//! matches — otherwise the doc is reparsed and reindexed in place. Every
//! `write_cached` also indexes what it just wrote ([`index_written_doc`]),
//! so sync/import keep the index warm.
//!
//! Connections are per-thread (rayon workers reindex concurrently; WAL +
//! busy timeout serialize the writers), keyed by database path so tests
//! that repoint `$TOOLPATH_CONFIG_DIR` get fresh connections.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use super::plan::RowPredicate;
use crate::config::config_dir;

const INDEX_FILE: &str = "index.db";
const SCHEMA_VERSION: i64 = 1;

/// A handle to the index: just its resolved path plus the guarantee that
/// the schema exists (ensured once, on the thread that opened it).
/// Cloneable into worker closures; actual connections are per-thread.
#[derive(Debug, Clone)]
pub struct StepIndex {
    db_path: PathBuf,
}

/// Open (creating/migrating if needed) the default index. `None` when
/// disabled via `TOOLPATH_QUERY_NO_INDEX` or when the database cannot be
/// opened — the caller falls back to plain file parsing.
pub fn open_default() -> Option<StepIndex> {
    if matches!(std::env::var("TOOLPATH_QUERY_NO_INDEX"), Ok(v) if !v.is_empty() && v != "0") {
        return None;
    }
    let db_path = config_dir().ok()?.join(INDEX_FILE);
    match ensure_schema(&db_path) {
        Ok(()) => Some(StepIndex { db_path }),
        Err(e) => {
            eprintln!("warning: query index unavailable: {e:#}");
            None
        }
    }
}

impl StepIndex {
    /// Rows for a doc whose recorded stamp matches `(mtime_ns, size)`,
    /// parsed back to wrapped-step values (prefiltered by `pred` when
    /// given). `None` means stale or never indexed — reparse the file.
    pub fn fresh_steps(
        &self,
        cache_id: &str,
        mtime_ns: i64,
        size: u64,
        pred: Option<&RowPredicate>,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        self.with_conn(|conn| {
            if !stamp_matches(conn, cache_id, mtime_ns, size)? {
                return Ok(None);
            }
            let (where_sql, params) = predicate_sql(pred);
            let sql = format!("SELECT json FROM steps WHERE cache_id = ?1{where_sql} ORDER BY seq");
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&cache_id];
            for p in &params {
                bound.push(p);
            }
            let mut rows = stmt.query(&bound[..])?;
            let mut steps = Vec::new();
            while let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                steps.push(serde_json::from_str(&json).context("parse indexed step")?);
            }
            Ok(Some(steps))
        })
    }

    /// `SELECT count(*)` for a fresh doc — the fully absorbed path: no row
    /// bytes are read or parsed. `None` means stale or never indexed.
    pub fn fresh_count(
        &self,
        cache_id: &str,
        mtime_ns: i64,
        size: u64,
        pred: Option<&RowPredicate>,
    ) -> Result<Option<i64>> {
        self.with_conn(|conn| {
            if !stamp_matches(conn, cache_id, mtime_ns, size)? {
                return Ok(None);
            }
            let (where_sql, params) = predicate_sql(pred);
            let sql = format!("SELECT count(*) FROM steps WHERE cache_id = ?1{where_sql}");
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&cache_id];
            for p in &params {
                bound.push(p);
            }
            let n: i64 = stmt.query_row(&bound[..], |row| row.get(0))?;
            Ok(Some(n))
        })
    }

    /// Replace a doc's rows and stamp in one transaction.
    pub fn store(
        &self,
        cache_id: &str,
        steps: &[serde_json::Value],
        mtime_ns: i64,
        size: u64,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM steps WHERE cache_id = ?1", [cache_id])?;
            tx.execute(
                "INSERT OR REPLACE INTO documents (cache_id, mtime_ns, size) VALUES (?1, ?2, ?3)",
                rusqlite::params![cache_id, mtime_ns, size as i64],
            )?;
            {
                let mut ins = tx.prepare_cached(
                    "INSERT INTO steps (cache_id, seq, json) VALUES (?1, ?2, ?3)",
                )?;
                for (seq, step) in steps.iter().enumerate() {
                    ins.execute(rusqlite::params![
                        cache_id,
                        seq as i64,
                        serde_json::to_string(step)?
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Drop a doc's rows and stamp (evicted or unreadable doc).
    pub fn purge(&self, cache_id: &str) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("DELETE FROM steps WHERE cache_id = ?1", [cache_id])?;
            tx.execute("DELETE FROM documents WHERE cache_id = ?1", [cache_id])?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Drop rows for docs no longer in the cache listing. Called on full
    /// scans, where `keep` really is the complete cache.
    pub fn retain(&self, keep: &std::collections::HashSet<&str>) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT cache_id FROM documents")?;
            let known: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            let tx = conn.unchecked_transaction()?;
            for id in known.iter().filter(|id| !keep.contains(id.as_str())) {
                tx.execute("DELETE FROM steps WHERE cache_id = ?1", [id])?;
                tx.execute("DELETE FROM documents WHERE cache_id = ?1", [id])?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Run `f` with this thread's connection to `self.db_path`, opening
    /// one if the thread has none (or has one for a different path). The
    /// connection is taken out of the slot while `f` runs, so a nested call
    /// opens a short-lived second connection instead of panicking on the
    /// `RefCell`.
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        use std::cell::RefCell;
        thread_local! {
            static CONN: RefCell<Option<(PathBuf, Connection)>> = const { RefCell::new(None) };
        }
        CONN.with(|cell| {
            let conn = match cell.borrow_mut().take() {
                Some((p, c)) if p == self.db_path => c,
                _ => open_conn(&self.db_path)?,
            };
            let result = f(&conn);
            *cell.borrow_mut() = Some((self.db_path.clone(), conn));
            result
        })
    }
}

/// The file's identity stamp: `(mtime_ns, size)`. Any stat failure reads
/// as "unknowable", which never matches a recorded stamp.
pub fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let ns = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .try_into()
        .ok()?;
    Some((ns, meta.len()))
}

/// Write-through for `cache::write_cached`: index the doc that was just
/// written, stamped with the written file's own stat.
pub fn index_written_doc(cache_id: &str, doc: &toolpath::v1::Graph, path: &Path) -> Result<()> {
    let Some(index) = open_default() else {
        return Ok(());
    };
    let Some((mtime_ns, size)) = file_stamp(path) else {
        return Ok(());
    };
    let steps = super::wrap_document(cache_id, doc);
    index.store(cache_id, &steps, mtime_ns, size)
}

/// Purge hook for `p cache rm`.
pub fn purge_id(cache_id: &str) -> Result<()> {
    if let Some(index) = open_default() {
        index.purge(cache_id)?;
    }
    Ok(())
}

fn stamp_matches(conn: &Connection, cache_id: &str, mtime_ns: i64, size: u64) -> Result<bool> {
    let mut stmt =
        conn.prepare_cached("SELECT mtime_ns, size FROM documents WHERE cache_id = ?1")?;
    let stamp: Option<(i64, i64)> = stmt
        .query_row([cache_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    Ok(stamp == Some((mtime_ns, size as i64)))
}

/// A predicate's SQL over the generated columns, as (`" AND …"`, params).
/// Parameters continue from `?2` (`?1` is always `cache_id`). Each atom is
/// exactly its jq counterpart on wrapper-produced rows — see
/// [`RowPredicate`].
fn predicate_sql(pred: Option<&RowPredicate>) -> (String, Vec<String>) {
    let mut params = Vec::new();
    let sql = match pred {
        None => String::new(),
        Some(p) => format!(" AND {}", predicate_clause(p, &mut params)),
    };
    (sql, params)
}

fn predicate_clause(pred: &RowPredicate, params: &mut Vec<String>) -> String {
    match pred {
        RowPredicate::DeadEnd(b) => format!("dead_end = {}", i32::from(*b)),
        RowPredicate::ActorEq(s) => {
            params.push(s.clone());
            format!("actor = ?{}", params.len() + 1)
        }
        RowPredicate::ActorStartsWith(s) => {
            // `substr` counts characters, so this is exact byte-prefix
            // equality for any valid UTF-8 prefix (unlike LIKE, which is
            // ASCII-case-insensitive, or GLOB, which interprets * ? [).
            params.push(s.clone());
            format!(
                "substr(actor, 1, {}) = ?{}",
                s.chars().count(),
                params.len() + 1
            )
        }
        RowPredicate::SourceEq(s) => {
            params.push(s.clone());
            format!("source = ?{}", params.len() + 1)
        }
        RowPredicate::And(l, r) => {
            let l = predicate_clause(l, params);
            let r = predicate_clause(r, params);
            format!("({l} AND {r})")
        }
    }
}

fn open_conn(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path).with_context(|| format!("open {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

/// Create/refresh the schema. A version mismatch drops everything — the
/// index is derived state, so a rebuild is the migration.
fn ensure_schema(db_path: &Path) -> Result<()> {
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let conn = open_conn(db_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600));
    }
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        conn.execute_batch("DROP TABLE IF EXISTS steps; DROP TABLE IF EXISTS documents;")?;
    }
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS documents (
             cache_id TEXT PRIMARY KEY,
             mtime_ns INTEGER NOT NULL,
             size     INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS steps (
             cache_id TEXT NOT NULL,
             seq      INTEGER NOT NULL,
             json     TEXT NOT NULL,
             dead_end INT  GENERATED ALWAYS AS (json_extract(json, '$.dead_end')) VIRTUAL,
             actor    TEXT GENERATED ALWAYS AS (json_extract(json, '$.step.actor')) VIRTUAL,
             source   TEXT GENERATED ALWAYS AS (json_extract(json, '$.path.meta.source')) VIRTUAL,
             PRIMARY KEY (cache_id, seq)
         );
         CREATE INDEX IF NOT EXISTS steps_dead_end ON steps (cache_id, dead_end);
         CREATE INDEX IF NOT EXISTS steps_actor    ON steps (cache_id, actor);
         CREATE INDEX IF NOT EXISTS steps_source   ON steps (cache_id, source);
         PRAGMA user_version = {SCHEMA_VERSION};"
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CONFIG_DIR_ENV, TEST_ENV_LOCK};
    use serde_json::json;

    fn with_cfg<F: FnOnce(&Path) -> R, R>(f: F) -> R {
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

    fn rows() -> Vec<serde_json::Value> {
        vec![
            json!({"cache_id": "d", "step": {"id": "s1", "actor": "agent:claude"}, "dead_end": false,
                   "path": {"id": "p", "meta": {"source": "claude-code"}}}),
            json!({"cache_id": "d", "step": {"id": "s2", "actor": "human:ben"}, "dead_end": true,
                   "path": {"id": "p", "meta": {"source": "claude-code"}}}),
            json!({"cache_id": "d", "step": {"id": "s3", "actor": "agent:codex"}, "dead_end": true,
                   "path": {"id": "p", "meta": {"source": "codex"}}}),
        ]
    }

    #[test]
    fn store_and_fresh_roundtrip() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("d", &rows(), 42, 100).unwrap();

            // Matching stamp serves the rows verbatim, in order.
            let served = index.fresh_steps("d", 42, 100, None).unwrap().unwrap();
            assert_eq!(served, rows());

            // Any stamp drift reads as stale.
            assert!(index.fresh_steps("d", 43, 100, None).unwrap().is_none());
            assert!(index.fresh_steps("d", 42, 101, None).unwrap().is_none());
            assert!(index.fresh_steps("other", 42, 100, None).unwrap().is_none());
        });
    }

    #[test]
    fn predicates_filter_rows_and_counts() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("d", &rows(), 1, 1).unwrap();

            let dead = RowPredicate::DeadEnd(true);
            let served = index.fresh_steps("d", 1, 1, Some(&dead)).unwrap().unwrap();
            assert_eq!(served.len(), 2);
            assert_eq!(index.fresh_count("d", 1, 1, Some(&dead)).unwrap(), Some(2));

            let agent = RowPredicate::ActorStartsWith("agent:".into());
            assert_eq!(index.fresh_count("d", 1, 1, Some(&agent)).unwrap(), Some(2));

            let both = RowPredicate::And(Box::new(dead), Box::new(agent));
            let served = index.fresh_steps("d", 1, 1, Some(&both)).unwrap().unwrap();
            assert_eq!(served.len(), 1);
            assert_eq!(served[0]["step"]["id"], "s3");

            let src = RowPredicate::SourceEq("claude-code".into());
            assert_eq!(index.fresh_count("d", 1, 1, Some(&src)).unwrap(), Some(2));

            let eq = RowPredicate::ActorEq("human:ben".into());
            assert_eq!(index.fresh_count("d", 1, 1, Some(&eq)).unwrap(), Some(1));

            assert_eq!(index.fresh_count("d", 1, 1, None).unwrap(), Some(3));
        });
    }

    #[test]
    fn store_replaces_and_purge_forgets() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("d", &rows(), 1, 1).unwrap();
            index.store("d", &rows()[..1], 2, 2).unwrap();
            let served = index.fresh_steps("d", 2, 2, None).unwrap().unwrap();
            assert_eq!(served.len(), 1, "store replaces prior rows");

            index.purge("d").unwrap();
            assert!(index.fresh_steps("d", 2, 2, None).unwrap().is_none());
        });
    }

    #[test]
    fn retain_drops_absent_docs() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("keep", &rows(), 1, 1).unwrap();
            index.store("gone", &rows(), 1, 1).unwrap();
            let keep: std::collections::HashSet<&str> = ["keep"].into();
            index.retain(&keep).unwrap();
            assert!(index.fresh_steps("keep", 1, 1, None).unwrap().is_some());
            assert!(index.fresh_steps("gone", 1, 1, None).unwrap().is_none());
        });
    }

    #[test]
    fn empty_doc_is_fresh_with_zero_rows() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("empty", &[], 1, 1).unwrap();
            let served = index.fresh_steps("empty", 1, 1, None).unwrap().unwrap();
            assert!(served.is_empty(), "fresh but empty ≠ stale");
            assert_eq!(index.fresh_count("empty", 1, 1, None).unwrap(), Some(0));
        });
    }

    #[test]
    fn schema_version_bump_rebuilds() {
        with_cfg(|_| {
            let index = open_default().unwrap();
            index.store("d", &rows(), 1, 1).unwrap();
            index
                .with_conn(|conn| {
                    conn.pragma_update(None, "user_version", 999)?;
                    Ok(())
                })
                .unwrap();
            // Reopening sees the foreign version and starts over.
            let index = open_default().unwrap();
            assert!(index.fresh_steps("d", 1, 1, None).unwrap().is_none());
        });
    }

    #[test]
    fn no_index_env_disables() {
        with_cfg(|_| {
            unsafe {
                std::env::set_var("TOOLPATH_QUERY_NO_INDEX", "1");
            }
            let disabled = open_default();
            unsafe {
                std::env::remove_var("TOOLPATH_QUERY_NO_INDEX");
            }
            assert!(disabled.is_none());
        });
    }
}
