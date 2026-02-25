use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    #[serde(default)]
    pub is_sidechain: bool,

    #[serde(rename = "type")]
    pub entry_type: String,

    #[serde(default)]
    pub uuid: String,

    #[serde(default)]
    pub timestamp: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: MessageRole,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", alias = "stop_reason")]
    pub stop_reason: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", alias = "stop_sequence")]
    pub stop_sequence: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(default)]
        is_error: bool,
    },
    /// Catch-all for unknown content types
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Parts(Vec<ToolResultPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPart {
    #[serde(default)]
    pub text: Option<String>,
}

impl ToolResultContent {
    pub fn text(&self) -> String {
        match self {
            ToolResultContent::Text(s) => s.clone(),
            ToolResultContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// A reference to a tool use entry within a content part.
#[derive(Debug)]
pub struct ToolUseRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a Value,
}

/// A reference to a tool result entry within a content part.
#[derive(Debug)]
pub struct ToolResultRef<'a> {
    pub tool_use_id: &'a str,
    pub content: &'a ToolResultContent,
    pub is_error: bool,
}

impl Message {
    /// Collapsed text content, joining all text parts with newlines.
    ///
    /// Returns an empty string if content is `None` or contains no text parts.
    pub fn text(&self) -> String {
        match &self.content {
            Some(MessageContent::Text(t)) => t.clone(),
            Some(MessageContent::Parts(parts)) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        }
    }

    /// Thinking blocks, if any.
    ///
    /// Returns `None` when the message has no thinking content (not an empty vec).
    pub fn thinking(&self) -> Option<Vec<&str>> {
        let parts = match &self.content {
            Some(MessageContent::Parts(parts)) => parts,
            _ => return None,
        };
        let thinking: Vec<&str> = parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect();
        if thinking.is_empty() {
            None
        } else {
            Some(thinking)
        }
    }

    /// Tool use entries, if any.
    pub fn tool_uses(&self) -> Vec<ToolUseRef<'_>> {
        let parts = match &self.content {
            Some(MessageContent::Parts(parts)) => parts,
            _ => return Vec::new(),
        };
        parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { id, name, input } => Some(ToolUseRef { id, name, input }),
                _ => None,
            })
            .collect()
    }

    /// Tool result entries, if any.
    pub fn tool_results(&self) -> Vec<ToolResultRef<'_>> {
        let parts = match &self.content {
            Some(MessageContent::Parts(parts)) => parts,
            _ => return Vec::new(),
        };
        parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some(ToolResultRef {
                    tool_use_id,
                    content,
                    is_error: *is_error,
                }),
                _ => None,
            })
            .collect()
    }

    /// Whether this message has the given role.
    pub fn is_role(&self, role: MessageRole) -> bool {
        self.role == role
    }

    /// Whether this is a user message.
    pub fn is_user(&self) -> bool {
        self.role == MessageRole::User
    }

    /// Whether this is an assistant message.
    pub fn is_assistant(&self) -> bool {
        self.role == MessageRole::Assistant
    }
}

impl ConversationEntry {
    /// Role of the message, if present.
    pub fn role(&self) -> Option<&MessageRole> {
        self.message.as_ref().map(|m| &m.role)
    }

    /// Collapsed text content of the message.
    ///
    /// Delegates to [`Message::text`]. Returns an empty string if no message is present.
    pub fn text(&self) -> String {
        self.message.as_ref().map(|m| m.text()).unwrap_or_default()
    }

    /// Thinking blocks from the message, if any.
    pub fn thinking(&self) -> Option<Vec<&str>> {
        self.message.as_ref().and_then(|m| m.thinking())
    }

    /// Tool use entries from the message, if any.
    pub fn tool_uses(&self) -> Vec<ToolUseRef<'_>> {
        self.message
            .as_ref()
            .map(|m| m.tool_uses())
            .unwrap_or_default()
    }

    /// Stop reason, if present.
    pub fn stop_reason(&self) -> Option<&str> {
        self.message.as_ref().and_then(|m| m.stop_reason.as_deref())
    }

    /// Model name, if present.
    pub fn model(&self) -> Option<&str> {
        self.message.as_ref().and_then(|m| m.model.as_deref())
    }
}

impl ContentPart {
    /// Returns a short summary of this content part.
    pub fn summary(&self) -> String {
        match self {
            ContentPart::Text { text } => {
                if text.chars().count() > 100 {
                    let truncated: String = text.chars().take(97).collect();
                    format!("{}...", truncated)
                } else {
                    text.clone()
                }
            }
            ContentPart::Thinking { .. } => "[thinking]".to_string(),
            ContentPart::ToolUse { name, .. } => format!("[tool_use: {}]", name),
            ContentPart::ToolResult {
                is_error, content, ..
            } => {
                let text = content.text();
                let prefix = if *is_error { "error" } else { "result" };
                if text.chars().count() > 80 {
                    let truncated: String = text.chars().take(77).collect();
                    format!("[{}: {}...]", prefix, truncated)
                } else {
                    format!("[{}: {}]", prefix, text)
                }
            }
            ContentPart::Unknown => "[unknown]".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl std::str::FromStr for MessageRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "user" => Ok(MessageRole::User),
            "assistant" => Ok(MessageRole::Assistant),
            "system" => Ok(MessageRole::System),
            _ => Err(format!("Invalid message role: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(alias = "input_tokens")]
    pub input_tokens: Option<u32>,
    #[serde(alias = "output_tokens")]
    pub output_tokens: Option<u32>,
    #[serde(alias = "cache_creation_input_tokens")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(alias = "cache_read_input_tokens")]
    pub cache_read_input_tokens: Option<u32>,
    #[serde(alias = "cache_creation")]
    pub cache_creation: Option<CacheCreation>,
    #[serde(alias = "service_tier")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheCreation {
    #[serde(alias = "ephemeral_5m_input_tokens")]
    pub ephemeral_5m_input_tokens: Option<u32>,
    #[serde(alias = "ephemeral_1h_input_tokens")]
    pub ephemeral_1h_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub display: String,

    #[serde(rename = "pastedContents", default)]
    pub pasted_contents: HashMap<String, Value>,

    pub timestamp: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub session_id: String,
    pub project_path: Option<String>,
    pub entries: Vec<ConversationEntry>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

impl Conversation {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            project_path: None,
            entries: Vec::new(),
            started_at: None,
            last_activity: None,
        }
    }

    pub fn add_entry(&mut self, entry: ConversationEntry) {
        if let Ok(timestamp) = entry.timestamp.parse::<DateTime<Utc>>() {
            if self.started_at.is_none() || Some(timestamp) < self.started_at {
                self.started_at = Some(timestamp);
            }
            if self.last_activity.is_none() || Some(timestamp) > self.last_activity {
                self.last_activity = Some(timestamp);
            }
        }

        if self.project_path.is_none() {
            self.project_path = entry.cwd.clone();
        }

        self.entries.push(entry);
    }

    pub fn user_messages(&self) -> Vec<&ConversationEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.entry_type == "user"
                    && e.message
                        .as_ref()
                        .map(|m| m.role == MessageRole::User)
                        .unwrap_or(false)
            })
            .collect()
    }

    pub fn assistant_messages(&self) -> Vec<&ConversationEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.entry_type == "assistant"
                    && e.message
                        .as_ref()
                        .map(|m| m.role == MessageRole::Assistant)
                        .unwrap_or(false)
            })
            .collect()
    }

    pub fn tool_uses(&self) -> Vec<(&ConversationEntry, &ContentPart)> {
        let mut results = Vec::new();

        for entry in &self.entries {
            if let Some(message) = &entry.message
                && let Some(MessageContent::Parts(parts)) = &message.content
            {
                for part in parts {
                    if matches!(part, ContentPart::ToolUse { .. }) {
                        results.push((entry, part));
                    }
                }
            }
        }

        results
    }

    pub fn message_count(&self) -> usize {
        self.entries.iter().filter(|e| e.message.is_some()).count()
    }

    pub fn duration(&self) -> Option<chrono::Duration> {
        match (self.started_at, self.last_activity) {
            (Some(start), Some(end)) => Some(end - start),
            _ => None,
        }
    }

    /// Returns entries after the given UUID.
    /// If the UUID is not found, returns all entries (for full sync).
    /// If the UUID is the last entry, returns an empty vec.
    pub fn entries_since(&self, since_uuid: &str) -> Vec<ConversationEntry> {
        match self.entries.iter().position(|e| e.uuid == since_uuid) {
            Some(idx) => self.entries.iter().skip(idx + 1).cloned().collect(),
            None => self.entries.clone(),
        }
    }

    /// Returns the UUID of the last entry, if any.
    pub fn last_uuid(&self) -> Option<&str> {
        self.entries.last().map(|e| e.uuid.as_str())
    }

    /// Text of the first user message, truncated to `max_len` characters.
    pub fn title(&self, max_len: usize) -> Option<String> {
        self.first_user_text().map(|text| {
            if text.chars().count() > max_len {
                let truncated: String = text.chars().take(max_len).collect();
                format!("{}...", truncated)
            } else {
                text
            }
        })
    }

    /// Full text of the first user message, untruncated.
    pub fn first_user_text(&self) -> Option<String> {
        self.entries.iter().find_map(|e| {
            e.message.as_ref().and_then(|msg| {
                if msg.is_user() {
                    let text = msg.text();
                    if text.is_empty() { None } else { Some(text) }
                } else {
                    None
                }
            })
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetadata {
    pub session_id: String,
    pub project_path: String,
    pub file_path: std::path::PathBuf,
    pub message_count: usize,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_conversation() -> Conversation {
        let mut convo = Conversation::new("test-session".to_string());

        let entries = vec![
            r#"{"uuid":"uuid-1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello"}}"#,
            r#"{"uuid":"uuid-2","type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"role":"assistant","content":"Hi"}}"#,
            r#"{"uuid":"uuid-3","type":"user","timestamp":"2024-01-01T00:00:02Z","message":{"role":"user","content":"How are you?"}}"#,
            r#"{"uuid":"uuid-4","type":"assistant","timestamp":"2024-01-01T00:00:03Z","message":{"role":"assistant","content":"I'm good!"}}"#,
        ];

        for entry_json in entries {
            let entry: ConversationEntry = serde_json::from_str(entry_json).unwrap();
            convo.add_entry(entry);
        }

        convo
    }

    #[test]
    fn test_entries_since_middle() {
        let convo = create_test_conversation();

        // Get entries since uuid-2 (should return uuid-3, uuid-4)
        let since = convo.entries_since("uuid-2");

        assert_eq!(since.len(), 2);
        assert_eq!(since[0].uuid, "uuid-3");
        assert_eq!(since[1].uuid, "uuid-4");
    }

    #[test]
    fn test_entries_since_first() {
        let convo = create_test_conversation();

        // Get entries since uuid-1 (should return uuid-2, uuid-3, uuid-4)
        let since = convo.entries_since("uuid-1");

        assert_eq!(since.len(), 3);
        assert_eq!(since[0].uuid, "uuid-2");
    }

    #[test]
    fn test_entries_since_last() {
        let convo = create_test_conversation();

        // Get entries since last UUID (should return empty)
        let since = convo.entries_since("uuid-4");

        assert!(since.is_empty());
    }

    #[test]
    fn test_entries_since_unknown() {
        let convo = create_test_conversation();

        // Get entries since unknown UUID (should return all entries)
        let since = convo.entries_since("unknown-uuid");

        assert_eq!(since.len(), 4);
    }

    #[test]
    fn test_last_uuid() {
        let convo = create_test_conversation();

        assert_eq!(convo.last_uuid(), Some("uuid-4"));
    }

    #[test]
    fn test_last_uuid_empty() {
        let convo = Conversation::new("empty-session".to_string());

        assert_eq!(convo.last_uuid(), None);
    }

    // ── Conversation methods ───────────────────────────────────────────

    #[test]
    fn test_user_messages() {
        let convo = create_test_conversation();
        let users = convo.user_messages();
        assert_eq!(users.len(), 2);
        assert!(users.iter().all(|e| e.entry_type == "user"));
    }

    #[test]
    fn test_assistant_messages() {
        let convo = create_test_conversation();
        let assistants = convo.assistant_messages();
        assert_eq!(assistants.len(), 2);
        assert!(assistants.iter().all(|e| e.entry_type == "assistant"));
    }

    #[test]
    fn test_message_count() {
        let convo = create_test_conversation();
        assert_eq!(convo.message_count(), 4);
    }

    #[test]
    fn test_duration() {
        let convo = create_test_conversation();
        let dur = convo.duration().unwrap();
        assert_eq!(dur.num_seconds(), 3); // 00:00:00 to 00:00:03
    }

    #[test]
    fn test_duration_empty_conversation() {
        let convo = Conversation::new("empty".to_string());
        assert!(convo.duration().is_none());
    }

    #[test]
    fn test_add_entry_tracks_timestamps() {
        let mut convo = Conversation::new("test".to_string());
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-06-15T10:00:00Z","message":{"role":"user","content":"hi"}}"#
        ).unwrap();
        convo.add_entry(entry);

        assert!(convo.started_at.is_some());
        assert!(convo.last_activity.is_some());
        assert_eq!(convo.started_at, convo.last_activity);
    }

    #[test]
    fn test_add_entry_sets_project_path() {
        let mut convo = Conversation::new("test".to_string());
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-06-15T10:00:00Z","cwd":"/home/user/project","message":{"role":"user","content":"hi"}}"#
        ).unwrap();
        convo.add_entry(entry);
        assert_eq!(convo.project_path.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn test_tool_uses() {
        let mut convo = Conversation::new("test".to_string());
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/test"}}]}}"#
        ).unwrap();
        convo.add_entry(entry);

        let uses = convo.tool_uses();
        assert_eq!(uses.len(), 1);
        match uses[0].1 {
            ContentPart::ToolUse { name, .. } => assert_eq!(name, "Read"),
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_tool_uses_empty() {
        let convo = create_test_conversation();
        // The test conversation uses MessageContent::Text, no tool uses
        let uses = convo.tool_uses();
        assert!(uses.is_empty());
    }

    // ── ContentPart::summary ───────────────────────────────────────────

    #[test]
    fn test_content_part_summary_text_short() {
        let part = ContentPart::Text {
            text: "Hello world".to_string(),
        };
        assert_eq!(part.summary(), "Hello world");
    }

    #[test]
    fn test_content_part_summary_text_long() {
        let long = "A".repeat(200);
        let part = ContentPart::Text { text: long };
        let summary = part.summary();
        assert!(summary.ends_with("..."));
        assert!(summary.chars().count() <= 100);
    }

    #[test]
    fn test_content_part_summary_thinking() {
        let part = ContentPart::Thinking {
            thinking: "deep thought".to_string(),
            signature: None,
        };
        assert_eq!(part.summary(), "[thinking]");
    }

    #[test]
    fn test_content_part_summary_tool_use() {
        let part = ContentPart::ToolUse {
            id: "t1".to_string(),
            name: "Write".to_string(),
            input: serde_json::json!({}),
        };
        assert_eq!(part.summary(), "[tool_use: Write]");
    }

    #[test]
    fn test_content_part_summary_tool_result_short() {
        let part = ContentPart::ToolResult {
            tool_use_id: "t1".to_string(),
            content: ToolResultContent::Text("OK".to_string()),
            is_error: false,
        };
        assert_eq!(part.summary(), "[result: OK]");
    }

    #[test]
    fn test_content_part_summary_tool_result_error() {
        let part = ContentPart::ToolResult {
            tool_use_id: "t1".to_string(),
            content: ToolResultContent::Text("fail".to_string()),
            is_error: true,
        };
        assert_eq!(part.summary(), "[error: fail]");
    }

    #[test]
    fn test_content_part_summary_tool_result_long() {
        let long = "X".repeat(200);
        let part = ContentPart::ToolResult {
            tool_use_id: "t1".to_string(),
            content: ToolResultContent::Text(long),
            is_error: false,
        };
        let summary = part.summary();
        assert!(summary.starts_with("[result:"));
        assert!(summary.ends_with("...]"));
    }

    #[test]
    fn test_content_part_summary_unknown() {
        let part = ContentPart::Unknown;
        assert_eq!(part.summary(), "[unknown]");
    }

    // ── ToolResultContent::text ────────────────────────────────────────

    #[test]
    fn test_tool_result_content_text_string() {
        let c = ToolResultContent::Text("hello".to_string());
        assert_eq!(c.text(), "hello");
    }

    #[test]
    fn test_tool_result_content_text_parts() {
        let c = ToolResultContent::Parts(vec![
            ToolResultPart {
                text: Some("line1".to_string()),
            },
            ToolResultPart { text: None },
            ToolResultPart {
                text: Some("line2".to_string()),
            },
        ]);
        assert_eq!(c.text(), "line1\nline2");
    }

    // ── MessageRole::from_str ──────────────────────────────────────────

    #[test]
    fn test_message_role_from_str() {
        assert_eq!("user".parse::<MessageRole>().unwrap(), MessageRole::User);
        assert_eq!(
            "assistant".parse::<MessageRole>().unwrap(),
            MessageRole::Assistant
        );
        assert_eq!(
            "system".parse::<MessageRole>().unwrap(),
            MessageRole::System
        );
    }

    #[test]
    fn test_message_role_from_str_case_insensitive() {
        assert_eq!("USER".parse::<MessageRole>().unwrap(), MessageRole::User);
        assert_eq!(
            "Assistant".parse::<MessageRole>().unwrap(),
            MessageRole::Assistant
        );
    }

    #[test]
    fn test_message_role_from_str_invalid() {
        assert!("invalid".parse::<MessageRole>().is_err());
    }

    // ── Message convenience methods ──────────────────────────────────

    #[test]
    fn test_message_text_from_string() {
        let msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text("Hello world".to_string())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert_eq!(msg.text(), "Hello world");
    }

    #[test]
    fn test_message_text_from_parts() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "First".to_string(),
                },
                ContentPart::Thinking {
                    thinking: "hmm".to_string(),
                    signature: None,
                },
                ContentPart::Text {
                    text: "Second".to_string(),
                },
            ])),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert_eq!(msg.text(), "First\nSecond");
    }

    #[test]
    fn test_message_text_none() {
        let msg = Message {
            role: MessageRole::User,
            content: None,
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert_eq!(msg.text(), "");
    }

    #[test]
    fn test_message_thinking() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Parts(vec![
                ContentPart::Thinking {
                    thinking: "deep thought".to_string(),
                    signature: None,
                },
                ContentPart::Text {
                    text: "answer".to_string(),
                },
                ContentPart::Thinking {
                    thinking: "more thought".to_string(),
                    signature: None,
                },
            ])),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        let thinking = msg.thinking().unwrap();
        assert_eq!(thinking, vec!["deep thought", "more thought"]);
    }

    #[test]
    fn test_message_thinking_none() {
        let msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text("hi".to_string())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert!(msg.thinking().is_none());
    }

    #[test]
    fn test_message_tool_uses() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Parts(vec![
                ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file": "test.rs"}),
                },
                ContentPart::Text {
                    text: "checking".to_string(),
                },
                ContentPart::ToolUse {
                    id: "t2".to_string(),
                    name: "Write".to_string(),
                    input: serde_json::json!({}),
                },
            ])),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        let uses = msg.tool_uses();
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].name, "Read");
        assert_eq!(uses[1].name, "Write");
    }

    #[test]
    fn test_message_tool_results() {
        let msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts(vec![
                ContentPart::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: ToolResultContent::Text("file contents".to_string()),
                    is_error: false,
                },
                ContentPart::ToolResult {
                    tool_use_id: "t2".to_string(),
                    content: ToolResultContent::Text("error msg".to_string()),
                    is_error: true,
                },
            ])),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        let results = msg.tool_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_use_id, "t1");
        assert_eq!(results[0].content.text(), "file contents");
        assert!(!results[0].is_error);
        assert_eq!(results[1].tool_use_id, "t2");
        assert!(results[1].is_error);
    }

    #[test]
    fn test_message_tool_results_empty() {
        let msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text("hello".to_string())),
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert!(msg.tool_results().is_empty());
    }

    #[test]
    fn test_message_role_checks() {
        let user_msg = Message {
            role: MessageRole::User,
            content: None,
            model: None,
            id: None,
            message_type: None,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        };
        assert!(user_msg.is_user());
        assert!(!user_msg.is_assistant());
        assert!(user_msg.is_role(MessageRole::User));
    }

    // ── ConversationEntry convenience methods ────────────────────────

    #[test]
    fn test_entry_text() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"Hello there"}}"#,
        )
        .unwrap();
        assert_eq!(entry.text(), "Hello there");
    }

    #[test]
    fn test_entry_text_no_message() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(entry.text(), "");
    }

    #[test]
    fn test_entry_role() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hi"}}"#,
        )
        .unwrap();
        assert_eq!(entry.role(), Some(&MessageRole::User));
    }

    #[test]
    fn test_entry_stop_reason() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"done","stopReason":"end_turn"}}"#,
        )
        .unwrap();
        assert_eq!(entry.stop_reason(), Some("end_turn"));
    }

    // ── Snake_case deserialization (real JSONL format) ─────────────

    #[test]
    fn test_stop_reason_snake_case() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"done","stop_reason":"end_turn","stop_sequence":null}}"#,
        )
        .unwrap();
        assert_eq!(entry.stop_reason(), Some("end_turn"));
        assert!(entry.message.as_ref().unwrap().stop_sequence.is_none());
    }

    #[test]
    fn test_usage_snake_case() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"hi","usage":{"input_tokens":1200,"output_tokens":350,"cache_creation_input_tokens":100,"cache_read_input_tokens":500,"service_tier":"standard"}}}"#,
        )
        .unwrap();
        let usage = entry.message.unwrap().usage.unwrap();
        assert_eq!(usage.input_tokens, Some(1200));
        assert_eq!(usage.output_tokens, Some(350));
        assert_eq!(usage.cache_creation_input_tokens, Some(100));
        assert_eq!(usage.cache_read_input_tokens, Some(500));
        assert_eq!(usage.service_tier.as_deref(), Some("standard"));
    }

    #[test]
    fn test_cache_creation_snake_case() {
        let json = r#"{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20}"#;
        let cc: CacheCreation = serde_json::from_str(json).unwrap();
        assert_eq!(cc.ephemeral_5m_input_tokens, Some(10));
        assert_eq!(cc.ephemeral_1h_input_tokens, Some(20));
    }

    #[test]
    fn test_full_assistant_entry_snake_case() {
        // Matches the actual JSONL format written by Claude Code
        let json = r#"{"parentUuid":"abc","isSidechain":false,"userType":"external","cwd":"/project","sessionId":"sess-1","version":"2.1.37","message":{"model":"claude-opus-4-6","id":"msg_123","type":"message","role":"assistant","content":[{"type":"text","text":"Done."}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":4561,"cache_read_input_tokens":17868,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":4561},"output_tokens":4,"service_tier":"standard"}},"requestId":"req_123","type":"assistant","uuid":"u1","timestamp":"2024-01-01T00:00:00Z"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        let msg = entry.message.unwrap();
        assert_eq!(msg.stop_reason.as_deref(), Some("end_turn"));
        assert!(msg.stop_sequence.is_none());
        let usage = msg.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.cache_creation_input_tokens, Some(4561));
        assert_eq!(usage.cache_read_input_tokens, Some(17868));
        assert_eq!(usage.service_tier.as_deref(), Some("standard"));
        let cc = usage.cache_creation.unwrap();
        assert_eq!(cc.ephemeral_5m_input_tokens, Some(0));
        assert_eq!(cc.ephemeral_1h_input_tokens, Some(4561));
    }

    #[test]
    fn test_entry_model() {
        let entry: ConversationEntry = serde_json::from_str(
            r#"{"uuid":"u1","type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":"hi","model":"claude-opus-4-6"}}"#,
        )
        .unwrap();
        assert_eq!(entry.model(), Some("claude-opus-4-6"));
    }

    // ── Conversation title/first_user_text ───────────────────────────

    #[test]
    fn test_conversation_title() {
        let convo = create_test_conversation();
        let title = convo.title(4).unwrap();
        assert_eq!(title, "Hell...");
    }

    #[test]
    fn test_conversation_title_short() {
        let convo = create_test_conversation();
        let title = convo.title(100).unwrap();
        assert_eq!(title, "Hello");
    }

    #[test]
    fn test_conversation_first_user_text() {
        let convo = create_test_conversation();
        assert_eq!(convo.first_user_text(), Some("Hello".to_string()));
    }

    #[test]
    fn test_conversation_title_empty() {
        let convo = Conversation::new("empty".to_string());
        assert!(convo.title(50).is_none());
    }
}
