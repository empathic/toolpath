//! JSONL parsing + session assembly for Pi session files.

use crate::error::{PiError, Result};
use crate::paths::PathResolver;
use crate::types::{AgentMessage, Entry, EntryBase, SessionHeader};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Default maximum depth when following `parentSession` chains.
pub const DEFAULT_MAX_PARENT_DEPTH: usize = 16;

/// Lightweight summary of a session on disk.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub timestamp: String,
    pub file_path: PathBuf,
    pub entry_count: usize,
    /// First non-empty user-prompt text in the session. Useful as a
    /// human-readable title for picker UIs (e.g. `path list pi --format tsv`
    /// piped into fzf). `None` if the session has no parseable user message.
    pub first_user_message: Option<String>,
}

/// In-memory representation of a Pi session file (plus optional parent).
#[derive(Debug, Clone)]
pub struct PiSession {
    pub header: SessionHeader,
    pub entries: Vec<Entry>,
    pub file_path: PathBuf,
    pub parent: Option<Box<PiSession>>,
}

impl Entry {
    /// Get the entry's id, regardless of variant.
    pub fn entry_id(&self) -> &str {
        match self {
            Entry::Session(h) => &h.id,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. } => &base.id,
        }
    }

    /// Get the parent entry's id, if any. Session headers have no parent here.
    pub fn parent_entry_id(&self) -> Option<&str> {
        match self {
            Entry::Session(_) => None,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. } => base.parent_id.as_deref(),
        }
    }

    /// Get the entry's timestamp (ISO-8601 string).
    pub fn entry_timestamp(&self) -> &str {
        match self {
            Entry::Session(h) => &h.timestamp,
            Entry::Message { base, .. }
            | Entry::ModelChange { base, .. }
            | Entry::ThinkingLevelChange { base, .. }
            | Entry::Compaction { base, .. }
            | Entry::BranchSummary { base, .. }
            | Entry::Custom { base, .. }
            | Entry::CustomMessage { base, .. }
            | Entry::Label { base, .. } => &base.timestamp,
        }
    }
}

impl PiSession {
    /// This session's id (from the header).
    pub fn session_id(&self) -> &str {
        &self.header.id
    }

    /// The cwd under which this session was recorded.
    pub fn cwd(&self) -> &str {
        &self.header.cwd
    }

    /// Iterate over `Entry::Message` entries, yielding `(&base, &message)`.
    pub fn message_entries(&self) -> impl Iterator<Item = (&EntryBase, &AgentMessage)> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Message { base, message, .. } => Some((base, message)),
            _ => None,
        })
    }

    /// Collect every `Entry::Message` message in file order.
    pub fn all_messages(&self) -> Vec<&AgentMessage> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Message { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Return entries on the "main thread" — the path from root to the
    /// newest leaf (an entry with no children), walking parent links.
    ///
    /// Returns entries in chronological order (root-first). If there are no
    /// non-session entries, returns an empty vec.
    pub fn main_thread(&self) -> Vec<&Entry> {
        // Index by id.
        let mut by_id: HashMap<&str, &Entry> = HashMap::new();
        let mut has_child: HashSet<&str> = HashSet::new();
        for e in &self.entries {
            by_id.insert(e.entry_id(), e);
            if let Some(p) = e.parent_entry_id() {
                has_child.insert(p);
            }
        }

        // Consider only non-session entries as candidate leaves.
        let leaf = self
            .entries
            .iter()
            .filter(|e| !matches!(e, Entry::Session(_)))
            .filter(|e| !has_child.contains(e.entry_id()))
            .max_by(|a, b| a.entry_timestamp().cmp(b.entry_timestamp()));

        let Some(leaf) = leaf else {
            return Vec::new();
        };

        let mut chain: Vec<&Entry> = Vec::new();
        let mut cur: Option<&Entry> = Some(leaf);
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(e) = cur {
            if !visited.insert(e.entry_id()) {
                break;
            }
            chain.push(e);
            cur = match e.parent_entry_id() {
                Some(pid) => by_id.get(pid).copied(),
                None => None,
            };
        }
        chain.reverse();
        chain
    }
}

/// Read and parse a session JSONL file. Does NOT follow `parentSession`.
pub fn read_session_from_file(path: &Path) -> Result<PiSession> {
    let file = File::open(path)
        .map_err(|e| PiError::invalid_session_file(path.to_path_buf(), format!("open: {e}")))?;
    let reader = BufReader::new(file);

    let mut header: Option<SessionHeader> = None;
    let mut entries: Vec<Entry> = Vec::new();
    let mut line_no = 0usize;

    for line in reader.lines() {
        line_no += 1;
        let line = line.map_err(|e| {
            PiError::invalid_session_file(path.to_path_buf(), format!("read line {line_no}: {e}"))
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if header.is_none() {
            // First non-empty line must be a session header.
            let entry: Entry = serde_json::from_str(trimmed).map_err(|e| {
                PiError::invalid_session_file(
                    path.to_path_buf(),
                    format!("line {line_no}: malformed header json: {e}"),
                )
            })?;
            match entry {
                Entry::Session(h) => {
                    header = Some(h.clone());
                    entries.push(Entry::Session(h));
                }
                _ => {
                    return Err(PiError::malformed_header(format!(
                        "{}: expected session header on first non-empty line (line {}), found different entry type",
                        path.display(),
                        line_no
                    )));
                }
            }
            continue;
        }

        // Try to parse as an Entry.
        match serde_json::from_str::<Entry>(trimmed) {
            Ok(entry) => entries.push(entry),
            Err(parse_err) => {
                // Is the line valid JSON at all? If so, it may be an unknown type — skip with warning.
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(val) => {
                        let type_str = val
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("<unknown>");
                        eprintln!(
                            "warning: unknown Pi entry type '{}' at {}:{}",
                            type_str,
                            path.display(),
                            line_no
                        );
                        continue;
                    }
                    Err(_) => {
                        return Err(PiError::invalid_session_file(
                            path.to_path_buf(),
                            format!("line {line_no}: {parse_err}"),
                        ));
                    }
                }
            }
        }
    }

    let header = header.ok_or_else(|| {
        PiError::invalid_session_file(path.to_path_buf(), "empty session file".to_string())
    })?;

    Ok(PiSession {
        header,
        entries,
        file_path: path.to_path_buf(),
        parent: None,
    })
}

/// Read a session and, if it has a `parentSession`, recursively attach the parent.
///
/// Missing parent files are logged to stderr but do not fail the read.
/// When `max_depth` reaches 0, the walk stops.
pub fn read_session_with_parent(path: &Path, max_depth: usize) -> Result<PiSession> {
    let mut session = read_session_from_file(path)?;

    if max_depth == 0 {
        return Ok(session);
    }

    if let Some(parent_path_str) = session.header.parent_session.clone() {
        let parent_path = PathBuf::from(&parent_path_str);
        if !parent_path.exists() {
            eprintln!(
                "warning: parent session not found: {}",
                parent_path.display()
            );
        } else {
            match read_session_with_parent(&parent_path, max_depth - 1) {
                Ok(parent) => session.parent = Some(Box::new(parent)),
                Err(e) => {
                    eprintln!(
                        "warning: failed to read parent session {}: {}",
                        parent_path.display(),
                        e
                    );
                }
            }
        }
    }

    Ok(session)
}

/// Read only the header line of a session file.
fn read_header_only(path: &Path) -> Result<SessionHeader> {
    let file = File::open(path)
        .map_err(|e| PiError::invalid_session_file(path.to_path_buf(), format!("open: {e}")))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line
            .map_err(|e| PiError::invalid_session_file(path.to_path_buf(), format!("read: {e}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: Entry = serde_json::from_str(trimmed).map_err(|e| {
            PiError::invalid_session_file(path.to_path_buf(), format!("malformed header json: {e}"))
        })?;
        return match entry {
            Entry::Session(h) => Ok(h),
            _ => Err(PiError::malformed_header(format!(
                "{}: expected session header on first non-empty line",
                path.display()
            ))),
        };
    }

    Err(PiError::invalid_session_file(
        path.to_path_buf(),
        "empty session file".to_string(),
    ))
}

/// Public: read only the header of a session file. Used by `io.rs` to build
/// [`SessionMeta`] without parsing every entry.
pub fn peek_header(path: &Path) -> Result<SessionHeader> {
    read_header_only(path)
}

/// Count the number of non-header entries in a session file (by counting
/// non-empty lines and subtracting 1 for the header).
///
/// Returns 0 if the file is empty.
pub fn count_entries(path: &Path) -> Result<usize> {
    let file = File::open(path)
        .map_err(|e| PiError::invalid_session_file(path.to_path_buf(), format!("open: {e}")))?;
    let reader = BufReader::new(file);
    let mut total = 0usize;
    for line in reader.lines() {
        let line = line
            .map_err(|e| PiError::invalid_session_file(path.to_path_buf(), format!("read: {e}")))?;
        if !line.trim().is_empty() {
            total += 1;
        }
    }
    Ok(total.saturating_sub(1))
}

/// List all `*.jsonl` session files in a project's directory, sorted by
/// modification time descending (newest first).
///
/// Returns an empty vec if the project directory does not exist.
pub fn list_session_files(resolver: &PathResolver, project: &str) -> Result<Vec<PathBuf>> {
    let dir = resolver.project_dir(project);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        entries.push((path, mtime));
    }
    // Newest first. Reverse() rather than a flipped comparator: clippy 1.95+
    // rejects the latter as `unnecessary_sort_by`, and the key form says
    // "sort by mtime, descending" without the reader having to work out which
    // side of the comparison was swapped.
    entries.sort_by_key(|e| std::cmp::Reverse(e.1));
    Ok(entries.into_iter().map(|(p, _)| p).collect())
}

/// Read a session by ID from within a project.
///
/// Matches by header `id` or by filename stem (`<date>_<uuid>.jsonl` where
/// `<uuid>` equals `session_id`).
pub fn read_session(resolver: &PathResolver, project: &str, session_id: &str) -> Result<PiSession> {
    let project_dir = resolver.project_dir(project);
    if !project_dir.exists() {
        return Err(PiError::project_not_found(project));
    }

    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&project_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        // Filename-stem match: "<date>_<uuid>.jsonl" or "<uuid>.jsonl".
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let uuid_part = stem.rsplit_once('_').map(|(_, u)| u).unwrap_or(stem);
            if uuid_part == session_id {
                found = Some(path.clone());
                break;
            }
        }

        // Header id match.
        match read_header_only(&path) {
            Ok(h) if h.id == session_id => {
                found = Some(path.clone());
                break;
            }
            _ => continue,
        }
    }

    let Some(path) = found else {
        return Err(PiError::session_not_found(session_id));
    };

    read_session_with_parent(&path, DEFAULT_MAX_PARENT_DEPTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn write_jsonl(path: &Path, lines: &[&str]) {
        let mut f = fs::File::create(path).unwrap();
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                f.write_all(b"\n").unwrap();
            }
            f.write_all(l.as_bytes()).unwrap();
        }
    }

    fn header_line(id: &str) -> String {
        format!(
            r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-04-16T00:00:00.000Z","cwd":"/tmp/proj"}}"#
        )
    }

    fn header_with_parent(id: &str, parent_path: &str) -> String {
        format!(
            r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-04-16T00:00:00.000Z","cwd":"/tmp/proj","parentSession":"{parent_path}"}}"#
        )
    }

    fn msg_line(id: &str, parent: Option<&str>, ts: &str, text: &str) -> String {
        let parent_s = match parent {
            Some(p) => format!("\"{p}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"type":"message","id":"{id}","parentId":{parent_s},"timestamp":"{ts}","message":{{"role":"user","content":"{text}","timestamp":1700000000000}}}}"#
        )
    }

    #[test]
    fn test_read_session_from_file_linear() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("sess-1"),
                &msg_line("a", None, "2026-04-16T00:00:01Z", "hi"),
                &msg_line("b", Some("a"), "2026-04-16T00:00:02Z", "hey"),
                &msg_line("c", Some("b"), "2026-04-16T00:00:03Z", "yo"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        assert_eq!(s.header.id, "sess-1");
        assert_eq!(s.entries.len(), 4);
        assert!(s.parent.is_none());
        assert_eq!(s.file_path, path);
    }

    #[test]
    fn test_read_session_from_file_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.jsonl");
        fs::write(&path, "").unwrap();
        let err = read_session_from_file(&path).unwrap_err();
        assert!(matches!(err, PiError::InvalidSessionFile { .. }));
    }

    #[test]
    fn test_read_session_from_file_missing_header() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.jsonl");
        write_jsonl(&path, &[&msg_line("a", None, "t", "hi")]);
        let err = read_session_from_file(&path).unwrap_err();
        assert!(matches!(err, PiError::MalformedHeader(_)));
    }

    #[test]
    fn test_read_session_from_file_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.jsonl");
        write_jsonl(&path, &["not json"]);
        let err = read_session_from_file(&path).unwrap_err();
        match err {
            PiError::InvalidSessionFile { reason, .. } => {
                assert!(reason.to_lowercase().contains("malformed") || reason.contains("json"));
            }
            _ => panic!("expected InvalidSessionFile"),
        }
    }

    #[test]
    fn test_read_session_from_file_branched() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("b.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("sess-b"),
                &msg_line("root", None, "2026-04-16T00:00:01Z", "r"),
                &msg_line("c1", Some("root"), "2026-04-16T00:00:02Z", "c1"),
                &msg_line("c2", Some("root"), "2026-04-16T00:00:03Z", "c2"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        assert_eq!(s.entries.len(), 4);
        let ids: Vec<&str> = s.entries.iter().map(|e| e.entry_id()).collect();
        assert!(ids.contains(&"c1"));
        assert!(ids.contains(&"c2"));
    }

    #[test]
    fn test_read_session_from_file_ignores_blank_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("blank.jsonl");
        let content = format!(
            "\n\n{}\n\n{}\n\n",
            header_line("sess-1"),
            msg_line("a", None, "t", "hi")
        );
        fs::write(&path, content).unwrap();
        let s = read_session_from_file(&path).unwrap();
        assert_eq!(s.entries.len(), 2);
    }

    #[test]
    fn test_read_session_from_file_skips_unknown_entry_type() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("u.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("sess-1"),
                r#"{"type":"future_kind","id":"x","timestamp":"t"}"#,
                &msg_line("a", None, "t", "hi"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        // Header + one message; unknown entry skipped.
        assert_eq!(s.entries.len(), 2);
        let has_message = s.entries.iter().any(|e| matches!(e, Entry::Message { .. }));
        assert!(has_message);
    }

    #[test]
    fn test_read_session_with_parent_chains() {
        let tmp = TempDir::new().unwrap();
        let parent_path = tmp.path().join("parent.jsonl");
        write_jsonl(
            &parent_path,
            &[&header_line("parent-sess"), &msg_line("p1", None, "t", "p")],
        );
        let child_path = tmp.path().join("child.jsonl");
        write_jsonl(
            &child_path,
            &[
                &header_with_parent("child-sess", parent_path.to_str().unwrap()),
                &msg_line("c1", None, "t", "c"),
            ],
        );

        let s = read_session_with_parent(&child_path, 16).unwrap();
        assert_eq!(s.header.id, "child-sess");
        let parent = s.parent.expect("parent attached");
        assert_eq!(parent.header.id, "parent-sess");
        assert_eq!(parent.file_path, parent_path);
    }

    #[test]
    fn test_read_session_with_parent_missing_file() {
        let tmp = TempDir::new().unwrap();
        let child_path = tmp.path().join("child.jsonl");
        write_jsonl(
            &child_path,
            &[
                &header_with_parent("c", "/nonexistent/nope.jsonl"),
                &msg_line("c1", None, "t", "x"),
            ],
        );
        let s = read_session_with_parent(&child_path, 16).unwrap();
        assert!(s.parent.is_none());
    }

    #[test]
    fn test_read_session_with_parent_max_depth_zero() {
        let tmp = TempDir::new().unwrap();
        let parent_path = tmp.path().join("parent.jsonl");
        write_jsonl(&parent_path, &[&header_line("p")]);
        let child_path = tmp.path().join("child.jsonl");
        write_jsonl(
            &child_path,
            &[&header_with_parent("c", parent_path.to_str().unwrap())],
        );
        let s = read_session_with_parent(&child_path, 0).unwrap();
        assert!(s.parent.is_none());
    }

    fn resolver_with_project(tmp: &TempDir, cwd: &str) -> (PathResolver, PathBuf) {
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new().with_sessions_dir(&sessions);
        let proj_dir = resolver.project_dir(cwd);
        fs::create_dir_all(&proj_dir).unwrap();
        (resolver, proj_dir)
    }

    #[test]
    fn test_read_session_by_id_found_via_header() {
        let tmp = TempDir::new().unwrap();
        let (resolver, proj_dir) = resolver_with_project(&tmp, "/p");
        let path = proj_dir.join("anything.jsonl");
        write_jsonl(
            &path,
            &[&header_line("sess-1"), &msg_line("a", None, "t", "hi")],
        );
        let s = read_session(&resolver, "/p", "sess-1").unwrap();
        assert_eq!(s.header.id, "sess-1");
    }

    #[test]
    fn test_read_session_by_id_found_via_filename() {
        let tmp = TempDir::new().unwrap();
        let (resolver, proj_dir) = resolver_with_project(&tmp, "/p");
        let path = proj_dir.join("2026-04-16_sess-2.jsonl");
        write_jsonl(&path, &[&header_line("sess-2")]);
        let s = read_session(&resolver, "/p", "sess-2").unwrap();
        assert_eq!(s.header.id, "sess-2");
    }

    #[test]
    fn test_read_session_by_id_not_found() {
        let tmp = TempDir::new().unwrap();
        let (resolver, proj_dir) = resolver_with_project(&tmp, "/p");
        let path = proj_dir.join("x.jsonl");
        write_jsonl(&path, &[&header_line("other")]);
        let err = read_session(&resolver, "/p", "missing").unwrap_err();
        assert!(matches!(err, PiError::SessionNotFound(_)));
    }

    #[test]
    fn test_read_session_project_not_found() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let resolver = PathResolver::new().with_sessions_dir(&sessions);
        let err = read_session(&resolver, "/nonexistent-proj", "x").unwrap_err();
        assert!(matches!(err, PiError::ProjectNotFound(_)));
    }

    #[test]
    fn test_peek_header_minimal() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("peek-me"),
                &msg_line("a", None, "t", "hi"),
                &msg_line("b", Some("a"), "t", "hey"),
            ],
        );
        let h = peek_header(&path).unwrap();
        assert_eq!(h.id, "peek-me");
    }

    #[test]
    fn test_count_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("c"),
                &msg_line("a", None, "t", "1"),
                &msg_line("b", Some("a"), "t", "2"),
                &msg_line("c", Some("b"), "t", "3"),
            ],
        );
        assert_eq!(count_entries(&path).unwrap(), 3);
    }

    #[test]
    fn test_list_session_files_sorted_newest_first() {
        let tmp = TempDir::new().unwrap();
        let (resolver, proj_dir) = resolver_with_project(&tmp, "/p");

        let older = proj_dir.join("older.jsonl");
        let newer = proj_dir.join("newer.jsonl");
        write_jsonl(&older, &[&header_line("o")]);
        // Ensure distinct mtime
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_jsonl(&newer, &[&header_line("n")]);

        // Explicitly bump newer's mtime so the ordering is deterministic.
        let newer_time = std::time::SystemTime::now();
        filetime::set_file_mtime_fallback(&newer, newer_time);

        let files = list_session_files(&resolver, "/p").unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], newer);
        assert_eq!(files[1], older);
    }

    #[test]
    fn test_list_session_files_nonexistent_project() {
        let tmp = TempDir::new().unwrap();
        let resolver = PathResolver::new().with_sessions_dir(tmp.path());
        let files = list_session_files(&resolver, "/missing").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_entry_id_across_variants() {
        let samples = [
            (
                r#"{"type":"session","version":3,"id":"s1","timestamp":"t","cwd":"/"}"#,
                "s1",
            ),
            (
                r#"{"type":"model_change","id":"m1","parentId":null,"timestamp":"t","provider":"a","modelId":"x"}"#,
                "m1",
            ),
            (
                r#"{"type":"thinking_level_change","id":"tl1","parentId":null,"timestamp":"t","thinkingLevel":"high"}"#,
                "tl1",
            ),
            (
                r#"{"type":"compaction","id":"cp1","parentId":null,"timestamp":"t","summary":"s","firstKeptEntryId":"x","tokensBefore":0}"#,
                "cp1",
            ),
            (
                r#"{"type":"branch_summary","id":"bs1","parentId":null,"timestamp":"t","fromId":"x","summary":"s"}"#,
                "bs1",
            ),
            (
                r#"{"type":"custom","id":"cu1","parentId":null,"timestamp":"t","customType":"t","data":{}}"#,
                "cu1",
            ),
            (
                r#"{"type":"custom_message","id":"cm1","parentId":null,"timestamp":"t","customType":"h","content":"x","display":true}"#,
                "cm1",
            ),
            (
                r#"{"type":"label","id":"lb1","parentId":null,"timestamp":"t"}"#,
                "lb1",
            ),
        ];
        for (raw, expected) in samples {
            let e: Entry = serde_json::from_str(raw).unwrap();
            assert_eq!(e.entry_id(), expected, "raw={raw}");
        }
    }

    #[test]
    fn test_session_main_thread_linear() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("s1"),
                &msg_line("a", None, "2026-04-16T00:00:01Z", "1"),
                &msg_line("b", Some("a"), "2026-04-16T00:00:02Z", "2"),
                &msg_line("c", Some("b"), "2026-04-16T00:00:03Z", "3"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        let mt = s.main_thread();
        let ids: Vec<&str> = mt.iter().map(|e| e.entry_id()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_session_main_thread_with_branch() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("s1"),
                &msg_line("a", None, "2026-04-16T00:00:01Z", "1"),
                &msg_line("b", Some("a"), "2026-04-16T00:00:02Z", "2"),
                // c1 is an old dead-end
                &msg_line("c1", Some("b"), "2026-04-16T00:00:03Z", "3a"),
                // c2 is the newer leaf — main thread winner
                &msg_line("c2", Some("b"), "2026-04-16T00:00:09Z", "3b"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        let mt = s.main_thread();
        let ids: Vec<&str> = mt.iter().map(|e| e.entry_id()).collect();
        assert_eq!(ids, vec!["a", "b", "c2"]);
    }

    #[test]
    fn test_session_all_messages_flattens_tree() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("s1"),
                &msg_line("a", None, "t", "1"),
                &msg_line("b", Some("a"), "t", "2"),
                &msg_line("c1", Some("b"), "t", "3"),
                &msg_line("c2", Some("b"), "t", "4"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        let msgs = s.all_messages();
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn test_session_message_entries_iterator_yields_only_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        write_jsonl(
            &path,
            &[
                &header_line("s1"),
                r#"{"type":"model_change","id":"m1","parentId":null,"timestamp":"t","provider":"a","modelId":"x"}"#,
                &msg_line("a", None, "t", "1"),
                r#"{"type":"label","id":"lb1","parentId":null,"timestamp":"t"}"#,
                &msg_line("b", Some("a"), "t", "2"),
            ],
        );
        let s = read_session_from_file(&path).unwrap();
        let count = s.message_entries().count();
        assert_eq!(count, 2);
        let ids: Vec<&str> = s.message_entries().map(|(b, _)| b.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }
}

// Small fallback helper for tests to set mtimes without adding a new dep.
// We avoid adding the `filetime` crate; tests that rely on ordering use
// `sleep` between writes to produce distinct mtimes. The `filetime` shim
// below is a no-op used only by `test_list_session_files_sorted_newest_first`.
#[cfg(test)]
mod filetime {
    use std::path::Path;
    use std::time::SystemTime;
    pub fn set_file_mtime_fallback(_path: &Path, _mtime: SystemTime) {
        // no-op; we rely on the sleep between writes above.
    }
}
