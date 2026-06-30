//! Reading OpenClaw session JSONL: parse the header + entry tree, follow
//! `parentSession` chains, and resolve the visible-leaf thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::{OpenClawError, Result};
use crate::paths::{ParsedKey, SessionsIndex};
use crate::types::{Entry, SessionHeader, SUPPORTED_VERSION};

/// A parsed OpenClaw session: its header, the entry tree, and (optionally) the
/// routing identity recovered from `sessions.json`.
#[derive(Debug, Clone)]
pub struct OpenClawSession {
    /// The session header (line 1).
    pub header: SessionHeader,
    /// All entries after the header, in file order.
    pub entries: Vec<Entry>,
    /// Path the session was read from.
    pub file_path: PathBuf,
    /// Parent session (when this session was forked via `parentSession`).
    pub parent: Option<Box<OpenClawSession>>,
    /// The routing key from `sessions.json`, if found.
    pub session_key: Option<String>,
    /// Parsed components of [`Self::session_key`].
    pub parsed_key: Option<ParsedKey>,
}

/// Read a single session file (does not follow `parentSession`).
pub fn read_session_from_file(path: &Path) -> Result<OpenClawSession> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());

    let header_line = lines
        .next()
        .ok_or_else(|| OpenClawError::invalid_session_file(path, "empty file"))?;
    let header = parse_header(header_line, path)?;
    if header.version != SUPPORTED_VERSION {
        return Err(OpenClawError::UnsupportedVersion(header.version));
    }

    let mut entries = Vec::new();
    for line in lines {
        match serde_json::from_str::<Entry>(line) {
            Ok(Entry::Session(_)) => {} // stray header line; ignore
            Ok(entry) => entries.push(entry),
            Err(_) => {
                // Tolerate unknown future entry types: if the line is valid JSON
                // (just a `type` we don't model), skip it; otherwise it's
                // genuinely malformed.
                if serde_json::from_str::<serde_json::Value>(line).is_err() {
                    return Err(OpenClawError::invalid_session_file(
                        path,
                        "malformed JSON entry line",
                    ));
                }
            }
        }
    }

    Ok(OpenClawSession {
        header,
        entries,
        file_path: path.to_path_buf(),
        parent: None,
        session_key: None,
        parsed_key: None,
    })
}

/// Read a session and follow `parentSession` up to `max_depth` parents.
pub fn read_session_with_parent(path: &Path, max_depth: usize) -> Result<OpenClawSession> {
    let mut session = read_session_from_file(path)?;
    if max_depth == 0 {
        return Ok(session);
    }
    if let Some(parent_path) = session.header.parent_session.clone() {
        match read_session_with_parent(Path::new(&parent_path), max_depth - 1) {
            Ok(parent) => session.parent = Some(Box::new(parent)),
            Err(e) => eprintln!("warning: failed to read parent session {parent_path}: {e}"),
        }
    }
    Ok(session)
}

fn parse_header(line: &str, path: &Path) -> Result<SessionHeader> {
    match serde_json::from_str::<Entry>(line) {
        Ok(Entry::Session(h)) => Ok(h),
        Ok(_) => Err(OpenClawError::malformed_header(
            "first line is not a session header",
        )),
        Err(e) => Err(OpenClawError::invalid_session_file(
            path,
            format!("bad header line: {e}"),
        )),
    }
}

impl OpenClawSession {
    /// Recover the routing key (channel/peer) for this session from a
    /// `sessions.json` index in the session file's directory, if present.
    pub fn attach_routing_key(&mut self) {
        if let Some(dir) = self.file_path.parent()
            && let Some(index) = SessionsIndex::load(dir)
            && let Some((key, parsed)) = index.routing_key_for(&self.header.id)
        {
            self.session_key = Some(key);
            self.parsed_key = Some(parsed);
        }
    }

    /// The session id chain (oldest first), following `parentSession`.
    pub fn session_id_chain(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(parent) = &self.parent {
            ids.extend(parent.session_id_chain());
        }
        ids.push(self.header.id.clone());
        ids
    }

    /// The entries on the live conversation, root-to-leaf.
    ///
    /// The visible head is the target of the last `leaf` entry; failing that,
    /// the last non-leaf entry. From there we walk `parentId` to the root and
    /// reverse, so the result is the live thread in chronological order.
    pub fn main_thread(&self) -> Vec<&Entry> {
        let mut by_id: HashMap<&str, &Entry> = HashMap::new();
        for e in &self.entries {
            if let Some(b) = e.base() {
                by_id.insert(b.id.as_str(), e);
            }
        }

        let leaf_target = self.entries.iter().rev().find_map(|e| match e {
            Entry::Leaf {
                target_id: Some(t), ..
            } => Some(t.as_str()),
            _ => None,
        });
        let start = leaf_target.or_else(|| {
            self.entries.iter().rev().find_map(|e| match e {
                Entry::Leaf { .. } => None,
                _ => e.base().map(|b| b.id.as_str()),
            })
        });

        let mut chain: Vec<&Entry> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cur = start;
        while let Some(id) = cur {
            if !seen.insert(id) {
                break; // cycle guard
            }
            match by_id.get(id) {
                Some(e) => {
                    chain.push(*e);
                    cur = e.base().and_then(|b| b.parent_id.as_deref());
                }
                None => break,
            }
        }
        chain.reverse();
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentMessage;

    fn fixture() -> OpenClawSession {
        read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).unwrap()
    }

    #[test]
    fn reads_dm_fixture() {
        let s = fixture();
        assert_eq!(s.header.version, 3);
        assert_eq!(s.header.id, "sess-abc");
        assert_eq!(s.header.cwd, "/home/u/proj");
        // 7 entries (blank line tolerated; header excluded): 3 messages-user/asst,
        // model_change, 2 tool results, leaf.
        assert!(s.entries.len() >= 6);
        assert!(
            s.entries
                .iter()
                .any(|e| matches!(e, Entry::Message { .. }))
        );
    }

    #[test]
    fn blank_lines_tolerated() {
        // The fixture contains a blank line between entries; if it weren't
        // tolerated, parsing would error.
        assert!(read_session_from_file(Path::new("tests/fixtures/dm_session.jsonl")).is_ok());
    }

    #[test]
    fn rejects_non_v3() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("old.jsonl");
        std::fs::write(
            &p,
            r#"{"type":"session","version":2,"id":"x","timestamp":"t","cwd":"/"}"#,
        )
        .unwrap();
        let err = read_session_from_file(&p).unwrap_err();
        assert!(matches!(err, OpenClawError::UnsupportedVersion(2)));
    }

    #[test]
    fn tolerates_unknown_entry_type() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"session\",\"version\":3,\"id\":\"x\",\"timestamp\":\"t\",\"cwd\":\"/\"}\n\
             {\"type\":\"future_thing\",\"id\":\"f\",\"parentId\":null,\"timestamp\":\"t\"}\n\
             {\"type\":\"message\",\"id\":\"m\",\"parentId\":null,\"timestamp\":\"t\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
        )
        .unwrap();
        let s = read_session_from_file(&p).unwrap();
        // unknown skipped, message kept
        assert_eq!(s.entries.len(), 1);
        assert!(matches!(&s.entries[0], Entry::Message { .. }));
    }

    #[test]
    fn main_thread_follows_leaf_to_root() {
        let s = fixture();
        let thread = s.main_thread();
        // root is the user message e10
        let first = thread.first().unwrap();
        assert_eq!(first.base().unwrap().id, "e10");
        assert!(matches!(
            first,
            Entry::Message {
                message: AgentMessage::User { .. },
                ..
            }
        ));
        // the last content entry is the edit tool result e15 (leaf target)
        let last = thread.last().unwrap();
        assert_eq!(last.base().unwrap().id, "e15");
        // the read tool result is on the thread
        assert!(thread.iter().any(|e| e.base().unwrap().id == "e13"));
    }

    #[test]
    fn attach_routing_key_recovers_channel() {
        let mut s = fixture();
        s.attach_routing_key();
        let key = s.parsed_key.expect("routing key");
        assert_eq!(key.channel.as_deref(), Some("whatsapp"));
        assert_eq!(key.peer_id.as_deref(), Some("15555550123"));
    }
}
