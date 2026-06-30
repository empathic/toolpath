//! Lightweight session listing and metadata extraction (no full parse).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::Result;
use crate::error::OpenClawError;
use crate::paths::{PathResolver, SessionsIndex};

/// Lightweight metadata for one session, gathered without a full parse.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Session id (header `id`, or filename stem as a fallback).
    pub id: String,
    /// Session timestamp (header `timestamp`, or file mtime as a fallback).
    pub timestamp: String,
    /// Path to the transcript file.
    pub file_path: PathBuf,
    /// Number of non-header entries.
    pub entry_count: usize,
    /// First non-empty user message text, for topic display.
    pub first_user_message: Option<String>,
    /// Working directory recorded in the header.
    pub cwd: Option<String>,
    /// Routing key from `sessions.json`, if any.
    pub session_key: Option<String>,
    /// The agent bucket this session was found under.
    pub agent_id: String,
}

/// List sessions under one agent, newest first.
pub fn list_sessions(resolver: &PathResolver, agent_id: &str) -> Result<Vec<SessionMeta>> {
    let dir = resolver.agent_sessions_dir(agent_id);
    if !dir.exists() {
        return Err(OpenClawError::agent_not_found(agent_id));
    }
    let index = SessionsIndex::load(&dir);

    let mut rows: Vec<(SessionMeta, SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if !is_transcript_file(&path) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let header = peek_header(&path);
        let (id, timestamp, cwd) = match header {
            Some((id, ts, cwd)) => (id, ts, cwd),
            None => (stem, file_mtime_rfc3339(&path).unwrap_or_default(), None),
        };
        let entry_count = count_nonempty_lines(&path)?.saturating_sub(1);
        let first_user_message = extract_first_user_message(&path)?;
        let session_key = index
            .as_ref()
            .and_then(|i| i.routing_key_for(&id).map(|(k, _)| k));
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        rows.push((
            SessionMeta {
                id,
                timestamp,
                file_path: path,
                entry_count,
                first_user_message,
                cwd,
                session_key,
                agent_id: agent_id.to_string(),
            },
            mtime,
        ));
    }

    rows.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp).then_with(|| b.1.cmp(&a.1)));
    Ok(rows.into_iter().map(|(m, _)| m).collect())
}

/// List sessions across every agent bucket, newest first.
pub fn list_all_sessions(resolver: &PathResolver) -> Result<Vec<SessionMeta>> {
    let mut all = Vec::new();
    for agent_id in resolver.list_agent_ids()? {
        if let Ok(rows) = list_sessions(resolver, &agent_id) {
            all.extend(rows);
        }
    }
    all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(all)
}

/// True for a session transcript `*.jsonl`, excluding `*.trajectory.jsonl`
/// telemetry sidecars.
fn is_transcript_file(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return false;
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    !stem.ends_with(".trajectory")
}

/// Read just the header line and pull `id` / `timestamp` / `cwd`.
fn peek_header(path: &Path) -> Option<(String, String, Option<String>)> {
    let content = std::fs::read_to_string(path).ok()?;
    let line = content.lines().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("session") {
        return None;
    }
    let id = obj.get("id")?.as_str()?.to_string();
    let timestamp = obj
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let cwd = obj
        .get("cwd")
        .and_then(|c| c.as_str())
        .map(str::to_string);
    Some((id, timestamp, cwd))
}

fn count_nonempty_lines(path: &Path) -> Result<usize> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().filter(|l| !l.trim().is_empty()).count())
}

fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = mtime.into();
    Some(dt.to_rfc3339())
}

/// Extract the first non-empty user-message text (string or text blocks).
pub fn extract_first_user_message(path: &Path) -> Result<Option<String>> {
    let content = std::fs::read_to_string(path)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp state dir with the DM fixture under agents/main/sessions.
    fn temp_state() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/main/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = std::fs::read("tests/fixtures/dm_session.jsonl").unwrap();
        std::fs::write(dir.join("sess-abc.jsonl"), fixture).unwrap();
        std::fs::copy("tests/fixtures/sessions.json", dir.join("sessions.json")).unwrap();
        // a trajectory sidecar that must be ignored
        std::fs::write(dir.join("sess-abc.trajectory.jsonl"), "{}\n").unwrap();
        tmp
    }

    #[test]
    fn lists_sessions_with_metadata() {
        let tmp = temp_state();
        let resolver = PathResolver::with_state_dir(tmp.path());
        let rows = list_sessions(&resolver, "main").unwrap();
        assert_eq!(rows.len(), 1, "trajectory sidecar must be excluded");
        let m = &rows[0];
        assert_eq!(m.id, "sess-abc");
        assert_eq!(m.first_user_message.as_deref(), Some("Fix the bug in x.ts"));
        assert_eq!(m.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(
            m.session_key.as_deref(),
            Some("agent:main:whatsapp:direct:15555550123")
        );
        assert!(m.entry_count >= 6);
    }

    #[test]
    fn lists_all_sessions_across_agents() {
        let tmp = temp_state();
        let resolver = PathResolver::with_state_dir(tmp.path());
        let rows = list_all_sessions(&resolver).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "main");
    }

    #[test]
    fn first_user_message_from_string_content() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"session\",\"version\":3,\"id\":\"x\",\"timestamp\":\"t\",\"cwd\":\"/\"}\n\
             {\"type\":\"message\",\"id\":\"m\",\"parentId\":null,\"timestamp\":\"t\",\"message\":{\"role\":\"user\",\"content\":\"hello there\",\"timestamp\":1}}\n",
        )
        .unwrap();
        assert_eq!(
            extract_first_user_message(&p).unwrap().as_deref(),
            Some("hello there")
        );
    }
}
