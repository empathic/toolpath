use crate::types::{ContentPart, Conversation, ConversationEntry, HistoryEntry, MessageRole};
use chrono::{DateTime, Utc};

pub struct ConversationQuery<'a> {
    conversation: &'a Conversation,
}

impl<'a> ConversationQuery<'a> {
    pub fn new(conversation: &'a Conversation) -> Self {
        Self { conversation }
    }

    pub fn by_role(&self, role: MessageRole) -> Vec<&'a ConversationEntry> {
        self.conversation
            .entries
            .iter()
            .filter(|e| e.message.as_ref().map(|m| m.role == role).unwrap_or(false))
            .collect()
    }

    pub fn by_type(&self, entry_type: &str) -> Vec<&'a ConversationEntry> {
        self.conversation
            .entries
            .iter()
            .filter(|e| e.entry_type == entry_type)
            .collect()
    }

    pub fn by_time_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Vec<&'a ConversationEntry> {
        self.conversation
            .entries
            .iter()
            .filter(|e| {
                if let Ok(timestamp) = e.timestamp.parse::<DateTime<Utc>>() {
                    timestamp >= start && timestamp <= end
                } else {
                    false
                }
            })
            .collect()
    }

    pub fn tool_uses_by_name(&self, tool_name: &str) -> Vec<&'a ConversationEntry> {
        self.conversation
            .entries
            .iter()
            .filter(|e| {
                if let Some(message) = &e.message
                    && let Some(crate::types::MessageContent::Parts(parts)) = &message.content
                {
                    return parts.iter().any(|p| {
                        if let ContentPart::ToolUse { name, .. } = p {
                            name == tool_name
                        } else {
                            false
                        }
                    });
                }
                false
            })
            .collect()
    }

    pub fn contains_text(&self, search: &str) -> Vec<&'a ConversationEntry> {
        let search_lower = search.to_lowercase();
        self.conversation
            .entries
            .iter()
            .filter(|e| {
                if let Some(message) = &e.message {
                    match &message.content {
                        Some(crate::types::MessageContent::Text(text)) => {
                            text.to_lowercase().contains(&search_lower)
                        }
                        Some(crate::types::MessageContent::Parts(parts)) => {
                            parts.iter().any(|p| match p {
                                ContentPart::Text { text } => {
                                    text.to_lowercase().contains(&search_lower)
                                }
                                ContentPart::ToolResult { content, .. } => {
                                    content.text().to_lowercase().contains(&search_lower)
                                }
                                _ => false,
                            })
                        }
                        None => false,
                    }
                } else {
                    false
                }
            })
            .collect()
    }

    pub fn errors(&self) -> Vec<&'a ConversationEntry> {
        self.conversation
            .entries
            .iter()
            .filter(|e| {
                if let Some(message) = &e.message
                    && let Some(crate::types::MessageContent::Parts(parts)) = &message.content
                {
                    return parts.iter().any(|p| {
                        if let ContentPart::ToolResult { is_error, .. } = p {
                            *is_error
                        } else {
                            false
                        }
                    });
                }
                false
            })
            .collect()
    }
}

pub struct HistoryQuery<'a> {
    history: &'a [HistoryEntry],
}

impl<'a> HistoryQuery<'a> {
    pub fn new(history: &'a [HistoryEntry]) -> Self {
        Self { history }
    }

    pub fn by_project(&self, project: &str) -> Vec<&'a HistoryEntry> {
        self.history
            .iter()
            .filter(|e| e.project.as_deref() == Some(project))
            .collect()
    }

    pub fn by_session(&self, session_id: &str) -> Vec<&'a HistoryEntry> {
        self.history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(session_id))
            .collect()
    }

    pub fn by_time_range(&self, start: i64, end: i64) -> Vec<&'a HistoryEntry> {
        self.history
            .iter()
            .filter(|e| e.timestamp >= start && e.timestamp <= end)
            .collect()
    }

    pub fn contains_text(&self, search: &str) -> Vec<&'a HistoryEntry> {
        let search_lower = search.to_lowercase();
        self.history
            .iter()
            .filter(|e| e.display.to_lowercase().contains(&search_lower))
            .collect()
    }

    pub fn recent(&self, count: usize) -> Vec<&'a HistoryEntry> {
        let mut sorted: Vec<&'a HistoryEntry> = self.history.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        sorted.into_iter().take(count).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Conversation, ConversationEntry, Message, MessageContent};

    fn create_test_conversation() -> Conversation {
        let mut conv = Conversation::new("test".to_string());

        let user_entry = ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "user".to_string(),
            uuid: "1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test".to_string()),
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text("Hello world".to_string())),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        };

        let assistant_entry = ConversationEntry {
            parent_uuid: Some("1".to_string()),
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "2".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            session_id: Some("test".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text("Hi there".to_string())),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        };

        conv.add_entry(user_entry);
        conv.add_entry(assistant_entry);
        conv
    }

    #[test]
    fn test_query_by_role() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let user_msgs = query.by_role(MessageRole::User);
        assert_eq!(user_msgs.len(), 1);

        let assistant_msgs = query.by_role(MessageRole::Assistant);
        assert_eq!(assistant_msgs.len(), 1);
    }

    #[test]
    fn test_query_contains_text() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let results = query.contains_text("Hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uuid, "1");

        let results = query.contains_text("Hi");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uuid, "2");
    }

    #[test]
    fn test_query_by_type() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let users = query.by_type("user");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].uuid, "1");

        let assistants = query.by_type("assistant");
        assert_eq!(assistants.len(), 1);
        assert_eq!(assistants[0].uuid, "2");
    }

    #[test]
    fn test_query_by_time_range() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let start = "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let results = query.by_time_range(start, end);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uuid, "1");
    }

    #[test]
    fn test_query_by_time_range_all() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let start = "2023-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let results = query.by_time_range(start, end);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_query_tool_uses_by_name() {
        // Create a conversation with tool use
        let mut conv = Conversation::new("test".to_string());
        let entry = ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "3".to_string(),
            timestamp: "2024-01-01T00:00:02Z".to_string(),
            session_id: Some("test".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::Value::Object(Default::default()),
                }])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        };
        conv.add_entry(entry);

        let query = ConversationQuery::new(&conv);
        let reads = query.tool_uses_by_name("Read");
        assert_eq!(reads.len(), 1);

        let writes = query.tool_uses_by_name("Write");
        assert!(writes.is_empty());
    }

    #[test]
    fn test_query_errors() {
        let mut conv = Conversation::new("test".to_string());
        let entry = ConversationEntry {
            parent_uuid: None,
            is_sidechain: false,
            entry_type: "assistant".to_string(),
            uuid: "e1".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: Some("test".to_string()),
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Parts(vec![ContentPart::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: crate::types::ToolResultContent::Text("failed!".to_string()),
                    is_error: true,
                }])),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            cwd: None,
            git_branch: None,
            version: None,
            user_type: None,
            request_id: None,
            tool_use_result: None,
            snapshot: None,
            message_id: None,
            extra: Default::default(),
        };
        conv.add_entry(entry);

        let query = ConversationQuery::new(&conv);
        let errors = query.errors();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_query_errors_empty() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);
        assert!(query.errors().is_empty());
    }

    #[test]
    fn test_query_contains_text_case_insensitive() {
        let conv = create_test_conversation();
        let query = ConversationQuery::new(&conv);

        let results = query.contains_text("hello");
        assert_eq!(results.len(), 1);
    }

    // ── HistoryQuery ───────────────────────────────────────────────────

    fn create_test_history() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                display: "fix bug in auth".to_string(),
                pasted_contents: Default::default(),
                timestamp: 1000,
                project: Some("/project/a".to_string()),
                session_id: Some("session-1".to_string()),
            },
            HistoryEntry {
                display: "add feature X".to_string(),
                pasted_contents: Default::default(),
                timestamp: 2000,
                project: Some("/project/b".to_string()),
                session_id: Some("session-2".to_string()),
            },
            HistoryEntry {
                display: "refactor auth module".to_string(),
                pasted_contents: Default::default(),
                timestamp: 3000,
                project: Some("/project/a".to_string()),
                session_id: Some("session-1".to_string()),
            },
        ]
    }

    #[test]
    fn test_history_by_project() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.by_project("/project/a");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_history_by_session() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.by_session("session-2");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "add feature X");
    }

    #[test]
    fn test_history_by_time_range() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.by_time_range(1500, 2500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].timestamp, 2000);
    }

    #[test]
    fn test_history_contains_text() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.contains_text("auth");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_history_contains_text_case_insensitive() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.contains_text("AUTH");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_history_recent() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.recent(2);
        assert_eq!(results.len(), 2);
        // Most recent first
        assert_eq!(results[0].timestamp, 3000);
        assert_eq!(results[1].timestamp, 2000);
    }

    #[test]
    fn test_history_recent_more_than_available() {
        let history = create_test_history();
        let query = HistoryQuery::new(&history);

        let results = query.recent(10);
        assert_eq!(results.len(), 3);
    }
}
