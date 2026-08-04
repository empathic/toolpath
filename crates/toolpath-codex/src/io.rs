//! Higher-level filesystem operations over [`PathResolver`].

use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::RolloutReader;
use crate::types::{RolloutItem, Session, SessionMetadata};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ConvoIO {
    resolver: PathResolver,
}

impl ConvoIO {
    pub fn new() -> Self {
        Self {
            resolver: PathResolver::new(),
        }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    pub fn codex_dir_path(&self) -> Result<PathBuf> {
        self.resolver.codex_dir()
    }

    /// List every rollout file under `~/.codex/sessions/`, newest first.
    pub fn list_rollout_files(&self) -> Result<Vec<PathBuf>> {
        self.resolver.list_rollout_files()
    }

    /// List every session id (the rollout filename stem, which
    /// [`Self::read_session`] resolves without a tree walk), newest
    /// first. One directory walk, no file reads — unlike
    /// [`Self::list_sessions`], which parses every file for metadata.
    pub fn list_session_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .list_rollout_files()?
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect())
    }

    /// Return lightweight metadata for every rollout, newest first.
    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let files = self.list_rollout_files()?;
        let mut metas = Vec::with_capacity(files.len());
        for path in files {
            match self.read_metadata(&path) {
                Ok(m) => metas.push(m),
                Err(e) => {
                    eprintln!("Warning: failed to read {}: {}", path.display(), e);
                }
            }
        }
        metas.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        Ok(metas)
    }

    /// Read one session by id or filename stem.
    pub fn read_session(&self, session_id: &str) -> Result<Session> {
        let path = self.resolver.find_rollout_file(session_id)?;
        RolloutReader::read_session(&path)
    }

    /// Read one session by absolute path.
    pub fn read_session_path<P: AsRef<std::path::Path>>(&self, path: P) -> Result<Session> {
        RolloutReader::read_session(path)
    }

    /// Cheap per-file metadata: a single streaming pass that
    /// JSON-parses only the head of the file (session_meta, first
    /// timestamps, first user prompt all live there in this
    /// append-only log) plus the final line (last timestamp), and
    /// otherwise just counts lines. Multi-gigabyte session trees made
    /// the previous parse-every-line approach a minute-plus stall in
    /// every session-listing surface (`p list codex`, `share`, bare
    /// `resume`).
    ///
    /// Bounded-head consequences, deliberate: `first_user_message` is
    /// `None` if the first prompt appears after the head budget, and
    /// `line_count` counts non-empty lines (unparseable ones
    /// included) rather than successfully parsed ones.
    pub fn read_metadata<P: AsRef<std::path::Path>>(&self, path: P) -> Result<SessionMetadata> {
        use std::io::BufRead;

        let path = path.as_ref();
        if !path.exists() {
            return Err(crate::error::ConvoError::SessionNotFound(
                path.display().to_string(),
            ));
        }

        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
        let mut raw = String::new();
        let mut last_nonempty = String::new();

        let mut line_count = 0usize;
        let mut hunt = HeadHunt::default();

        loop {
            raw.clear();
            match reader.read_line(&mut raw) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    eprintln!(
                        "Warning: IO error reading {} after line {}: {}",
                        path.display(),
                        line_count,
                        e
                    );
                    break;
                }
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            line_count += 1;
            hunt.ingest(line_count, trimmed);
            std::mem::swap(&mut last_nonempty, &mut raw);
        }

        hunt.ingest_tail_line(last_nonempty.trim());
        Ok(hunt.into_metadata(path, line_count))
    }

    /// Like [`Self::read_metadata`] but O(1) in file size: reads one
    /// chunk from the head (session_meta, first timestamps, first user
    /// prompt) and one chunk from the tail (newest timestamp), never
    /// streaming the bytes in between. The price is no `line_count` —
    /// this is the right call for recency-ranked listing surfaces that
    /// hydrate only the newest N sessions.
    pub fn peek_metadata<P: AsRef<std::path::Path>>(&self, path: P) -> Result<SessionPeek> {
        use std::io::{Read, Seek, SeekFrom};

        const CHUNK: u64 = 256 * 1024;

        let path = path.as_ref();
        if !path.exists() {
            return Err(crate::error::ConvoError::SessionNotFound(
                path.display().to_string(),
            ));
        }

        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();

        let mut head = vec![0u8; CHUNK.min(len) as usize];
        file.read_exact(&mut head)?;
        let head_text = String::from_utf8_lossy(&head);
        // When the chunk cut a line in half, the final fragment is not
        // a complete record — drop it (the tail chunk covers file end).
        let head_complete = if len > CHUNK {
            match head_text.rfind('\n') {
                Some(i) => &head_text[..i],
                None => "",
            }
        } else {
            &head_text
        };

        let mut hunt = HeadHunt::default();
        let mut line_no = 0usize;
        for line in head_complete.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            line_no += 1;
            hunt.ingest(line_no, trimmed);
        }

        if len > CHUNK {
            let mut tail = vec![0u8; CHUNK as usize];
            file.seek(SeekFrom::Start(len - CHUNK))?;
            file.read_exact(&mut tail)?;
            let tail_text = String::from_utf8_lossy(&tail);
            // Skip the first fragment (likely mid-line after the seek);
            // walk backward to the last parseable line.
            for line in tail_text.lines().rev() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if hunt.try_ingest_tail_line(trimmed) {
                    break;
                }
            }
        }

        // Last resort for last_activity: the file's mtime — good
        // enough for a recency listing when no line parsed.
        if hunt.last_ts.is_none()
            && let Ok(meta) = std::fs::metadata(path)
            && let Ok(modified) = meta.modified()
        {
            hunt.last_ts = Some(chrono::DateTime::<chrono::Utc>::from(modified));
        }

        let m = hunt.into_metadata(path, 0);
        Ok(SessionPeek {
            id: m.id,
            file_path: m.file_path,
            started_at: m.started_at,
            last_activity: m.last_activity,
            cwd: m.cwd,
            cli_version: m.cli_version,
            first_user_message: m.first_user_message,
            git_branch: m.git_branch,
            git_commit: m.git_commit,
        })
    }

    pub fn session_exists(&self, session_id: &str) -> bool {
        self.resolver.find_rollout_file(session_id).is_ok()
    }
}

/// [`SessionMetadata`] minus `line_count`, produced by
/// [`ConvoIO::peek_metadata`], which never reads enough of the file to
/// count lines.
#[derive(Debug, Clone)]
pub struct SessionPeek {
    pub id: String,
    pub file_path: PathBuf,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_activity: Option<chrono::DateTime<chrono::Utc>>,
    pub cwd: Option<PathBuf>,
    pub cli_version: Option<String>,
    pub first_user_message: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
}

/// Shared head-of-file metadata hunt used by both `read_metadata`
/// (streaming) and `peek_metadata` (chunked): session_meta, first/last
/// timestamps, and the first user prompt, all bounded by
/// `HEAD_PARSE_BUDGET` parsed lines.
#[derive(Default)]
struct HeadHunt {
    parsed: usize,
    first_line_meta_id: Option<String>,
    meta: Option<Box<crate::types::SessionMeta>>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    last_ts: Option<chrono::DateTime<chrono::Utc>>,
    first_user: Option<String>,
    first_user_fallback: Option<String>,
}

impl HeadHunt {
    /// Parse at most this many non-empty head lines looking for
    /// session_meta / timestamps / the first user prompt. Real
    /// sessions surface the prompt within the first dozen lines
    /// (after session_meta, turn_context, and injected context).
    const HEAD_PARSE_BUDGET: usize = 100;

    fn ingest(&mut self, line_no: usize, trimmed: &str) {
        use crate::types::{ResponseItem, RolloutLine};

        let still_hunting =
            self.meta.is_none() || self.started_at.is_none() || self.first_user.is_none();
        if !still_hunting || self.parsed >= Self::HEAD_PARSE_BUDGET {
            return;
        }
        self.parsed += 1;
        let Ok(line) = serde_json::from_str::<RolloutLine>(trimmed) else {
            return;
        };
        if let Some(ts) = line.parsed_timestamp() {
            if self.started_at.is_none_or(|s| ts < s) {
                self.started_at = Some(ts);
            }
            if self.last_ts.is_none_or(|l| ts > l) {
                self.last_ts = Some(ts);
            }
        }
        if line_no == 1 && line.kind == "session_meta" {
            self.first_line_meta_id = line
                .payload
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        if self.first_user.is_none()
            && line.kind == "event_msg"
            && line.payload.get("type").and_then(|v| v.as_str()) == Some("user_message")
            && let Some(msg) = line.payload.get("message").and_then(|v| v.as_str())
            && !msg.is_empty()
        {
            self.first_user = Some(msg.to_string());
        }
        match line.item() {
            RolloutItem::SessionMeta(m) if self.meta.is_none() => self.meta = Some(m),
            RolloutItem::ResponseItem(ResponseItem::Message(m))
                if m.role == "user" && self.first_user_fallback.is_none() =>
            {
                let t = m.text();
                if !t.is_empty() {
                    self.first_user_fallback = Some(t);
                }
            }
            _ => {}
        }
    }

    /// The tail line carries the newest timestamp in an append-only
    /// log; parse just that one.
    fn ingest_tail_line(&mut self, trimmed: &str) {
        self.try_ingest_tail_line(trimmed);
    }

    /// Returns true when the line parsed as a rollout line (whether or
    /// not it carried a newer timestamp).
    fn try_ingest_tail_line(&mut self, trimmed: &str) -> bool {
        use crate::types::RolloutLine;
        let Ok(line) = serde_json::from_str::<RolloutLine>(trimmed) else {
            return false;
        };
        if let Some(ts) = line.parsed_timestamp()
            && self.last_ts.is_none_or(|l| ts > l)
        {
            self.last_ts = Some(ts);
        }
        true
    }

    fn into_metadata(self, path: &std::path::Path, line_count: usize) -> SessionMetadata {
        // Same id rule as RolloutReader::derive_session_id: the first
        // line's session_meta payload wins, else the filename stem.
        let id = self.first_line_meta_id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| crate::paths::session_id_from_stem(stem).to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });

        let (cwd, cli_version, git_branch, git_commit) = match &self.meta {
            Some(m) => (
                Some(m.cwd.clone()),
                Some(m.cli_version.clone()),
                m.git.as_ref().and_then(|g| g.branch.clone()),
                m.git.as_ref().and_then(|g| g.commit_hash.clone()),
            ),
            None => (None, None, None, None),
        };

        SessionMetadata {
            id,
            file_path: path.to_path_buf(),
            started_at: self.started_at,
            last_activity: self.last_ts,
            cwd,
            cli_version,
            first_user_message: self.first_user.or(self.first_user_fallback),
            git_branch,
            git_commit,
            line_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ConvoIO) {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        let day = codex.join("sessions/2026/04/20");
        fs::create_dir_all(&day).unwrap();
        let body = [
            r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{"id":"019dabc6-aaa","timestamp":"2026-04-20T16:43:30.171Z","cwd":"/tmp/proj","originator":"codex-tui","cli_version":"0.118.0","source":"cli","git":{"commit_hash":"abc","branch":"main"}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        ]
        .join("\n");
        fs::write(
            day.join("rollout-2026-04-20T10-00-00-019dabc6-aaa.jsonl"),
            body,
        )
        .unwrap();

        let resolver = PathResolver::new().with_codex_dir(&codex);
        (temp, ConvoIO::with_resolver(resolver))
    }

    #[test]
    fn lists_rollouts() {
        let (_t, io) = setup();
        let files = io.list_rollout_files().unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn list_sessions_returns_metadata() {
        let (_t, io) = setup();
        let sessions = io.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "019dabc6-aaa");
        assert_eq!(sessions[0].first_user_message.as_deref(), Some("hi"));
        assert_eq!(sessions[0].git_branch.as_deref(), Some("main"));
        assert_eq!(sessions[0].git_commit.as_deref(), Some("abc"));
        assert_eq!(sessions[0].cli_version.as_deref(), Some("0.118.0"));
    }

    /// Ids come from filenames alone, so even a file whose body would
    /// fail to parse is listed; the failure surfaces on read instead.
    #[test]
    fn list_session_ids_returns_stems_without_reading_bodies() {
        let (_t, io) = setup();
        let day = io.resolver().sessions_root().unwrap().join("2026/04/21");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-04-21T09-00-00-bbb.jsonl"),
            "not json",
        )
        .unwrap();

        let ids = io.list_session_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"rollout-2026-04-20T10-00-00-019dabc6-aaa".to_string()));
        assert!(ids.contains(&"rollout-2026-04-21T09-00-00-bbb".to_string()));
        for id in &ids {
            assert!(io.read_session(id).is_ok() || id.contains("bbb"));
        }
    }

    #[test]
    fn read_session_by_id() {
        let (_t, io) = setup();
        let s = io.read_session("019dabc6-aaa").unwrap();
        assert_eq!(s.lines.len(), 2);
    }

    #[test]
    fn read_session_by_partial_uuid() {
        let (_t, io) = setup();
        let s = io.read_session("019dabc6").unwrap();
        assert_eq!(s.id, "019dabc6-aaa");
    }

    #[test]
    fn session_exists() {
        let (_t, io) = setup();
        assert!(io.session_exists("019dabc6-aaa"));
        assert!(!io.session_exists("nope"));
    }

    #[test]
    fn metadata_line_count_accurate() {
        let (_t, io) = setup();
        let metas = io.list_sessions().unwrap();
        assert_eq!(metas[0].line_count, 2);
    }

    #[test]
    fn list_sessions_empty_when_no_root() {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let io = ConvoIO::with_resolver(PathResolver::new().with_codex_dir(&codex));
        assert!(io.list_sessions().unwrap().is_empty());
    }

    /// Write a rollout body into the fixture tree and return its path.
    fn write_rollout(io: &ConvoIO, name: &str, body: &str) -> PathBuf {
        let day = io.resolver().sessions_root().unwrap().join("2026/04/22");
        fs::create_dir_all(&day).unwrap();
        let path = day.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn metadata_last_activity_comes_from_tail_line() {
        let (_t, io) = setup();
        let body = [
            r#"{"timestamp":"2026-04-22T10:00:00.000Z","type":"session_meta","payload":{"id":"019dtail-bbb","cwd":"/tmp/p","originator":"codex-tui","cli_version":"0.118.0","source":"cli"}}"#,
            r#"{"timestamp":"2026-04-22T10:00:01.000Z","type":"event_msg","payload":{"type":"user_message","message":"first prompt"}}"#,
            r#"{"timestamp":"2026-04-22T11:30:00.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
        ]
        .join("\n");
        let path = write_rollout(&io, "rollout-2026-04-22T10-00-00-019dtail-bbb.jsonl", &body);
        let m = io.read_metadata(&path).unwrap();
        assert_eq!(m.id, "019dtail-bbb");
        assert_eq!(
            m.started_at
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T10:00:00Z"
        );
        assert_eq!(
            m.last_activity
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T11:30:00Z"
        );
        assert_eq!(m.first_user_message.as_deref(), Some("first prompt"));
    }

    /// line_count counts non-empty lines; blank and unparseable lines
    /// don't abort the scan (the count deliberately includes junk —
    /// it approximates message_count, nothing more).
    #[test]
    fn metadata_line_count_counts_nonempty_lines_tolerantly() {
        let (_t, io) = setup();
        let body = [
            r#"{"timestamp":"2026-04-22T10:00:00.000Z","type":"session_meta","payload":{"id":"019djunk-ccc","cwd":"/tmp/p","originator":"codex-tui","cli_version":"0.118.0","source":"cli"}}"#,
            "",
            r#"{"not json"#,
            r#"{"timestamp":"2026-04-22T10:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
        ]
        .join("\n");
        let path = write_rollout(&io, "rollout-2026-04-22T10-00-00-019djunk-ccc.jsonl", &body);
        let m = io.read_metadata(&path).unwrap();
        assert_eq!(m.line_count, 3);
        assert_eq!(m.first_user_message.as_deref(), Some("hi"));
    }

    /// The head-parse budget bounds the prompt hunt: a first user
    /// message buried past the budget yields None rather than a full
    /// parse of the file.
    #[test]
    fn metadata_first_user_none_beyond_head_budget() {
        let (_t, io) = setup();
        let mut lines = vec![
            r#"{"timestamp":"2026-04-22T10:00:00.000Z","type":"session_meta","payload":{"id":"019ddeep-ddd","cwd":"/tmp/p","originator":"codex-tui","cli_version":"0.118.0","source":"cli"}}"#.to_string(),
        ];
        for i in 0..150 {
            lines.push(format!(
                r#"{{"timestamp":"2026-04-22T10:00:01.000Z","type":"event_msg","payload":{{"type":"task_started","n":{i}}}}}"#
            ));
        }
        lines.push(
            r#"{"timestamp":"2026-04-22T10:05:00.000Z","type":"event_msg","payload":{"type":"user_message","message":"buried"}}"#.to_string(),
        );
        let path = write_rollout(
            &io,
            "rollout-2026-04-22T10-00-00-019ddeep-ddd.jsonl",
            &lines.join("\n"),
        );
        let m = io.read_metadata(&path).unwrap();
        assert_eq!(m.first_user_message, None);
        assert_eq!(m.line_count, 152);
        // The tail line still supplies last_activity even though the
        // head budget was exhausted long before it.
        assert_eq!(
            m.last_activity
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T10:05:00Z"
        );
    }

    #[test]
    fn peek_matches_read_metadata_on_small_file() {
        let (_t, io) = setup();
        let files = io.list_rollout_files().unwrap();
        let full = io.read_metadata(&files[0]).unwrap();
        let peek = io.peek_metadata(&files[0]).unwrap();
        assert_eq!(peek.id, full.id);
        assert_eq!(peek.started_at, full.started_at);
        assert_eq!(peek.last_activity, full.last_activity);
        assert_eq!(peek.cwd, full.cwd);
        assert_eq!(peek.first_user_message, full.first_user_message);
        assert_eq!(peek.git_branch, full.git_branch);
    }

    /// A file much larger than the peek chunk: the head facts come
    /// from the first chunk, last_activity from the tail chunk, and
    /// the middle is never needed.
    #[test]
    fn peek_big_file_tail_supplies_last_activity() {
        let (_t, io) = setup();
        let mut lines = vec![
            r#"{"timestamp":"2026-04-22T10:00:00.000Z","type":"session_meta","payload":{"id":"019dpeek-eee","cwd":"/tmp/p","originator":"codex-tui","cli_version":"0.118.0","source":"cli"}}"#.to_string(),
            r#"{"timestamp":"2026-04-22T10:00:01.000Z","type":"event_msg","payload":{"type":"user_message","message":"peek prompt"}}"#.to_string(),
        ];
        // ~600 KB of filler so the file dwarfs the 256 KiB chunk.
        let filler = "x".repeat(2000);
        for i in 0..300 {
            lines.push(format!(
                r#"{{"timestamp":"2026-04-22T10:10:00.000Z","type":"event_msg","payload":{{"type":"task_started","n":{i},"pad":"{filler}"}}}}"#
            ));
        }
        lines.push(
            r#"{"timestamp":"2026-04-22T12:00:00.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#.to_string(),
        );
        let path = write_rollout(
            &io,
            "rollout-2026-04-22T10-00-00-019dpeek-eee.jsonl",
            &lines.join("\n"),
        );
        assert!(std::fs::metadata(&path).unwrap().len() > 512 * 1024);
        let m = io.peek_metadata(&path).unwrap();
        assert_eq!(m.id, "019dpeek-eee");
        assert_eq!(m.first_user_message.as_deref(), Some("peek prompt"));
        assert_eq!(
            m.last_activity
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T12:00:00Z"
        );
        assert_eq!(
            m.started_at
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T10:00:00Z"
        );
    }

    /// A truncated final line (crash mid-write) must not lose the tail
    /// timestamp: the reverse walk skips junk to the last parseable line.
    #[test]
    fn peek_tolerates_truncated_tail_line() {
        let (_t, io) = setup();
        let filler = "y".repeat(2000);
        let mut lines = vec![
            r#"{"timestamp":"2026-04-22T10:00:00.000Z","type":"session_meta","payload":{"id":"019dcut-fff","cwd":"/tmp/p","originator":"codex-tui","cli_version":"0.118.0","source":"cli"}}"#.to_string(),
        ];
        for i in 0..300 {
            lines.push(format!(
                r#"{{"timestamp":"2026-04-22T11:00:00.000Z","type":"event_msg","payload":{{"type":"task_started","n":{i},"pad":"{filler}"}}}}"#
            ));
        }
        lines.push(
            r#"{"timestamp":"2026-04-22T11:30:00.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#.to_string(),
        );
        lines.push(r#"{"timestamp":"2026-04-22T11:59:59.000Z","type":"event_"#.to_string()); // truncated
        let path = write_rollout(
            &io,
            "rollout-2026-04-22T10-00-00-019dcut-fff.jsonl",
            &lines.join("\n"),
        );
        let m = io.peek_metadata(&path).unwrap();
        assert_eq!(
            m.last_activity
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-04-22T11:30:00Z"
        );
    }
}
