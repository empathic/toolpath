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

/// Does this token look like an Amp thread id (`T-<uuidv7>`)?
///
/// Stricter than "starts with `T-`", because the id column shares a line
/// with a free-form title and a title word like `T-shirt` must not be
/// mistaken for an id. Deliberately does NOT require the body to be pure
/// hex: the real defense is taking the *last* matching token on the row
/// (the id is the last column), and demanding hex would silently drop
/// every thread if Amp ever widened the id alphabet.
fn looks_like_thread_id(token: &str) -> bool {
    match token.strip_prefix("T-") {
        Some(rest) => rest.len() >= 8 && rest.contains('-'),
        None => false,
    }
}

/// Public shape check for Amp thread ids (`T-<uuidv7-ish>`). Callers that
/// pass an operator-supplied id straight into the `amp` subprocess argv
/// should validate with this first — a value starting with `-` would parse
/// as a flag.
pub fn is_thread_id(token: &str) -> bool {
    looks_like_thread_id(token)
}

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
    ///
    /// The Thread ID is the **last** column and the Title is the first, so
    /// each row's id is the *last* `T-`-prefixed token on the line — taking
    /// the first would harvest a title like "T-shirt sizing" instead. Ids
    /// are also required to look like `T-<uuid-ish>` (long, hyphenated) so
    /// a short title token can't masquerade as one.
    fn list_thread_ids(&self) -> Result<Vec<String>> {
        let out = self.run(&["threads", "list"])?;
        let mut ids = Vec::new();
        for line in out.lines() {
            let id = line
                .split_whitespace()
                .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
                .rfind(|t| looks_like_thread_id(t));
            if let Some(id) = id
                && !ids.iter().any(|i| i == id)
            {
                ids.push(id.to_string());
            }
        }
        Ok(ids)
    }
}

/// Creates and seeds Amp threads — the writer half of [`ThreadFetcher`].
///
/// Amp threads are server-authoritative: there is no local store to write
/// into, and the server's own thread-import path is a Rivet actor call
/// behind a websocket-token handshake (see
/// `docs/agents/formats/amp/writing-compatible.md`). What *is* first-party,
/// documented, and sufficient for resume is this two-step: create an empty
/// thread server-side, then send it one user message carrying the prior
/// session's context.
pub trait ThreadWriter: Send + Sync + std::fmt::Debug {
    /// Create a fresh, empty thread and return its server-assigned id.
    /// Only ever creates new threads — never touches existing ones.
    fn create_thread(&self) -> Result<String>;

    /// Send one user message into a thread. This runs a real agent turn.
    fn send_message(&self, thread_id: &str, message: &str) -> Result<()>;
}

/// Production writer: shells out to the `amp` CLI (`threads new`,
/// `threads continue -x`), inheriting its login exactly like [`CliFetcher`].
#[derive(Debug, Clone)]
pub struct CliWriter {
    bin: PathBuf,
}

impl Default for CliWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl CliWriter {
    pub fn new() -> Self {
        Self { bin: "amp".into() }
    }

    /// Override the binary path (tests point this at a stub script).
    pub fn with_bin<P: Into<PathBuf>>(mut self, bin: P) -> Self {
        self.bin = bin.into();
        self
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        self.spawn(args, None)
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Result<String> {
        self.spawn(args, Some(stdin))
    }

    fn spawn(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        use std::io::Write;
        use std::process::Stdio;

        let mut cmd = std::process::Command::new(&self.bin);
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ConvoError::AmpCliNotFound,
            _ => ConvoError::Io(e),
        })?;
        // The (multi-MB) body streams in from its own thread while
        // `wait_with_output` drains stdout/stderr — writing it from this
        // thread would deadlock as soon as a chatty child fills its stdout
        // pipe before consuming all of stdin.
        let feeder = stdin.map(|body| {
            let mut pipe = child.stdin.take().expect("stdin piped");
            let body = body.as_bytes().to_vec();
            std::thread::spawn(move || {
                pipe.write_all(&body)
                // Dropped here, closing the pipe so `amp` sees EOF.
            })
        });
        let out = child.wait_with_output()?;
        let fed = feeder.map(|h| h.join().expect("stdin feeder panicked"));
        if !out.status.success() {
            // The exit status + stderr is the real diagnosis; a write error
            // (EPIPE into an early-exiting child) is only its symptom and
            // must not mask it.
            return Err(ConvoError::AmpCliFailed {
                command: format!("{} {}", self.bin.display(), args.join(" ")),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        if let Some(Err(e)) = fed {
            // Exit 0 but the body was never consumed — not a successful send.
            return Err(ConvoError::Io(e));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl ThreadWriter for CliWriter {
    fn create_thread(&self) -> Result<String> {
        // `amp threads new` prints just the id, and creates the thread
        // server-side without running a model turn. This is the one parse
        // that names irreversible server-side state (the filed artifact and
        // the `threads continue` target), so it demands the same id shape
        // and punctuation-trimming as the fetcher's list parser — `Created
        // T-abc.` must not yield `T-abc.`.
        let out = self.run(&["threads", "new"])?;
        let id = out
            .split_whitespace()
            .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
            .find(|t| looks_like_thread_id(t))
            .ok_or_else(|| ConvoError::AmpCliFailed {
                command: format!("{} threads new", self.bin.display()),
                stderr: format!("no T-… thread id in output: {}", out.trim()),
            })?;
        Ok(id.to_string())
    }

    fn send_message(&self, thread_id: &str, message: &str) -> Result<()> {
        // The message goes over **stdin**, not argv: `amp --help` documents
        // both forms ("pass a message as an argument… or pipe via stdin"),
        // and a rendered transcript routinely exceeds the argv ceiling
        // (1 MB `ARG_MAX` on macOS, 32 KB on Windows) once a session
        // contains a large diff — which would fail with E2BIG *after* the
        // thread had already been created.
        self.run_with_stdin(&["threads", "continue", thread_id, "-x"], message)?;
        Ok(())
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

    fn path_for(&self, thread_id: &str) -> Result<PathBuf> {
        let exact = self.dir.join(format!("{thread_id}.json"));
        if exact.is_file() {
            return Ok(exact);
        }
        // Unique-prefix match on file stems. Each failure mode keeps its own
        // error class: not-found, ambiguous, and I/O (e.g. permissions) are
        // three different user problems.
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConvoError::SessionNotFound(thread_id.to_string()));
            }
            Err(e) => return Err(ConvoError::Io(e)),
        };
        let mut hit: Option<PathBuf> = None;
        for entry in entries {
            let path = entry.map_err(ConvoError::Io)?.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && stem.starts_with(thread_id)
            {
                if let Some(prev) = &hit {
                    return Err(ConvoError::InvalidFormat(format!(
                        "ambiguous thread id prefix {thread_id}: matches {} and {}",
                        prev.display(),
                        path.display()
                    )));
                }
                hit = Some(path);
            }
        }
        hit.ok_or_else(|| ConvoError::SessionNotFound(thread_id.to_string()))
    }
}

impl ThreadFetcher for DirFetcher {
    fn fetch_export(&self, thread_id: &str) -> Result<String> {
        let path = self.path_for(thread_id)?;
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
    fn dir_fetcher_ambiguous_prefix_is_a_distinct_error() {
        let t = tempfile::tempdir().unwrap();
        for suffix in ["0001", "0002"] {
            std::fs::write(
                t.path()
                    .join(format!("T-019fa111-aaaa-7bbb-8ccc-ddddeeee{suffix}.json")),
                "{}",
            )
            .unwrap();
        }
        let f = DirFetcher::new(t.path());
        match f.fetch_export("T-019fa111").unwrap_err() {
            ConvoError::InvalidFormat(msg) => {
                assert!(msg.contains("ambiguous"), "actual: {msg}")
            }
            other => panic!("ambiguity must not report SessionNotFound, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn dir_fetcher_unreadable_dir_reports_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let f = DirFetcher::new(t.path());
        let err = f.fetch_export("T-x").unwrap_err();
        std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, ConvoError::Io(_)), "got {err:?}");
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
        /// The real layout is Title-first, Thread-ID-last (see
        /// docs/agents/formats/amp/resume-and-sessions.md), and titles are
        /// free-form — including titles that begin with `T-`.
        fn cli_fetcher_list_extracts_thread_ids() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(
                t.path(),
                r#"printf 'Title  Last Updated  Visibility  Messages  Thread ID\n'
printf 'T-shirt sizing rubric  2m ago  Private  1  T-019fa4db-29cf-70c9-8d9b-81524df70e52\n'
printf 'T-1000-terminator-notes  1d ago  Private  2  T-019fa111-aaaa-7bbb-8ccc-ddddeeee0002\n'"#,
            );
            let f = CliFetcher::new().with_bin(bin);
            assert_eq!(
                f.list_thread_ids().unwrap(),
                vec![
                    "T-019fa4db-29cf-70c9-8d9b-81524df70e52".to_string(),
                    "T-019fa111-aaaa-7bbb-8ccc-ddddeeee0002".to_string()
                ],
                "id comes from the last column; a T- title is not an id"
            );
        }

        // ── CliWriter ────────────────────────────────────────────────

        #[test]
        fn cli_writer_create_thread_parses_id() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), "echo T-019fa4db-29cf-70c9-8d9b-81524df70e52");
            let w = CliWriter::new().with_bin(bin);
            assert_eq!(
                w.create_thread().unwrap(),
                "T-019fa4db-29cf-70c9-8d9b-81524df70e52"
            );
        }

        /// A non-zero exit must never be reported as success — this is the
        /// writer that creates server-side state and spends credits.
        #[test]
        fn cli_writer_propagates_failure_exit_status() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), "echo 'out of credits' >&2; exit 1");
            let w = CliWriter::new().with_bin(bin);
            match w.create_thread().unwrap_err() {
                ConvoError::AmpCliFailed { stderr, .. } => {
                    assert!(stderr.contains("out of credits"))
                }
                other => panic!("expected AmpCliFailed, got {other:?}"),
            }
            let t2 = tempfile::tempdir().unwrap();
            let bin2 = stub_amp(t2.path(), "echo 'rate limited' >&2; exit 1");
            let w2 = CliWriter::new().with_bin(bin2);
            match w2.send_message("T-x", "hi").unwrap_err() {
                ConvoError::AmpCliFailed { stderr, .. } => {
                    assert!(stderr.contains("rate limited"))
                }
                other => panic!("expected AmpCliFailed, got {other:?}"),
            }
        }

        /// An early-exiting child (e.g. auth expired between `threads new`
        /// and `continue`) must surface its stderr, not a bare `Broken pipe`
        /// from writing the transcript into a closed stdin.
        #[test]
        fn cli_writer_early_exit_reports_stderr_not_broken_pipe() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), "echo 'not logged in' >&2; exit 1");
            let w = CliWriter::new().with_bin(bin);
            let big = "x".repeat(2 * 1024 * 1024);
            match w.send_message("T-abc", &big).unwrap_err() {
                ConvoError::AmpCliFailed { stderr, .. } => {
                    assert!(stderr.contains("not logged in"), "actual: {stderr}")
                }
                other => panic!("expected AmpCliFailed with stderr, got {other:?}"),
            }
        }

        /// The child may emit more than a pipe buffer of output before it
        /// drains stdin (progress logs); writing the multi-MB transcript
        /// from the parent thread while nobody reads stdout would deadlock.
        #[test]
        fn cli_writer_survives_chatty_child_output() {
            let t = tempfile::tempdir().unwrap();
            let out = t.path().join("seen");
            // ~266 KB of stdout BEFORE reading any stdin.
            let bin = stub_amp(
                t.path(),
                &format!(
                    "i=0; while [ $i -lt 4096 ]; do printf '%064d\\n' $i; i=$((i+1)); done; cat > {}",
                    out.display()
                ),
            );
            let w = CliWriter::new().with_bin(bin);
            let big = "y".repeat(2 * 1024 * 1024);
            w.send_message("T-abc", &big).unwrap();
            assert_eq!(std::fs::read_to_string(&out).unwrap().len(), big.len());
        }

        /// The one parse that names irreversible server-side state must be
        /// as strict as the fetcher's: id-shaped, punctuation-trimmed.
        #[test]
        fn cli_writer_create_thread_requires_id_shape_and_trims() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(
                t.path(),
                "echo 'Created T-019fa111-aaaa-7bbb-8ccc-ddddeeee0002.'",
            );
            let w = CliWriter::new().with_bin(bin);
            assert_eq!(
                w.create_thread().unwrap(),
                "T-019fa111-aaaa-7bbb-8ccc-ddddeeee0002"
            );
            // A short `T-…` word is a word, not a thread id.
            let t2 = tempfile::tempdir().unwrap();
            let bin2 = stub_amp(t2.path(), "echo 'T-junk output'");
            let w2 = CliWriter::new().with_bin(bin2);
            assert!(w2.create_thread().is_err());
        }

        #[test]
        fn cli_writer_rejects_output_without_a_thread_id() {
            let t = tempfile::tempdir().unwrap();
            let bin = stub_amp(t.path(), "echo 'Created a thread.'");
            let w = CliWriter::new().with_bin(bin);
            match w.create_thread().unwrap_err() {
                ConvoError::AmpCliFailed { stderr, .. } => {
                    assert!(stderr.contains("no T-"), "actual: {stderr}")
                }
                other => panic!("expected AmpCliFailed, got {other:?}"),
            }
        }

        #[test]
        fn cli_writer_missing_binary() {
            let w = CliWriter::new().with_bin("/definitely/not/a/real/amp");
            assert!(matches!(
                w.create_thread().unwrap_err(),
                ConvoError::AmpCliNotFound
            ));
        }

        /// The message must travel over stdin, not argv — a rendered
        /// transcript can exceed ARG_MAX, and that failure would land
        /// *after* the thread was created.
        #[test]
        fn cli_writer_sends_message_over_stdin_not_argv() {
            let t = tempfile::tempdir().unwrap();
            let out = t.path().join("seen");
            let bin = stub_amp(
                t.path(),
                &format!(
                    "cat > {}\nprintf '%s' \"$*\" > {}.argv",
                    out.display(),
                    out.display()
                ),
            );
            let w = CliWriter::new().with_bin(bin);
            // Comfortably beyond the 1 MB macOS single-arg ceiling.
            let big = "x".repeat(2 * 1024 * 1024);
            w.send_message("T-abc", &big).unwrap();
            assert_eq!(std::fs::read_to_string(&out).unwrap().len(), big.len());
            let argv = std::fs::read_to_string(format!("{}.argv", out.display())).unwrap();
            assert_eq!(argv, "threads continue T-abc -x");
        }
    }
}
