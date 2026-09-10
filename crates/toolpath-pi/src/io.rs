//! Filesystem enumeration for Pi sessions.
//!
//! This module produces lightweight [`SessionMeta`] summaries by scanning the
//! on-disk layout without reading full session bodies.

use crate::error::{PiError, Result};
use crate::paths::PathResolver;
use crate::reader::SessionMeta;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::SystemTime;

/// List project cwd paths with sessions on disk.
pub fn list_projects(resolver: &PathResolver) -> Result<Vec<String>> {
    resolver.list_projects().map_err(PiError::from)
}

/// List sessions for a project, newest first.
///
/// Each session is summarized into a [`SessionMeta`] by peeking at the header
/// line (if any) and counting non-empty lines. Files that cannot be opened are
/// logged to stderr and skipped — a single unreadable file does not fail the
/// listing.
pub fn list_sessions(resolver: &PathResolver, project: &str) -> Result<Vec<SessionMeta>> {
    let project_dir = resolver.project_dir(project);
    if !project_dir.exists() {
        return Err(PiError::project_not_found(project));
    }

    let mut metas: Vec<(SessionMeta, SystemTime)> = Vec::new();

    let read_dir = std::fs::read_dir(&project_dir)?;
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!(
                    "warning: skipping entry in {}: {}",
                    project_dir.display(),
                    err
                );
                continue;
            }
        };

        let path = entry.path();

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                eprintln!("warning: skipping {}: {}", path.display(), err);
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }

        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let header_line = match extract_header_line(&path) {
            Ok(line) => line,
            Err(err) => {
                eprintln!("warning: skipping {}: {}", path.display(), err);
                continue;
            }
        };

        let parsed = header_line.as_deref().and_then(parse_header);

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        let total_nonempty = match count_nonempty_lines(&path) {
            Ok(n) => n,
            Err(err) => {
                eprintln!("warning: skipping {}: {}", path.display(), err);
                continue;
            }
        };

        let (id, timestamp, entry_count, cwd) = match parsed {
            Some((id, ts, cwd)) => {
                let ec = total_nonempty.saturating_sub(1);
                (id, ts, ec, cwd)
            }
            None => {
                let id = fallback_id_from_stem(stem);
                let ts = file_mtime_rfc3339(&path).unwrap_or_else(|| String::from(""));
                (id, ts, total_nonempty, None)
            }
        };

        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let first_user_message = match extract_first_user_message(&path) {
            Ok(s) => s,
            Err(err) => {
                eprintln!(
                    "warning: skipping first-user-message extraction for {}: {}",
                    path.display(),
                    err
                );
                None
            }
        };

        metas.push((
            SessionMeta {
                id,
                timestamp,
                file_path: path,
                entry_count,
                first_user_message,
                cwd,
            },
            mtime,
        ));
    }

    // Descending by timestamp, mtime tiebreaker.
    metas.sort_by(|a, b| {
        b.0.timestamp
            .cmp(&a.0.timestamp)
            .then_with(|| b.1.cmp(&a.1))
    });

    Ok(metas.into_iter().map(|(m, _)| m).collect())
}

/// Open `path` and return its first non-empty line, if any.
fn extract_header_line(path: &Path) -> std::io::Result<Option<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
    Ok(None)
}

/// If the line is a `{"type":"session", ...}` header, return `(id, timestamp)`.
/// (id, timestamp, cwd) from a session header line; `None` when the
/// line is not a header or lacks id/timestamp.
fn parse_header(line: &str) -> Option<(String, String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("session") {
        return None;
    }
    let id = obj.get("id")?.as_str()?.to_string();
    let timestamp = obj.get("timestamp")?.as_str()?.to_string();
    let cwd = obj
        .get("cwd")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Some((id, timestamp, cwd))
}

/// Strip a leading timestamp prefix from a filename stem.
///
/// * Zero underscores → return full stem.
/// * One or more underscores → return everything after the first `_`.
fn fallback_id_from_stem(stem: &str) -> String {
    match stem.find('_') {
        Some(idx) => stem[idx + 1..].to_string(),
        None => stem.to_string(),
    }
}

/// Count non-empty lines in a file.
fn count_nonempty_lines(path: &Path) -> std::io::Result<usize> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut n = 0usize;
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

/// Walk the JSONL until we find a `{"type":"message"}` entry whose
/// `message.role == "user"` and whose content has non-empty text. Returns
/// `None` if no user prompt is present.
///
/// We don't deserialize into the full `Entry` enum here — Pi messages have
/// a varied schema and partial JSON parsing is enough for a "title" hint.
fn extract_first_user_message(path: &Path) -> std::io::Result<Option<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = match v.as_object() {
            Some(o) => o,
            None => continue,
        };
        if obj.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let msg = match obj.get("message").and_then(|m| m.as_object()) {
            Some(m) => m,
            None => continue,
        };
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let text = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|b| {
                    let bo = b.as_object()?;
                    if bo.get("type").and_then(|t| t.as_str()) == Some("text") {
                        bo.get("text").and_then(|t| t.as_str()).map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    Ok(None)
}

/// Return the mtime of `path` formatted as RFC 3339, if it can be read.
fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(mtime).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PiError;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn resolver_with(sessions_dir: &Path) -> PathResolver {
        PathResolver::new().with_sessions_dir(sessions_dir)
    }

    #[test]
    fn test_list_projects_empty() {
        let temp = TempDir::new().unwrap();
        let resolver = resolver_with(temp.path());
        let projects = list_projects(&resolver).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_list_projects_returns_projects_sorted() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("--home-bob-repo--")).unwrap();
        fs::create_dir(temp.path().join("--Users-alex-proj--")).unwrap();
        let resolver = resolver_with(temp.path());
        let projects = list_projects(&resolver).unwrap();
        assert_eq!(
            projects,
            vec!["/Users/alex/proj".to_string(), "/home/bob/repo".to_string(),]
        );
    }

    #[test]
    fn test_list_sessions_project_not_found() {
        let temp = TempDir::new().unwrap();
        let resolver = resolver_with(temp.path());
        let err = list_sessions(&resolver, "/does/not/exist").unwrap_err();
        match err {
            PiError::ProjectNotFound(p) => assert_eq!(p, "/does/not/exist"),
            other => panic!("expected ProjectNotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_list_sessions_empty_project() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert!(sessions.is_empty());
    }

    fn write_file(path: &Path, contents: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn test_list_sessions_returns_meta() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        let file = proj_dir.join("2026-04-16_s1.jsonl");
        write_file(
            &file,
            "{\"type\":\"session\",\"id\":\"s1\",\"timestamp\":\"2026-04-16T10:00:00Z\",\"cwd\":\"/p\",\"version\":3}\n\
             {\"type\":\"message\",\"id\":\"m1\"}\n\
             {\"type\":\"message\",\"id\":\"m2\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "s1");
        assert_eq!(s.timestamp, "2026-04-16T10:00:00Z");
        assert_eq!(s.entry_count, 2);
        assert_eq!(s.cwd.as_deref(), Some("/p"));
        assert!(s.file_path.to_string_lossy().ends_with(".jsonl"));
    }

    #[test]
    fn test_list_sessions_sorts_descending_by_timestamp() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("older.jsonl"),
            "{\"type\":\"session\",\"id\":\"old\",\"timestamp\":\"2026-01-01T00:00:00Z\"}\n",
        );
        write_file(
            &proj_dir.join("newer.jsonl"),
            "{\"type\":\"session\",\"id\":\"new\",\"timestamp\":\"2026-06-01T00:00:00Z\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "new");
        assert_eq!(sessions[1].id, "old");
    }

    #[test]
    fn test_list_sessions_fallback_id_from_filename() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("2026-04-16_fallback-id.jsonl"),
            "{\"type\":\"message\",\"id\":\"x\",\"timestamp\":\"t\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "fallback-id");
    }

    #[test]
    fn test_list_sessions_fallback_id_no_underscore() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("single.jsonl"),
            "{\"type\":\"message\",\"id\":\"x\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "single");
    }

    #[test]
    fn test_list_sessions_fallback_id_multiple_underscores() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("a_b_c.jsonl"),
            "{\"type\":\"message\",\"id\":\"x\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "b_c");
    }

    #[test]
    fn test_list_sessions_fallback_timestamp_is_mtime() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("x.jsonl"),
            "{\"type\":\"message\",\"id\":\"x\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        // Should parse as RFC 3339.
        let parsed = chrono::DateTime::parse_from_rfc3339(&sessions[0].timestamp);
        assert!(
            parsed.is_ok(),
            "expected RFC 3339 timestamp, got {:?}",
            sessions[0].timestamp
        );
    }

    #[test]
    fn test_list_sessions_entry_count_subtracts_header() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("s.jsonl"),
            "{\"type\":\"session\",\"id\":\"s\",\"timestamp\":\"2026-04-16T10:00:00Z\"}\n\
             {\"type\":\"message\",\"id\":\"1\"}\n\
             {\"type\":\"message\",\"id\":\"2\"}\n\
             {\"type\":\"message\",\"id\":\"3\"}\n\
             {\"type\":\"message\",\"id\":\"4\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].entry_count, 4);
    }

    #[test]
    fn test_list_sessions_entry_count_without_header() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(
            &proj_dir.join("x.jsonl"),
            "{\"type\":\"message\",\"id\":\"1\"}\n\
             {\"type\":\"message\",\"id\":\"2\"}\n\
             {\"type\":\"message\",\"id\":\"3\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].entry_count, 3);
    }

    #[test]
    fn test_list_sessions_ignores_non_jsonl_files() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(&proj_dir.join("notes.txt"), "hello\n");
        write_file(
            &proj_dir.join("session.jsonl"),
            "{\"type\":\"session\",\"id\":\"s\",\"timestamp\":\"2026-04-16T10:00:00Z\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s");
    }

    #[test]
    fn test_list_sessions_ignores_subdirectories() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        fs::create_dir(proj_dir.join("subdir")).unwrap();
        write_file(
            &proj_dir.join("s.jsonl"),
            "{\"type\":\"session\",\"id\":\"s\",\"timestamp\":\"2026-04-16T10:00:00Z\"}\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_list_sessions_skips_empty_files() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        write_file(&proj_dir.join("empty.jsonl"), "");
        let resolver = resolver_with(temp.path());
        // Should not panic, should include the file with fallback id.
        let sessions = list_sessions(&resolver, "/p").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "empty");
        assert_eq!(sessions[0].entry_count, 0);
    }

    #[test]
    fn test_list_sessions_warns_on_weird_file_but_continues() {
        let temp = TempDir::new().unwrap();
        let proj_dir = temp.path().join("--p--");
        fs::create_dir(&proj_dir).unwrap();
        // One valid session, one with un-parseable first line.
        write_file(
            &proj_dir.join("good.jsonl"),
            "{\"type\":\"session\",\"id\":\"good\",\"timestamp\":\"2026-04-16T10:00:00Z\"}\n",
        );
        write_file(
            &proj_dir.join("weird.jsonl"),
            "not-json at all\nmore junk\n",
        );
        let resolver = resolver_with(temp.path());
        let sessions = list_sessions(&resolver, "/p").unwrap();
        // Both files should appear; weird one falls back on filename + mtime.
        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"good"));
        assert!(ids.contains(&"weird"));
    }
}
