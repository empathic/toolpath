//! Session chain resolution for Claude Code's file rotation.
//!
//! When Claude Code rotates to a new JSONL file (plan-mode exit, context
//! overflow), the new file's first real entry carries the **old** session
//! ID — a "bridge entry" linking the two files. This module detects those
//! links and resolves full chains so callers can treat a multi-file
//! conversation as a single logical session.

use crate::error::Result;
use crate::paths::PathResolver;
use crate::reader::ConversationReader;
use crate::types::ConversationEntry;
use std::collections::{HashMap, HashSet};

/// Resolve a chain using a pre-built succession map.
///
/// Walks backwards from `session_id` to the head, then forwards to the
/// tail. Returns the chain in chronological order (oldest first).
pub(crate) fn resolve_chain_with_map(
    succession: &HashMap<String, String>,
    session_id: &str,
) -> Vec<String> {
    // Walk backwards to find the chain head (the segment with no predecessor).
    let reverse: HashMap<&str, &str> = succession
        .iter()
        .map(|(pred, succ)| (succ.as_str(), pred.as_str()))
        .collect();

    let mut visited = HashSet::new();
    let mut head = session_id;
    visited.insert(head);
    while let Some(&pred) = reverse.get(head) {
        if !visited.insert(pred) {
            break; // cycle detected
        }
        head = pred;
    }

    // Walk forward from head collecting the chain.
    let mut seen = HashSet::new();
    let mut chain = vec![head.to_string()];
    seen.insert(head);
    let mut current = head;
    while let Some(next) = succession.get(current) {
        if !seen.insert(next.as_str()) {
            break; // cycle detected
        }
        chain.push(next.clone());
        current = next;
    }

    chain
}

/// Cached index of session succession relationships.
///
/// Incrementally built: once a file has been checked and found not to be
/// a successor, it's never re-checked (`non_successors` is an immutable
/// property of a file's first entry).
#[derive(Debug, Clone)]
pub(crate) struct ChainIndex {
    /// predecessor session_id → successor file stem
    succession: HashMap<String, String>,
    /// successor file stem → predecessor session_id
    reverse: HashMap<String, String>,
    /// File stems confirmed not to be successors (immutable property)
    non_successors: HashSet<String>,
    /// All file stems we've ever checked
    known_files: HashSet<String>,
}

impl ChainIndex {
    pub(crate) fn new() -> Self {
        Self {
            succession: HashMap::new(),
            reverse: HashMap::new(),
            non_successors: HashSet::new(),
            known_files: HashSet::new(),
        }
    }

    /// Scan for new files and classify them. Files already in
    /// `known_files` are skipped entirely.
    pub(crate) fn refresh(
        &mut self,
        resolver: &PathResolver,
        project_path: &str,
    ) -> Result<()> {
        let sessions = resolver.list_conversations(project_path)?;

        for file_stem in &sessions {
            if self.known_files.contains(file_stem.as_str()) {
                continue;
            }
            self.known_files.insert(file_stem.clone());

            let path = resolver.conversation_file(project_path, file_stem)?;
            if let Some(first_sid) = ConversationReader::read_first_session_id(&path) {
                if first_sid != *file_stem {
                    // This file is a successor of first_sid
                    self.succession.insert(first_sid.clone(), file_stem.clone());
                    self.reverse.insert(file_stem.clone(), first_sid);
                } else {
                    self.non_successors.insert(file_stem.clone());
                }
            } else {
                self.non_successors.insert(file_stem.clone());
            }
        }

        Ok(())
    }

    /// Resolve the full chain containing `session_id` (oldest first).
    pub(crate) fn resolve_chain(&self, session_id: &str) -> Vec<String> {
        resolve_chain_with_map(&self.succession, session_id)
    }

    /// Return the chain head (earliest segment) for `session_id`.
    #[allow(dead_code)]
    pub(crate) fn head_for(&self, session_id: &str) -> String {
        let chain = self.resolve_chain(session_id);
        chain
            .into_iter()
            .next()
            .unwrap_or_else(|| session_id.to_string())
    }

    /// Immediate successor of `session_id`, if any.
    pub(crate) fn successor_of(&self, session_id: &str) -> Option<&str> {
        self.succession.get(session_id).map(|s| s.as_str())
    }

    /// All chain heads — file stems that are not successors of another.
    pub(crate) fn chain_heads(&self) -> Vec<String> {
        self.known_files
            .iter()
            .filter(|stem| !self.reverse.contains_key(stem.as_str()))
            .cloned()
            .collect()
    }

    /// Whether `session_id` is a successor file (not a chain head).
    #[allow(dead_code)]
    pub(crate) fn is_successor(&self, session_id: &str) -> bool {
        self.reverse.contains_key(session_id)
    }
}

/// Test whether an entry is a bridge entry (its `session_id` differs
/// from the owning conversation's session_id).
pub(crate) fn is_bridge_entry(entry: &ConversationEntry, owner_session_id: &str) -> bool {
    entry
        .session_id
        .as_ref()
        .is_some_and(|sid| !sid.is_empty() && sid != owner_session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_chain_env() -> (TempDir, PathResolver) {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        (temp, resolver)
    }

    fn write_session(resolver: &PathResolver, session_id: &str, entries: &[&str]) {
        let path = resolver
            .conversation_file("/test/project", session_id)
            .unwrap();
        fs::write(&path, entries.join("\n")).unwrap();
    }

    // ── ChainIndex tests ─────────────────────────────────────────────

    #[test]
    fn test_chain_index_basic_build() {
        let (_temp, resolver) = setup_chain_env();

        write_session(
            &resolver,
            "session-a",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-b",
            &[
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"New"}}"#,
            ],
        );

        let mut index = ChainIndex::new();
        index.refresh(&resolver, "/test/project").unwrap();

        assert_eq!(index.successor_of("session-a"), Some("session-b"));
        assert!(index.successor_of("session-b").is_none());
        assert!(index.is_successor("session-b"));
        assert!(!index.is_successor("session-a"));
        assert_eq!(index.head_for("session-b"), "session-a");
        assert_eq!(index.head_for("session-a"), "session-a");
    }

    #[test]
    fn test_chain_index_incremental_refresh() {
        let (_temp, resolver) = setup_chain_env();

        write_session(
            &resolver,
            "session-a",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#,
            ],
        );

        let mut index = ChainIndex::new();
        index.refresh(&resolver, "/test/project").unwrap();
        assert_eq!(index.chain_heads().len(), 1);
        assert!(index.chain_heads().contains(&"session-a".to_string()));

        // Add a successor file after initial build
        write_session(
            &resolver,
            "session-b",
            &[
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            ],
        );

        index.refresh(&resolver, "/test/project").unwrap();
        assert_eq!(index.successor_of("session-a"), Some("session-b"));

        let heads = index.chain_heads();
        assert_eq!(heads.len(), 1);
        assert!(heads.contains(&"session-a".to_string()));
    }

    #[test]
    fn test_chain_index_chain_heads() {
        let (_temp, resolver) = setup_chain_env();

        // Two independent conversations
        write_session(
            &resolver,
            "session-x",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-x","message":{"role":"user","content":"Hello"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-y",
            &[
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-y","message":{"role":"user","content":"World"}}"#,
            ],
        );

        // Chain: session-a → session-b
        write_session(
            &resolver,
            "session-a",
            &[
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Start"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-b",
            &[
                r#"{"type":"user","uuid":"u4","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            ],
        );

        let mut index = ChainIndex::new();
        index.refresh(&resolver, "/test/project").unwrap();

        let mut heads = index.chain_heads();
        heads.sort();
        // session-b is a successor, so heads are: session-a, session-x, session-y
        assert_eq!(heads.len(), 3);
        assert!(heads.contains(&"session-a".to_string()));
        assert!(heads.contains(&"session-x".to_string()));
        assert!(heads.contains(&"session-y".to_string()));
        assert!(!heads.contains(&"session-b".to_string()));
    }

    #[test]
    fn test_chain_index_resolve_chain_three_segments() {
        let (_temp, resolver) = setup_chain_env();

        write_session(
            &resolver,
            "session-a",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Start"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-b",
            &[
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge AB"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-c",
            &[
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T02:00:00Z","sessionId":"session-b","message":{"role":"user","content":"Bridge BC"}}"#,
            ],
        );

        let mut index = ChainIndex::new();
        index.refresh(&resolver, "/test/project").unwrap();

        assert_eq!(
            index.resolve_chain("session-a"),
            vec!["session-a", "session-b", "session-c"]
        );
        assert_eq!(
            index.resolve_chain("session-b"),
            vec!["session-a", "session-b", "session-c"]
        );
        assert_eq!(
            index.resolve_chain("session-c"),
            vec!["session-a", "session-b", "session-c"]
        );
    }

    // ── resolve_chain_with_map tests ─────────────────────────────────

    #[test]
    fn test_resolve_chain_with_map_cycle() {
        // Corrupt data: A→B and B→A would loop forever without cycle detection
        let mut succession = HashMap::new();
        succession.insert("session-a".to_string(), "session-b".to_string());
        succession.insert("session-b".to_string(), "session-a".to_string());

        let chain = resolve_chain_with_map(&succession, "session-a");
        // Should terminate and contain at most the two nodes (no infinite loop)
        assert!(chain.len() <= 2);
        assert!(chain.contains(&"session-a".to_string()));
    }

    #[test]
    fn test_resolve_chain_with_map_self_loop() {
        // Degenerate: A→A
        let mut succession = HashMap::new();
        succession.insert("session-a".to_string(), "session-a".to_string());

        let chain = resolve_chain_with_map(&succession, "session-a");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0], "session-a");
    }

    // ── is_bridge_entry tests ────────────────────────────────────────

    #[test]
    fn test_is_bridge_entry() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hi"}}"#,
        )
        .unwrap();

        // Entry with session_id "session-a" in a file owned by "session-b" is a bridge
        assert!(is_bridge_entry(&entry, "session-b"));

        // Entry with matching session_id is not a bridge
        assert!(!is_bridge_entry(&entry, "session-a"));
    }

    #[test]
    fn test_is_bridge_entry_no_session_id() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hi"}}"#,
        )
        .unwrap();

        // Entry without session_id is never a bridge
        assert!(!is_bridge_entry(&entry, "session-b"));
    }
}
