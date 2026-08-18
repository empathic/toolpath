//! Higher-level filesystem + DB operations over [`PathResolver`]
//! and [`DbReader`].

use crate::error::{ConvoError, Result};
use crate::paths::PathResolver;
use crate::reader::DbReader;
use crate::types::{MessageData, PartData, Project, Session, SessionMetadata};
use chrono::{TimeZone, Utc};
use std::path::PathBuf;

/// The facade most consumers want. Wraps a `PathResolver` and lazily
/// opens the database on demand.
pub struct ConvoIO {
    resolver: PathResolver,
}

impl ConvoIO {
    /// Roots at `home`, so the data directory is
    /// `<home>/.local/share/opencode`.
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self::with_resolver(PathResolver::new(home))
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.db_exists()
    }

    pub fn db_path(&self) -> PathBuf {
        self.resolver.db_path()
    }

    fn open_db(&self) -> Result<DbReader> {
        DbReader::open(self.resolver.db_path())
    }

    /// List every project in the database.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let db = self.open_db()?;
        db.list_projects()
    }

    /// List every session, optionally filtered by project id.
    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<Session>> {
        let db = self.open_db()?;
        db.list_sessions(project_id)
    }

    /// Lightweight per-session metadata. One DB query per session
    /// header + one aggregated query per session for the message
    /// count — cheap enough for interactive listing.
    pub fn list_session_metadata(&self, project_id: Option<&str>) -> Result<Vec<SessionMetadata>> {
        let db = self.open_db()?;
        let sessions = db.list_sessions(project_id)?;
        let mut out = Vec::with_capacity(sessions.len());
        for s in sessions {
            let full = match db.load_session(&s.id) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Warning: skipping {}: {}", s.id, e);
                    continue;
                }
            };
            out.push(SessionMetadata {
                id: full.id.clone(),
                project_id: full.project_id.clone(),
                directory: full.directory.clone(),
                title: full.title.clone(),
                version: full.version.clone(),
                started_at: Utc.timestamp_millis_opt(full.time_created).single(),
                last_activity: Utc.timestamp_millis_opt(full.time_updated).single(),
                message_count: full.messages.len(),
                first_user_message: full.first_user_text(),
                summary_additions: full.summary_additions,
                summary_deletions: full.summary_deletions,
                summary_files: full.summary_files,
            });
        }
        Ok(out)
    }

    /// Load one session fully (messages + parts attached).
    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        let db = self.open_db()?;
        db.load_session(session_id)
    }

    /// Return one session's metadata by id.
    pub fn read_metadata(&self, session_id: &str) -> Result<SessionMetadata> {
        let db = self.open_db()?;
        let s = db.load_session(session_id)?;
        Ok(SessionMetadata {
            id: s.id.clone(),
            project_id: s.project_id.clone(),
            directory: s.directory.clone(),
            title: s.title.clone(),
            version: s.version.clone(),
            started_at: Utc.timestamp_millis_opt(s.time_created).single(),
            last_activity: Utc.timestamp_millis_opt(s.time_updated).single(),
            message_count: s.messages.len(),
            first_user_message: s.first_user_text(),
            summary_additions: s.summary_additions,
            summary_deletions: s.summary_deletions,
            summary_files: s.summary_files,
        })
    }

    pub fn session_exists(&self, session_id: &str) -> Result<bool> {
        match self.open_db() {
            Ok(db) => Ok(db.get_session(session_id)?.is_some()),
            Err(ConvoError::DatabaseNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Discriminator counts across all parts in a session — useful
    /// for quick inspection / debugging.
    pub fn part_type_counts(
        &self,
        session_id: &str,
    ) -> Result<std::collections::BTreeMap<String, usize>> {
        let db = self.open_db()?;
        let parts = db.list_parts_for_session(session_id)?;
        let mut counts = std::collections::BTreeMap::new();
        for p in parts {
            let key = match p.data {
                PartData::Unknown => "unknown".to_string(),
                ref d => d.kind().to_string(),
            };
            *counts.entry(key).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// Total message-role counts. Handy for sanity checks.
    pub fn role_counts(
        &self,
        session_id: &str,
    ) -> Result<std::collections::BTreeMap<String, usize>> {
        let db = self.open_db()?;
        let msgs = db.list_messages_raw(session_id)?;
        let mut counts = std::collections::BTreeMap::new();
        for m in msgs {
            let k = match m.data {
                MessageData::User(_) => "user",
                MessageData::Assistant(_) => "assistant",
                MessageData::Other => "other",
            };
            *counts.entry(k.to_string()).or_insert(0) += 1;
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let conn = Connection::open(data.join("opencode.db")).unwrap();
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
            INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
              VALUES ('p1','/tmp/p',1000,2000,'[]');
            INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
              VALUES ('ses_a','p1','slug','/tmp/p','Greeting','1.0.0',1000,2000);
            INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
              ('m1','ses_a',1001,1001,'{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"p","modelID":"m"}}'),
              ('m2','ses_a',1002,1002,'{"parentID":"m1","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/tmp/p","root":"/tmp/p"},"cost":0.0,"tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"m","providerID":"p","time":{"created":1002},"finish":"stop"}');
            INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
              ('p1x','m1','ses_a',1001,1001,'{"type":"text","text":"hi"}'),
              ('p2a','m2','ses_a',1002,1002,'{"type":"step-start","snapshot":"abc"}'),
              ('p2b','m2','ses_a',1002,1002,'{"type":"text","text":"hello back"}'),
              ('p2c','m2','ses_a',1002,1002,'{"type":"step-finish","reason":"stop","snapshot":"abc","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0}');
        "#,
        )
        .unwrap();
        drop(conn);
        let resolver = PathResolver::new(temp.path()).with_data_dir(&data);
        (temp, ConvoIO::with_resolver(resolver))
    }

    #[test]
    fn new_roots_at_home() {
        let temp = TempDir::new().unwrap();
        let io = ConvoIO::new(temp.path());
        assert_eq!(
            io.db_path(),
            temp.path().join(".local/share/opencode/opencode.db")
        );
    }

    #[test]
    fn lists_projects_and_sessions() {
        let (_t, io) = fixture();
        assert_eq!(io.list_projects().unwrap().len(), 1);
        assert_eq!(io.list_sessions(None).unwrap().len(), 1);
        assert_eq!(io.list_sessions(Some("p1")).unwrap().len(), 1);
        assert_eq!(io.list_sessions(Some("nope")).unwrap().len(), 0);
    }

    #[test]
    fn session_metadata_populated() {
        let (_t, io) = fixture();
        let metas = io.list_session_metadata(None).unwrap();
        assert_eq!(metas.len(), 1);
        let m = &metas[0];
        assert_eq!(m.id, "ses_a");
        assert_eq!(m.message_count, 2);
        assert_eq!(m.first_user_message.as_deref(), Some("hi"));
    }

    #[test]
    fn part_type_counts_report() {
        let (_t, io) = fixture();
        let counts = io.part_type_counts("ses_a").unwrap();
        assert_eq!(counts["text"], 2);
        assert_eq!(counts["step-start"], 1);
        assert_eq!(counts["step-finish"], 1);
    }

    #[test]
    fn role_counts_report() {
        let (_t, io) = fixture();
        let c = io.role_counts("ses_a").unwrap();
        assert_eq!(c["user"], 1);
        assert_eq!(c["assistant"], 1);
    }

    #[test]
    fn session_exists_true_false() {
        let (_t, io) = fixture();
        assert!(io.session_exists("ses_a").unwrap());
        assert!(!io.session_exists("ses_missing").unwrap());
    }

    #[test]
    fn read_session_and_metadata() {
        let (_t, io) = fixture();
        let s = io.read_session("ses_a").unwrap();
        assert_eq!(s.messages.len(), 2);
        let m = io.read_metadata("ses_a").unwrap();
        assert_eq!(m.message_count, 2);
    }
}
