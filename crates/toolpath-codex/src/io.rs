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
        use crate::types::{ResponseItem, RolloutLine};
        use std::io::BufRead;

        // Parse at most this many non-empty head lines looking for
        // session_meta / timestamps / the first user prompt. Real
        // sessions surface the prompt within the first dozen lines
        // (after session_meta, turn_context, and injected context).
        const HEAD_PARSE_BUDGET: usize = 100;

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
        let mut head_parsed = 0usize;
        let mut first_line_meta_id: Option<String> = None;
        let mut meta: Option<Box<crate::types::SessionMeta>> = None;
        let mut started_at = None;
        let mut last_ts = None;
        let mut first_user: Option<String> = None;
        let mut first_user_fallback: Option<String> = None;

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

            let still_hunting = meta.is_none() || started_at.is_none() || first_user.is_none();
            if still_hunting && head_parsed < HEAD_PARSE_BUDGET {
                head_parsed += 1;
                if let Ok(line) = serde_json::from_str::<RolloutLine>(trimmed) {
                    if let Some(ts) = line.parsed_timestamp() {
                        if started_at.is_none_or(|s| ts < s) {
                            started_at = Some(ts);
                        }
                        if last_ts.is_none_or(|l| ts > l) {
                            last_ts = Some(ts);
                        }
                    }
                    if line_count == 1 && line.kind == "session_meta" {
                        first_line_meta_id = line
                            .payload
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                    if first_user.is_none()
                        && line.kind == "event_msg"
                        && line.payload.get("type").and_then(|v| v.as_str()) == Some("user_message")
                        && let Some(msg) = line.payload.get("message").and_then(|v| v.as_str())
                        && !msg.is_empty()
                    {
                        first_user = Some(msg.to_string());
                    }
                    match line.item() {
                        RolloutItem::SessionMeta(m) if meta.is_none() => meta = Some(m),
                        RolloutItem::ResponseItem(ResponseItem::Message(m))
                            if m.role == "user" && first_user_fallback.is_none() =>
                        {
                            let t = m.text();
                            if !t.is_empty() {
                                first_user_fallback = Some(t);
                            }
                        }
                        _ => {}
                    }
                }
            }
            std::mem::swap(&mut last_nonempty, &mut raw);
        }

        // The tail line carries the newest timestamp in an
        // append-only log; parse just that one.
        if let Ok(line) = serde_json::from_str::<RolloutLine>(last_nonempty.trim())
            && let Some(ts) = line.parsed_timestamp()
            && last_ts.is_none_or(|l| ts > l)
        {
            last_ts = Some(ts);
        }

        // Same id rule as RolloutReader::derive_session_id: the first
        // line's session_meta payload wins, else the filename stem.
        let id = first_line_meta_id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| crate::paths::session_id_from_stem(stem).to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });

        let (cwd, cli_version, git_branch, git_commit) = match &meta {
            Some(m) => (
                Some(m.cwd.clone()),
                Some(m.cli_version.clone()),
                m.git.as_ref().and_then(|g| g.branch.clone()),
                m.git.as_ref().and_then(|g| g.commit_hash.clone()),
            ),
            None => (None, None, None, None),
        };

        Ok(SessionMetadata {
            id,
            file_path: path.to_path_buf(),
            started_at,
            last_activity: last_ts,
            cwd,
            cli_version,
            first_user_message: first_user.or(first_user_fallback),
            git_branch,
            git_commit,
            line_count,
        })
    }

    pub fn session_exists(&self, session_id: &str) -> bool {
        self.resolver.find_rollout_file(session_id).is_ok()
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
}
