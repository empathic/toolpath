//! Conversation watching/tailing functionality.
//!
//! Provides a way to watch a conversation for new entries without re-processing
//! entries that have already been seen.

use crate::ClaudeConvo;
use crate::chain;
use crate::error::Result;
use crate::types::{Conversation, ConversationEntry, MessageRole};
use std::collections::HashSet;

/// Watches a conversation for new entries.
///
/// Tracks which entries have been seen (by UUID) and only returns new entries
/// on subsequent polls.
///
/// Uses `read_segment` (single-file) internally — the watcher tails
/// individual files and follows rotations via `ChainIndex`.
///
/// # Example
///
/// ```rust,no_run
/// use toolpath_claude::{ClaudeConvo, ConversationWatcher};
///
/// let manager = ClaudeConvo::new();
/// let mut watcher = ConversationWatcher::new(
///     manager,
///     "/path/to/project".to_string(),
///     "session-uuid".to_string(),
/// );
///
/// // First poll returns all existing entries
/// let entries = watcher.poll().unwrap();
/// println!("Initial entries: {}", entries.len());
///
/// // Subsequent polls return only new entries
/// loop {
///     std::thread::sleep(std::time::Duration::from_secs(1));
///     let new_entries = watcher.poll().unwrap();
///     for entry in new_entries {
///         println!("New entry: {:?}", entry.uuid);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ConversationWatcher {
    manager: ClaudeConvo,
    project: String,
    session_id: String,
    seen_uuids: HashSet<String>,
    role_filter: Option<MessageRole>,
    /// Avoids repeated successor scans when the current session is idle.
    successor_checked: bool,
    /// Rotations detected during the last `poll()`, consumed by
    /// `take_pending_rotations`. Each entry is `(from_session, to_session)`.
    pending_rotations: Vec<(String, String)>,
    /// Cached chain index for incremental successor lookup.
    chain_index: chain::ChainIndex,
}

impl ConversationWatcher {
    /// Creates a new watcher for the given conversation.
    pub fn new(manager: ClaudeConvo, project: String, session_id: String) -> Self {
        Self {
            manager,
            project,
            session_id,
            seen_uuids: HashSet::new(),
            role_filter: None,
            successor_checked: false,
            pending_rotations: Vec::new(),
            chain_index: chain::ChainIndex::new(),
        }
    }

    /// Sets a role filter - only entries with this role will be returned.
    pub fn with_role_filter(mut self, role: MessageRole) -> Self {
        self.role_filter = Some(role);
        self
    }

    /// Returns the project path being watched.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Returns the session ID being watched.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the number of entries that have been seen.
    pub fn seen_count(&self) -> usize {
        self.seen_uuids.len()
    }

    /// Polls for new conversation entries.
    ///
    /// On the first call, returns all existing entries (optionally filtered by role).
    /// On subsequent calls, returns only entries that haven't been seen before.
    ///
    /// When the current session has been rotated to a successor file, the
    /// watcher automatically follows the chain and returns entries from the
    /// new file. Call [`take_pending_rotations`] after poll to check whether
    /// a rotation occurred.
    pub fn poll(&mut self) -> Result<Vec<ConversationEntry>> {
        let convo = self.manager.read_segment(&self.project, &self.session_id)?;
        let new_entries = self.extract_new_entries(&convo)?;

        if !new_entries.is_empty() {
            self.successor_checked = false;
            return Ok(new_entries);
        }

        // No new entries — check for a successor session
        if self.follow_rotation()? {
            return self.poll();
        }

        Ok(new_entries)
    }

    /// Polls and returns the full conversation along with just the new entries.
    ///
    /// Useful when you need both the full state and the delta.
    /// Follows session rotations the same way [`poll`] does.
    pub fn poll_with_full(&mut self) -> Result<(Conversation, Vec<ConversationEntry>)> {
        let convo = self.manager.read_segment(&self.project, &self.session_id)?;
        let new_entries = self.extract_new_entries(&convo)?;

        if !new_entries.is_empty() {
            self.successor_checked = false;
            return Ok((convo, new_entries));
        }

        // No new entries — check for rotation
        if self.follow_rotation()? {
            return self.poll_with_full();
        }

        Ok((convo, new_entries))
    }

    /// Resets the watcher, clearing all seen UUIDs and rotation state.
    ///
    /// The next poll will return all entries as if it were the first call.
    /// Does **not** revert `session_id` — if a rotation was already followed,
    /// the watcher stays on the current (latest) session.
    pub fn reset(&mut self) {
        self.seen_uuids.clear();
        self.successor_checked = false;
        self.pending_rotations.clear();
    }

    /// Pre-marks entries as seen without returning them.
    ///
    /// Useful for initializing the watcher to only return future entries.
    pub fn mark_seen(&mut self, entries: &[ConversationEntry]) {
        for entry in entries {
            self.seen_uuids.insert(entry.uuid.clone());
        }
    }

    /// Skips existing entries - next poll will only return new entries.
    pub fn skip_existing(&mut self) -> Result<usize> {
        let convo = self.manager.read_segment(&self.project, &self.session_id)?;
        let count = convo.entries.len();
        for entry in &convo.entries {
            self.seen_uuids.insert(entry.uuid.clone());
        }
        Ok(count)
    }

    /// Returns all rotations detected during the last `poll()`, consuming
    /// them. Each entry is `(from_session, to_session)`. Multi-hop chains
    /// produce multiple entries in traversal order.
    pub fn take_pending_rotations(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_rotations)
    }

    /// Check for and follow a session rotation. Returns `true` if a
    /// successor was found and the watcher switched to it.
    fn follow_rotation(&mut self) -> Result<bool> {
        if self.successor_checked {
            return Ok(false);
        }
        self.successor_checked = true;

        self.chain_index
            .refresh(self.manager.resolver(), &self.project)?;

        if let Some(successor) = self.chain_index.successor_of(&self.session_id) {
            let successor = successor.to_string();
            let old_id = self.session_id.clone();
            self.pending_rotations.push((old_id, successor.clone()));
            self.session_id = successor;
            self.successor_checked = false;
            return Ok(true);
        }

        Ok(false)
    }

    fn extract_new_entries(&mut self, convo: &Conversation) -> Result<Vec<ConversationEntry>> {
        let mut new_entries = Vec::new();

        for entry in &convo.entries {
            if self.seen_uuids.contains(&entry.uuid) {
                continue;
            }

            // Skip bridge entries — they link sessions, not content
            if chain::is_bridge_entry(entry, &self.session_id) {
                self.seen_uuids.insert(entry.uuid.clone());
                continue;
            }

            // Apply role filter if set
            if let Some(role_filter) = self.role_filter {
                if let Some(msg) = &entry.message {
                    if msg.role != role_filter {
                        self.seen_uuids.insert(entry.uuid.clone());
                        continue;
                    }
                } else {
                    self.seen_uuids.insert(entry.uuid.clone());
                    continue;
                }
            }

            new_entries.push(entry.clone());
            self.seen_uuids.insert(entry.uuid.clone());
        }

        Ok(new_entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathResolver;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_jsonl(dir: &std::path::Path, session_id: &str, entries: &[&str]) {
        let project_dir = dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let file_path = project_dir.join(format!("{}.jsonl", session_id));
        fs::write(&file_path, entries.join("\n")).unwrap();
    }

    #[test]
    fn test_watcher_tracks_seen() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi there"}}"#;

        create_test_jsonl(&claude_dir, "session-1", &[entry1, entry2]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // First poll returns all entries
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(watcher.seen_count(), 2);

        // Second poll returns nothing (no new entries)
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_watcher_skip_existing() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;

        create_test_jsonl(&claude_dir, "session-1", &[entry1]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // Skip existing
        let skipped = watcher.skip_existing().unwrap();
        assert_eq!(skipped, 1);

        // Poll returns nothing
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_watcher_accessors() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        create_test_jsonl(&claude_dir, "session-1", &[]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        assert_eq!(watcher.project(), "/test/project");
        assert_eq!(watcher.session_id(), "session-1");
        assert_eq!(watcher.seen_count(), 0);
    }

    #[test]
    fn test_watcher_reset() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;
        create_test_jsonl(&claude_dir, "session-1", &[entry1]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // First poll
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(watcher.seen_count(), 1);

        // Reset
        watcher.reset();
        assert_eq!(watcher.seen_count(), 0);

        // Poll again should return entries
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_watcher_mark_seen() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#;
        create_test_jsonl(&claude_dir, "session-1", &[entry1, entry2]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        // Read conversation to get entries
        let convo = watcher.poll().unwrap();
        watcher.reset();

        // Mark first entry as seen
        watcher.mark_seen(&convo[..1]);
        assert_eq!(watcher.seen_count(), 1);

        // Poll should return only the second entry
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "uuid-2");
    }

    #[test]
    fn test_watcher_with_role_filter() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;
        let entry2 = r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#;
        create_test_jsonl(&claude_dir, "session-1", &[entry1, entry2]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        )
        .with_role_filter(MessageRole::User);

        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "uuid-1");
        // Both entries should be marked as seen (the assistant one was filtered but still seen)
        assert_eq!(watcher.seen_count(), 2);
    }

    #[test]
    fn test_watcher_follows_rotation() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Session A: original conversation
        let entry_a = r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#;
        fs::write(
            project_dir.join("session-a.jsonl"),
            format!("{}\n", entry_a),
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-a".to_string(),
        );

        // First poll: get entry from session-a
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "a1");
        assert_eq!(watcher.session_id(), "session-a");

        // Now create session-b with a bridge entry pointing to session-a
        let entries_b = vec![
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"b1","type":"user","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"New content"}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), entries_b.join("\n")).unwrap();

        // Second poll: should auto-follow to session-b
        let entries = watcher.poll().unwrap();
        assert_eq!(watcher.session_id(), "session-b");

        // Should get only b1 — bridge entry b0 is filtered out
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "b1");
        assert_eq!(entries[0].text(), "New content");
    }

    #[test]
    fn test_watcher_follows_rotation_with_full() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entry_a = r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#;
        fs::write(
            project_dir.join("session-a.jsonl"),
            format!("{}\n", entry_a),
        )
        .unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-a".to_string(),
        );

        // Consume session-a via poll_with_full
        let (convo, new_entries) = watcher.poll_with_full().unwrap();
        assert_eq!(new_entries.len(), 1);
        assert_eq!(convo.session_id, "session-a");

        // Create successor
        let entries_b = vec![
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"b1","type":"assistant","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"assistant","content":"Continued"}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), entries_b.join("\n")).unwrap();

        // poll_with_full follows rotation too
        let (convo2, new_entries2) = watcher.poll_with_full().unwrap();
        assert_eq!(watcher.session_id(), "session-b");
        assert_eq!(convo2.session_id, "session-b");
        // Only b1 — bridge b0 filtered
        assert_eq!(new_entries2.len(), 1);
        assert_eq!(new_entries2[0].uuid, "b1");
    }

    #[test]
    fn test_watcher_reset_clears_rotation_state() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");
        let project_dir = claude_dir.join("projects/-test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let entry_a = r#"{"uuid":"a1","type":"user","timestamp":"2024-01-01T00:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Hello"}}"#;
        fs::write(
            project_dir.join("session-a.jsonl"),
            format!("{}\n", entry_a),
        )
        .unwrap();

        // Session B (successor of A)
        let entries_b = vec![
            r#"{"uuid":"b0","type":"user","timestamp":"2024-01-01T01:00:00Z","sessionId":"session-a","message":{"role":"user","content":"Bridge"}}"#,
            r#"{"uuid":"b1","type":"user","timestamp":"2024-01-01T01:00:01Z","sessionId":"session-b","message":{"role":"user","content":"New"}}"#,
        ];
        fs::write(project_dir.join("session-b.jsonl"), entries_b.join("\n")).unwrap();

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-a".to_string(),
        );

        // Consume everything (a1, then follow to b1)
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1); // a1
        let entries = watcher.poll().unwrap();
        assert_eq!(entries.len(), 1); // b1
        assert_eq!(watcher.session_id(), "session-b");

        // Reset: seen UUIDs and rotation flags cleared
        watcher.reset();
        assert_eq!(watcher.seen_count(), 0);

        // Re-poll: should replay entries from session-b (current session)
        let entries = watcher.poll().unwrap();
        // b0 is a bridge (session_id != session-b), so only b1
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "b1");
    }

    #[test]
    fn test_watcher_poll_with_full() {
        let temp = TempDir::new().unwrap();
        let claude_dir = temp.path().join(".claude");

        let entry1 = r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#;
        create_test_jsonl(&claude_dir, "session-1", &[entry1]);

        let resolver = PathResolver::new().with_claude_dir(&claude_dir);
        let manager = ClaudeConvo::with_resolver(resolver);

        let mut watcher = ConversationWatcher::new(
            manager,
            "/test/project".to_string(),
            "session-1".to_string(),
        );

        let (convo, new_entries) = watcher.poll_with_full().unwrap();
        assert_eq!(convo.entries.len(), 1);
        assert_eq!(new_entries.len(), 1);

        // Second call should return full convo but no new entries
        let (convo2, new_entries2) = watcher.poll_with_full().unwrap();
        assert_eq!(convo2.entries.len(), 1);
        assert!(new_entries2.is_empty());
    }
}
