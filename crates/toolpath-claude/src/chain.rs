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

/// Build a map: predecessor_session_id → successor_file_stem.
///
/// Scans each JSONL file's first few lines. If the first entry's
/// `session_id` differs from the filename stem, that file is a
/// successor of the session named in the bridge entry.
pub(crate) fn build_succession_map(
    resolver: &PathResolver,
    project_path: &str,
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let sessions = resolver.list_conversations(project_path)?;

    for file_stem in &sessions {
        let path = resolver.conversation_file(project_path, file_stem)?;
        if let Some(first_sid) = ConversationReader::read_first_session_id(&path) {
            // If the first session_id in the file differs from the filename,
            // this file is a successor — the bridge entry points back to the
            // predecessor.
            if first_sid != *file_stem {
                map.insert(first_sid, file_stem.clone());
            }
        }
    }

    Ok(map)
}

/// Resolve the full chain containing `session_id`, returned in
/// chronological order (oldest segment first).
pub(crate) fn resolve_chain(
    resolver: &PathResolver,
    project_path: &str,
    session_id: &str,
) -> Result<Vec<String>> {
    let succession = build_succession_map(resolver, project_path)?;
    Ok(resolve_chain_with_map(&succession, session_id))
}

/// Like [`resolve_chain`] but uses a pre-built succession map.
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

/// Build the reverse map (successor → predecessor) from a succession map.
pub(crate) fn build_reverse_map(
    succession: &HashMap<String, String>,
) -> HashMap<String, String> {
    succession
        .iter()
        .map(|(pred, succ)| (succ.clone(), pred.clone()))
        .collect()
}

/// Find the immediate successor of `session_id`, if any.
pub(crate) fn find_successor(
    resolver: &PathResolver,
    project_path: &str,
    session_id: &str,
) -> Result<Option<String>> {
    let succession = build_succession_map(resolver, project_path)?;
    Ok(successor_of(&succession, session_id))
}

/// Look up the immediate successor in a pre-built map.
pub(crate) fn successor_of(
    succession: &HashMap<String, String>,
    session_id: &str,
) -> Option<String> {
    succession.get(session_id).cloned()
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

    #[test]
    fn test_build_succession_map() {
        let (_temp, resolver) = setup_chain_env();

        // Session A: normal file, session_id matches filename
        write_session(
            &resolver,
            "session-a",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#,
            ],
        );

        // Session B: bridge entry with session_id pointing to session-a
        write_session(
            &resolver,
            "session-b",
            &[
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Continued"}}"#,
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"New content"}}"#,
            ],
        );

        let map = build_succession_map(&resolver, "/test/project").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("session-a").unwrap(), "session-b");
    }

    #[test]
    fn test_resolve_chain_single() {
        let (_temp, resolver) = setup_chain_env();

        write_session(
            &resolver,
            "session-solo",
            &[
                r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-solo","message":{"role":"user","content":"Hello"}}"#,
            ],
        );

        let chain = resolve_chain(&resolver, "/test/project", "session-solo").unwrap();
        assert_eq!(chain, vec!["session-solo"]);
    }

    #[test]
    fn test_resolve_chain_two_segments() {
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
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"Continued"}}"#,
            ],
        );

        // From A
        let chain_from_a = resolve_chain(&resolver, "/test/project", "session-a").unwrap();
        assert_eq!(chain_from_a, vec!["session-a", "session-b"]);

        // From B — should resolve the same chain
        let chain_from_b = resolve_chain(&resolver, "/test/project", "session-b").unwrap();
        assert_eq!(chain_from_b, vec!["session-a", "session-b"]);
    }

    #[test]
    fn test_resolve_chain_three_segments() {
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
                r#"{"type":"user","uuid":"u3","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"Middle"}}"#,
            ],
        );

        write_session(
            &resolver,
            "session-c",
            &[
                r#"{"type":"user","uuid":"u4","timestamp":"2024-01-01T02:00:00Z","sessionId":"session-b","message":{"role":"user","content":"Bridge BC"}}"#,
                r#"{"type":"user","uuid":"u5","timestamp":"2024-01-01T02:00:01Z","sessionId":"session-c","message":{"role":"user","content":"End"}}"#,
            ],
        );

        let chain = resolve_chain(&resolver, "/test/project", "session-a").unwrap();
        assert_eq!(chain, vec!["session-a", "session-b", "session-c"]);

        // From the middle
        let chain_b = resolve_chain(&resolver, "/test/project", "session-b").unwrap();
        assert_eq!(chain_b, vec!["session-a", "session-b", "session-c"]);

        // From the tail
        let chain_c = resolve_chain(&resolver, "/test/project", "session-c").unwrap();
        assert_eq!(chain_c, vec!["session-a", "session-b", "session-c"]);
    }

    #[test]
    fn test_find_successor() {
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
                r#"{"type":"user","uuid":"u2","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            ],
        );

        assert_eq!(
            find_successor(&resolver, "/test/project", "session-a").unwrap(),
            Some("session-b".to_string())
        );
        assert_eq!(
            find_successor(&resolver, "/test/project", "session-b").unwrap(),
            None
        );
    }

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

    #[test]
    fn test_build_reverse_map() {
        let mut succession = HashMap::new();
        succession.insert("a".to_string(), "b".to_string());
        succession.insert("b".to_string(), "c".to_string());

        let reverse = build_reverse_map(&succession);
        assert_eq!(reverse.get("b").map(String::as_str), Some("a"));
        assert_eq!(reverse.get("c").map(String::as_str), Some("b"));
        assert!(reverse.get("a").is_none());
    }
}
