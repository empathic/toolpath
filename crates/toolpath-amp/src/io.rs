//! Fetching thread exports.
//!
//! Amp keeps no complete local record of a thread, so "reading a session"
//! means fetching the export document. The [`ThreadFetcher`] trait abstracts
//! where it comes from:
//!
//! - [`CliFetcher`] (production): shells out to `amp threads export <id>`,
//!   inheriting the CLI's login. Read-only — it never mutates Amp state.
//! - [`DirFetcher`] (tests / offline archives): reads `<dir>/<id>.json`.

use crate::error::{ConvoError, Result};
use crate::paths::PathResolver;
use crate::reader::ExportReader;
use crate::types::{Session, SessionMetadata};
use std::path::PathBuf;
use std::sync::Arc;

/// Source of thread-export documents.
pub trait ThreadFetcher: Send + Sync + std::fmt::Debug {
    /// Fetch the export document (raw JSON) for one thread.
    fn fetch_export(&self, thread_id: &str) -> Result<String>;

    /// List thread ids, most recent first when the source knows the order.
    fn list_thread_ids(&self) -> Result<Vec<String>>;
}

/// Production fetcher: shells out to the `amp` CLI.
#[derive(Debug, Clone)]
pub struct CliFetcher {
    bin: PathBuf,
}

impl Default for CliFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl CliFetcher {
    pub fn new() -> Self {
        Self { bin: "amp".into() }
    }

    /// Override the binary path (tests point this at a stub script).
    pub fn with_bin<P: Into<PathBuf>>(mut self, bin: P) -> Self {
        self.bin = bin.into();
        self
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new(&self.bin)
            .args(args)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ConvoError::AmpCliNotFound,
                _ => ConvoError::Io(e),
            })?;
        if !out.status.success() {
            return Err(ConvoError::AmpCliFailed {
                command: format!("{} {}", self.bin.display(), args.join(" ")),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl ThreadFetcher for CliFetcher {
    fn fetch_export(&self, thread_id: &str) -> Result<String> {
        self.run(&["threads", "export", thread_id])
    }

    /// Extract `T-…` ids from `amp threads list` (a human-oriented table
    /// whose exact layout is version-dependent; the id shape is stable).
    fn list_thread_ids(&self) -> Result<Vec<String>> {
        let out = self.run(&["threads", "list"])?;
        let mut ids = Vec::new();
        for line in out.lines() {
            for token in line.split_whitespace() {
                let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                if token.starts_with("T-") && token.len() > 2 && !ids.iter().any(|i| i == token) {
                    ids.push(token.to_string());
                }
            }
        }
        Ok(ids)
    }
}

/// Test/offline fetcher: reads pre-exported `<id>.json` files from a
/// directory.
#[derive(Debug, Clone)]
pub struct DirFetcher {
    dir: PathBuf,
}

impl DirFetcher {
    pub fn new<P: Into<PathBuf>>(dir: P) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, thread_id: &str) -> Option<PathBuf> {
        let exact = self.dir.join(format!("{thread_id}.json"));
        if exact.is_file() {
            return Some(exact);
        }
        // Unique-prefix match on file stems.
        let mut hit = None;
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && stem.starts_with(thread_id)
            {
                if hit.is_some() {
                    return None; // ambiguous
                }
                hit = Some(path.clone());
            }
        }
        hit
    }
}

impl ThreadFetcher for DirFetcher {
    fn fetch_export(&self, thread_id: &str) -> Result<String> {
        let path = self
            .path_for(thread_id)
            .ok_or_else(|| ConvoError::SessionNotFound(thread_id.to_string()))?;
        Ok(std::fs::read_to_string(path)?)
    }

    fn list_thread_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return Ok(ids),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// Higher-level session I/O over a [`ThreadFetcher`].
#[derive(Debug, Clone)]
pub struct ConvoIO {
    resolver: PathResolver,
    fetcher: Arc<dyn ThreadFetcher>,
}

impl Default for ConvoIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ConvoIO {
    pub fn new() -> Self {
        Self {
            resolver: PathResolver::new(),
            fetcher: Arc::new(CliFetcher::new()),
        }
    }

    pub fn with_resolver(mut self, resolver: PathResolver) -> Self {
        self.resolver = resolver;
        self
    }

    pub fn with_fetcher(mut self, fetcher: Arc<dyn ThreadFetcher>) -> Self {
        self.fetcher = fetcher;
        self
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    /// Fetch and parse one thread (tolerant mode).
    pub fn read_session(&self, thread_id: &str) -> Result<Session> {
        self.read_session_with(thread_id, false)
    }

    /// Fetch and parse one thread with explicit reader strictness.
    pub fn read_session_with(&self, thread_id: &str, strict: bool) -> Result<Session> {
        let json = self.fetcher.fetch_export(thread_id)?;
        ExportReader::parse_export_with(&json, strict).map(Session::from_export)
    }

    /// List thread ids known to the fetcher.
    pub fn list_thread_ids(&self) -> Result<Vec<String>> {
        self.fetcher.list_thread_ids()
    }

    /// Fetch one thread's lightweight metadata. Amp has no cheap metadata
    /// surface — this fetches the whole export (threads are small).
    pub fn read_metadata(&self, thread_id: &str) -> Result<SessionMetadata> {
        let session = self.read_session(thread_id)?;
        Ok(metadata_of(&session))
    }
}

/// Project a [`Session`] onto its listing metadata.
pub fn metadata_of(session: &Session) -> SessionMetadata {
    SessionMetadata {
        id: session.id.clone(),
        started_at: session.started_at(),
        last_activity: session.last_activity(),
        cwd: session.cwd(),
        version: session.version(),
        first_user_message: session.first_user_text(),
        line_count: session.message_count(),
        dir_path: session.source_path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/real-session.json");
    const REAL_ID: &str = "T-019fa4db-29cf-70c9-8d9b-81524df70e52";

    fn fixture_dir() -> (tempfile::TempDir, DirFetcher) {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(format!("{REAL_ID}.json")), REAL).unwrap();
        let f = DirFetcher::new(t.path());
        (t, f)
    }

    #[test]
    fn dir_fetcher_exact_and_prefix() {
        let (_t, f) = fixture_dir();
        assert!(f.fetch_export(REAL_ID).is_ok());
        assert!(f.fetch_export("T-019fa4db").is_ok());
        assert!(matches!(
            f.fetch_export("T-nope").unwrap_err(),
            ConvoError::SessionNotFound(_)
        ));
    }

    #[test]
    fn dir_fetcher_lists_stems() {
        let (_t, f) = fixture_dir();
        assert_eq!(f.list_thread_ids().unwrap(), vec![REAL_ID.to_string()]);
    }

    #[test]
    fn convo_io_reads_via_fetcher() {
        let (_t, f) = fixture_dir();
        let io = ConvoIO::new().with_fetcher(Arc::new(f));
        let session = io.read_session(REAL_ID).unwrap();
        assert_eq!(session.id, REAL_ID);
        assert_eq!(session.message_count(), 24);
    }

    #[test]
    fn convo_io_metadata() {
        let (_t, f) = fixture_dir();
        let io = ConvoIO::new().with_fetcher(Arc::new(f));
        let meta = io.read_metadata(REAL_ID).unwrap();
        assert_eq!(meta.id, REAL_ID);
        assert_eq!(meta.line_count, 24);
        assert_eq!(meta.cwd.as_deref(), Some("/tmp/amp-elicit"));
        assert_eq!(meta.version.as_deref(), Some("0.0.1785170481-ga5b614"));
        assert!(meta.first_user_message.is_some());
    }

    #[cfg(unix)]
    mod cli {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// Write an executable stub that plays `amp` for CliFetcher tests.
        fn stub_amp(dir: &std::path::Path, body: &str) -> PathBuf {
            let path = dir.join("amp-stub");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        }

        #[test]
        fn cli_fetcher_returns_stdout() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), r#"echo '{"id":"T-x","messages":[]}'"#);
            let f = CliFetcher::new().with_bin(bin);
            let json = f.fetch_export("T-x").unwrap();
            assert!(json.contains("\"T-x\""));
        }

        #[test]
        fn cli_fetcher_failure_carries_stderr() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), "echo 'no such thread' >&2; exit 1");
            let f = CliFetcher::new().with_bin(bin);
            match f.fetch_export("T-x").unwrap_err() {
                ConvoError::AmpCliFailed { stderr, command } => {
                    assert!(stderr.contains("no such thread"));
                    assert!(command.contains("threads export T-x"));
                }
                other => panic!("expected AmpCliFailed, got {other:?}"),
            }
        }

        #[test]
        fn cli_fetcher_missing_binary() {
            let f = CliFetcher::new().with_bin("/definitely/not/a/real/amp");
            assert!(matches!(
                f.fetch_export("T-x").unwrap_err(),
                ConvoError::AmpCliNotFound
            ));
        }

        #[test]
        fn cli_fetcher_list_extracts_thread_ids() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(
                t.path(),
                r#"printf 'T-019fa4db-29cf  Filesystem tool exercise  2h ago\nT-019fa111-aaaa  Other thread  1d ago\n'"#,
            );
            let f = CliFetcher::new().with_bin(bin);
            assert_eq!(
                f.list_thread_ids().unwrap(),
                vec!["T-019fa4db-29cf".to_string(), "T-019fa111-aaaa".to_string()]
            );
        }
    }
}
