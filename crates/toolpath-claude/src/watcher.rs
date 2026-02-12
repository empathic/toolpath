//! Conversation watching/tailing functionality.
//!
//! Provides a way to watch a conversation for new entries without re-processing
//! entries that have already been seen.

use crate::ClaudeConvo;
use crate::error::Result;
use crate::types::{Conversation, ConversationEntry, MessageRole};
use std::collections::HashSet;

/// Watches a conversation for new entries.
///
/// Tracks which entries have been seen (by UUID) and only returns new entries
/// on subsequent polls.
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
    pub fn poll(&mut self) -> Result<Vec<ConversationEntry>> {
        let convo = self
            .manager
            .read_conversation(&self.project, &self.session_id)?;
        self.extract_new_entries(&convo)
    }

    /// Polls and returns the full conversation along with just the new entries.
    ///
    /// Useful when you need both the full state and the delta.
    pub fn poll_with_full(&mut self) -> Result<(Conversation, Vec<ConversationEntry>)> {
        let convo = self
            .manager
            .read_conversation(&self.project, &self.session_id)?;
        let new_entries = self.extract_new_entries(&convo)?;
        Ok((convo, new_entries))
    }

    /// Resets the watcher, clearing all seen UUIDs.
    ///
    /// The next poll will return all entries as if it were the first call.
    pub fn reset(&mut self) {
        self.seen_uuids.clear();
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
        let convo = self
            .manager
            .read_conversation(&self.project, &self.session_id)?;
        let count = convo.entries.len();
        for entry in &convo.entries {
            self.seen_uuids.insert(entry.uuid.clone());
        }
        Ok(count)
    }

    fn extract_new_entries(&mut self, convo: &Conversation) -> Result<Vec<ConversationEntry>> {
        let mut new_entries = Vec::new();

        for entry in &convo.entries {
            if self.seen_uuids.contains(&entry.uuid) {
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
