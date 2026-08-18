//! Higher-level filesystem + DB operations over [`PathResolver`] and
//! [`DbReader`].

use crate::error::{CursorError, Result};
use crate::paths::{self, PathResolver};
use crate::reader::DbReader;
use crate::types::{ComposerHead, ComposerHeaders, CursorSession, CursorSessionMetadata};
use std::path::PathBuf;

/// The facade most consumers want. Wraps a [`PathResolver`] and lazily
/// opens the SQLite database on demand.
pub struct CursorIO {
    resolver: PathResolver,
}

impl CursorIO {
    /// Reads Cursor state under `home`, so the Anysphere directory is
    /// `<home>/.cursor`.
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

    /// Read `composer.composerHeaders` verbatim.
    pub fn read_composer_headers(&self) -> Result<ComposerHeaders> {
        self.open_db()?.read_composer_headers()
    }

    /// All composer ids known to the database, including drafts. Use
    /// [`Self::list_composers`] when you only want sessions that have at
    /// least one bubble on disk.
    pub fn list_composer_ids(&self) -> Result<Vec<String>> {
        self.open_db()?.list_composer_ids()
    }

    /// Composer headers paired to whether they have any rendered
    /// bubbles. Empty drafts (`has_bubbles == false`) are commonly
    /// dropped by callers that want a session list.
    pub fn list_composers(&self) -> Result<Vec<ComposerListing>> {
        let db = self.open_db()?;
        let headers = db.read_composer_headers()?;
        let mut out = Vec::with_capacity(headers.all_composers.len());
        for head in headers.all_composers {
            let has = db.composer_has_bubbles(&head.composer_id).unwrap_or(false);
            out.push(ComposerListing {
                has_bubbles: has,
                head,
            });
        }
        Ok(out)
    }

    /// Lightweight per-composer metadata for every composer that has
    /// at least one bubble on disk.
    pub fn list_session_metadata(&self) -> Result<Vec<CursorSessionMetadata>> {
        let db = self.open_db()?;
        let headers = db.read_composer_headers()?;
        let mut out = Vec::new();
        for head in headers.all_composers {
            if !db.composer_has_bubbles(&head.composer_id).unwrap_or(false) {
                continue;
            }
            match db.load_session(&head.composer_id) {
                Ok(s) => out.push(to_metadata(&s)),
                Err(e) => {
                    eprintln!("Warning: skipping composer {}: {}", head.composer_id, e);
                }
            }
        }
        Ok(out)
    }

    /// Lightweight metadata for one composer.
    pub fn read_session_metadata(&self, composer_id: &str) -> Result<CursorSessionMetadata> {
        let s = self.read_session(composer_id)?;
        Ok(to_metadata(&s))
    }

    /// Whether the composer exists in the database (either as a
    /// header or as bubble rows).
    pub fn composer_exists(&self, composer_id: &str) -> Result<bool> {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(CursorError::DatabaseNotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        if db.read_composer_data(composer_id)?.is_some() {
            return Ok(true);
        }
        db.composer_has_bubbles(composer_id)
    }

    /// Load one session fully (composer data + bubbles + referenced
    /// content blobs) and attach the JSONL transcript path when the
    /// composer's workspace folder maps cleanly into
    /// `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`.
    pub fn read_session(&self, composer_id: &str) -> Result<CursorSession> {
        let db = self.open_db()?;
        let mut session = db.load_session(composer_id)?;
        session.transcript_path = self.locate_transcript(&session);
        Ok(session)
    }

    /// Resolve the JSONL transcript path for a session, if the file
    /// exists on disk. Cursor only emits a transcript for agent-mode
    /// runs against a real on-disk workspace.
    pub fn locate_transcript(&self, session: &CursorSession) -> Option<PathBuf> {
        let abs = session.workspace_path()?.to_string_lossy().into_owned();
        if !abs.starts_with('/') {
            return None;
        }
        let slug = paths::slug_from_abs_path(&abs);
        let p = self.resolver.transcript_path(&slug, session.id());
        p.exists().then_some(p)
    }
}

/// One composer entry as returned by [`CursorIO::list_composers`].
#[derive(Debug, Clone)]
pub struct ComposerListing {
    pub head: ComposerHead,
    /// Whether `cursorDiskKV.bubbleId:<id>:*` has at least one row.
    /// Drafts the user opened but didn't interact with are `false`.
    pub has_bubbles: bool,
}

fn to_metadata(session: &CursorSession) -> CursorSessionMetadata {
    CursorSessionMetadata {
        id: session.id().to_string(),
        name: session.title(),
        subtitle: session
            .data
            .subtitle
            .clone()
            .or_else(|| session.head.as_ref().and_then(|h| h.subtitle.clone())),
        started_at: session.started_at(),
        last_activity: session.last_activity(),
        unified_mode: session.data.unified_mode.clone(),
        workspace_path: session.workspace_path(),
        message_count: session.bubbles.len(),
        first_user_message: session.first_user_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::tests::{BASIC_FIXTURE, fixture_db};
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, CursorIO) {
        let temp = TempDir::new().unwrap();
        let user_data = temp.path().join("UserData");
        let global = user_data.join("User").join("globalStorage");
        fs::create_dir_all(&global).unwrap();
        // Pre-populate the DB.
        let src = fixture_db(BASIC_FIXTURE);
        fs::copy(src.path(), global.join("state.vscdb")).unwrap();
        let resolver = PathResolver::new(temp.path())
            .with_anysphere_dir(temp.path().join(".cursor"))
            .with_user_data_dir(user_data);
        (temp, CursorIO::with_resolver(resolver))
    }

    #[test]
    fn new_roots_at_home() {
        let temp = TempDir::new().unwrap();
        let io = CursorIO::new(temp.path());
        assert_eq!(
            io.db_path(),
            io.resolver().user_dir().join("globalStorage/state.vscdb")
        );
        assert_eq!(io.resolver().home_dir(), temp.path());
    }

    #[test]
    fn lists_composers_with_has_bubbles_flag() {
        let (_t, io) = setup();
        let listings = io.list_composers().unwrap();
        assert_eq!(listings.len(), 1);
        assert!(listings[0].has_bubbles);
        assert_eq!(listings[0].head.composer_id, "c1");
    }

    #[test]
    fn list_session_metadata_populated() {
        let (_t, io) = setup();
        let metas = io.list_session_metadata().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "c1");
        assert_eq!(metas[0].message_count, 3);
        assert_eq!(metas[0].first_user_message.as_deref(), Some("hello"));
        assert_eq!(
            metas[0].workspace_path.as_deref(),
            Some(std::path::Path::new("/p"))
        );
    }

    #[test]
    fn composer_exists_true_false() {
        let (_t, io) = setup();
        assert!(io.composer_exists("c1").unwrap());
        assert!(!io.composer_exists("nope").unwrap());
    }

    #[test]
    fn read_session_locates_transcript_when_file_exists() {
        let (temp, io) = setup();
        // Place a transcript at the expected slug+UUID layout.
        let slug = paths::slug_from_abs_path("/p");
        let transcript_dir = temp
            .path()
            .join(".cursor/projects")
            .join(&slug)
            .join("agent-transcripts/c1");
        fs::create_dir_all(&transcript_dir).unwrap();
        fs::write(
            transcript_dir.join("c1.jsonl"),
            r#"{"role":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        let s = io.read_session("c1").unwrap();
        assert!(s.transcript_path.is_some());
        assert!(
            s.transcript_path
                .as_deref()
                .unwrap()
                .ends_with("agent-transcripts/c1/c1.jsonl")
        );
    }

    #[test]
    fn read_session_with_no_transcript_returns_none() {
        let (_t, io) = setup();
        let s = io.read_session("c1").unwrap();
        // No transcript file laid down.
        assert!(s.transcript_path.is_none());
    }
}
