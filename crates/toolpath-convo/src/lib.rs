#![doc = include_str!("../README.md")]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ── Error ────────────────────────────────────────────────────────────

/// Errors from conversation provider operations.
#[derive(Debug, thiserror::Error)]
pub enum ConvoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, ConvoError>;

// ── Core types ───────────────────────────────────────────────────────

/// Who produced a turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
    /// Provider-specific roles (e.g. "tool", "function").
    Other(String),
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
            Role::System => write!(f, "system"),
            Role::Other(s) => write!(f, "{}", s),
        }
    }
}

/// Token usage for a single turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// A tool invocation within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    /// Populated when the result is available in the same turn.
    pub result: Option<ToolResult>,
}

/// The result of a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

/// A single turn in a conversation, from any provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// Unique identifier within the conversation.
    pub id: String,

    /// Parent turn ID (for branching conversations).
    pub parent_id: Option<String>,

    /// Who produced this turn.
    pub role: Role,

    /// When this turn occurred (ISO 8601).
    pub timestamp: String,

    /// The visible text content (already collapsed from provider-specific formats).
    pub text: String,

    /// Internal reasoning (chain-of-thought, thinking blocks).
    pub thinking: Option<String>,

    /// Tool invocations in this turn.
    pub tool_uses: Vec<ToolInvocation>,

    /// Model identifier (e.g. "claude-opus-4-6", "gpt-4o").
    pub model: Option<String>,

    /// Why the turn ended (e.g. "end_turn", "tool_use", "max_tokens").
    pub stop_reason: Option<String>,

    /// Token usage for this turn.
    pub token_usage: Option<TokenUsage>,

    /// Provider-specific data that doesn't fit the common schema.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A complete conversation from any provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationView {
    /// Unique session/conversation identifier.
    pub id: String,

    /// When the conversation started.
    pub started_at: Option<DateTime<Utc>>,

    /// When the conversation was last active.
    pub last_activity: Option<DateTime<Utc>>,

    /// Ordered turns.
    pub turns: Vec<Turn>,
}

impl ConversationView {
    /// Title derived from the first user turn, truncated to `max_len` characters.
    pub fn title(&self, max_len: usize) -> Option<String> {
        let text = self
            .turns
            .iter()
            .find(|t| t.role == Role::User && !t.text.is_empty())
            .map(|t| &t.text)?;

        if text.chars().count() > max_len {
            let truncated: String = text.chars().take(max_len).collect();
            Some(format!("{}...", truncated))
        } else {
            Some(text.clone())
        }
    }

    /// All turns with the given role.
    pub fn turns_by_role(&self, role: &Role) -> Vec<&Turn> {
        self.turns.iter().filter(|t| &t.role == role).collect()
    }

    /// Turns added after the turn with the given ID.
    ///
    /// If the ID is not found, returns all turns. If the ID is the last
    /// turn, returns an empty slice.
    pub fn turns_since(&self, turn_id: &str) -> &[Turn] {
        match self.turns.iter().position(|t| t.id == turn_id) {
            Some(idx) if idx + 1 < self.turns.len() => &self.turns[idx + 1..],
            Some(_) => &[],
            None => &self.turns,
        }
    }
}

/// Lightweight metadata for a conversation (no turns loaded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub file_path: Option<PathBuf>,
}

// ── Events ───────────────────────────────────────────────────────────

/// Events emitted by a [`ConversationWatcher`].
#[derive(Debug, Clone)]
pub enum WatcherEvent {
    /// A turn seen for the first time.
    Turn(Box<Turn>),

    /// A previously-emitted turn with additional data filled in
    /// (e.g. tool results that arrived in a later log entry).
    ///
    /// Consumers should replace their stored copy of the turn with this
    /// updated version. The turn's `id` field identifies which turn to replace.
    TurnUpdated(Box<Turn>),

    /// A non-conversational progress/status event.
    Progress {
        kind: String,
        data: serde_json::Value,
    },
}

// ── Traits ───────────────────────────────────────────────────────────

/// Trait for converting provider-specific conversation data into the
/// generic [`ConversationView`].
///
/// Implement this on your provider's manager type (e.g. `ClaudeConvo`).
pub trait ConversationProvider {
    /// List conversation IDs for a project/workspace.
    fn list_conversations(&self, project: &str) -> Result<Vec<String>>;

    /// Load a full conversation as a [`ConversationView`].
    fn load_conversation(&self, project: &str, conversation_id: &str) -> Result<ConversationView>;

    /// Load metadata only (no turns).
    fn load_metadata(&self, project: &str, conversation_id: &str) -> Result<ConversationMeta>;

    /// List metadata for all conversations in a project.
    fn list_metadata(&self, project: &str) -> Result<Vec<ConversationMeta>>;
}

/// Trait for polling conversation updates from any provider.
pub trait ConversationWatcher {
    /// Poll for new events since the last poll.
    fn poll(&mut self) -> Result<Vec<WatcherEvent>>;

    /// Number of turns seen so far.
    fn seen_count(&self) -> usize;
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view() -> ConversationView {
        ConversationView {
            id: "sess-1".into(),
            started_at: None,
            last_activity: None,
            turns: vec![
                Turn {
                    id: "t1".into(),
                    parent_id: None,
                    role: Role::User,
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    text: "Fix the authentication bug in login.rs".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    extra: HashMap::new(),
                },
                Turn {
                    id: "t2".into(),
                    parent_id: Some("t1".into()),
                    role: Role::Assistant,
                    timestamp: "2026-01-01T00:00:01Z".into(),
                    text: "I'll fix that for you.".into(),
                    thinking: Some("The bug is in the token validation".into()),
                    tool_uses: vec![ToolInvocation {
                        id: "tool-1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"file": "src/login.rs"}),
                        result: Some(ToolResult {
                            content: "fn login() { ... }".into(),
                            is_error: false,
                        }),
                    }],
                    model: Some("claude-opus-4-6".into()),
                    stop_reason: Some("end_turn".into()),
                    token_usage: Some(TokenUsage {
                        input_tokens: Some(100),
                        output_tokens: Some(50),
                    }),
                    extra: HashMap::new(),
                },
                Turn {
                    id: "t3".into(),
                    parent_id: Some("t2".into()),
                    role: Role::User,
                    timestamp: "2026-01-01T00:00:02Z".into(),
                    text: "Thanks!".into(),
                    thinking: None,
                    tool_uses: vec![],
                    model: None,
                    stop_reason: None,
                    token_usage: None,
                    extra: HashMap::new(),
                },
            ],
        }
    }

    #[test]
    fn test_title_short() {
        let view = sample_view();
        let title = view.title(100).unwrap();
        assert_eq!(title, "Fix the authentication bug in login.rs");
    }

    #[test]
    fn test_title_truncated() {
        let view = sample_view();
        let title = view.title(10).unwrap();
        assert_eq!(title, "Fix the au...");
    }

    #[test]
    fn test_title_empty() {
        let view = ConversationView {
            id: "empty".into(),
            started_at: None,
            last_activity: None,
            turns: vec![],
        };
        assert!(view.title(50).is_none());
    }

    #[test]
    fn test_turns_by_role() {
        let view = sample_view();
        let users = view.turns_by_role(&Role::User);
        assert_eq!(users.len(), 2);
        let assistants = view.turns_by_role(&Role::Assistant);
        assert_eq!(assistants.len(), 1);
    }

    #[test]
    fn test_turns_since_middle() {
        let view = sample_view();
        let since = view.turns_since("t1");
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].id, "t2");
    }

    #[test]
    fn test_turns_since_last() {
        let view = sample_view();
        let since = view.turns_since("t3");
        assert!(since.is_empty());
    }

    #[test]
    fn test_turns_since_unknown() {
        let view = sample_view();
        let since = view.turns_since("nonexistent");
        assert_eq!(since.len(), 3);
    }

    #[test]
    fn test_role_display() {
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::Other("tool".into()).to_string(), "tool");
    }

    #[test]
    fn test_role_equality() {
        assert_eq!(Role::User, Role::User);
        assert_ne!(Role::User, Role::Assistant);
        assert_eq!(Role::Other("x".into()), Role::Other("x".into()));
        assert_ne!(Role::Other("x".into()), Role::Other("y".into()));
    }

    #[test]
    fn test_turn_serde_roundtrip() {
        let turn = &sample_view().turns[1];
        let json = serde_json::to_string(turn).unwrap();
        let back: Turn = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "t2");
        assert_eq!(back.model, Some("claude-opus-4-6".into()));
        assert_eq!(back.tool_uses.len(), 1);
        assert_eq!(back.tool_uses[0].name, "Read");
        assert!(back.tool_uses[0].result.is_some());
    }

    #[test]
    fn test_conversation_view_serde_roundtrip() {
        let view = sample_view();
        let json = serde_json::to_string(&view).unwrap();
        let back: ConversationView = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "sess-1");
        assert_eq!(back.turns.len(), 3);
    }

    #[test]
    fn test_watcher_event_variants() {
        let turn_event = WatcherEvent::Turn(Box::new(sample_view().turns[0].clone()));
        assert!(matches!(turn_event, WatcherEvent::Turn(_)));

        let updated_event = WatcherEvent::TurnUpdated(Box::new(sample_view().turns[1].clone()));
        assert!(matches!(updated_event, WatcherEvent::TurnUpdated(_)));

        let progress_event = WatcherEvent::Progress {
            kind: "agent_progress".into(),
            data: serde_json::json!({"status": "running"}),
        };
        assert!(matches!(progress_event, WatcherEvent::Progress { .. }));
    }

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert!(usage.input_tokens.is_none());
        assert!(usage.output_tokens.is_none());
    }

    #[test]
    fn test_conversation_meta() {
        let meta = ConversationMeta {
            id: "sess-1".into(),
            started_at: None,
            last_activity: None,
            message_count: 5,
            file_path: Some("/tmp/test.jsonl".into()),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: ConversationMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message_count, 5);
    }
}
