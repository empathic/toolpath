//! Derive Toolpath documents from Claude conversation logs.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]. All
//! Claude-specific work (cwd / git_branch / version → `view.base` and
//! `view.producer`, headerless preamble + non-message entries →
//! `view.events`, tool-result cross-entry assembly, file-write diff
//! synthesis via `git show HEAD:<path>`) happens in
//! [`crate::provider::to_view`]; nothing provider-specific lives in this
//! module.

use crate::provider::to_view;
use crate::types::Conversation;
use toolpath::v1::Path;

/// Configuration for deriving Toolpath documents from Claude conversations.
#[derive(Default)]
pub struct DeriveConfig {
    /// Override the project path used for `path.base.uri`.
    pub project_path: Option<String>,
    /// Include thinking blocks in the conversation artifact.
    pub include_thinking: bool,
}

/// Derive a Toolpath [`Path`] from a Claude [`Conversation`].
pub fn derive_path(conversation: &Conversation, config: &DeriveConfig) -> Path {
    let view = to_view(conversation);
    let prefix: String = conversation.session_id.chars().take(8).collect();
    let base_uri = config.project_path.as_ref().map(|p| {
        if p.starts_with('/') {
            format!("file://{}", p)
        } else {
            p.clone()
        }
    });
    let cfg = toolpath_convo::DeriveConfig {
        base_uri,
        title: Some(format!("Claude session: {}", prefix)),
        include_thinking: config.include_thinking,
        ..Default::default()
    };
    toolpath_convo::derive_path(&view, &cfg)
}

/// Derive Toolpath Paths from multiple conversations in a project.
pub fn derive_project(conversations: &[Conversation], config: &DeriveConfig) -> Vec<Path> {
    conversations
        .iter()
        .map(|c| derive_path(c, config))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Conversation, ConversationEntry, Message, MessageContent, MessageRole};
    use std::collections::HashMap;
    use toolpath::v1::Graph;

    fn user_entry(uuid: &str, parent: Option<&str>, text: &str, cwd: &str) -> ConversationEntry {
        ConversationEntry {
            entry_type: "user".into(),
            uuid: uuid.into(),
            parent_uuid: parent.map(str::to_string),
            session_id: Some("sess-1".into()),
            timestamp: "2026-01-01T00:00:00Z".into(),
            cwd: Some(cwd.into()),
            git_branch: Some("main".into()),
            version: Some("1.0.0".into()),
            user_type: None,
            request_id: None,
            message_id: None,
            snapshot: None,
            tool_use_result: None,
            is_sidechain: false,
            message: Some(Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text(text.into())),
                model: None,
                id: None,
                message_type: None,
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            }),
            extra: HashMap::new(),
        }
    }

    fn assistant_entry(uuid: &str, parent: Option<&str>, text: &str) -> ConversationEntry {
        ConversationEntry {
            entry_type: "assistant".into(),
            uuid: uuid.into(),
            parent_uuid: parent.map(str::to_string),
            session_id: Some("sess-1".into()),
            timestamp: "2026-01-01T00:00:01Z".into(),
            cwd: Some("/tmp/proj".into()),
            git_branch: Some("main".into()),
            version: Some("1.0.0".into()),
            user_type: None,
            request_id: None,
            message_id: None,
            snapshot: None,
            tool_use_result: None,
            is_sidechain: false,
            message: Some(Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text(text.into())),
                model: Some("claude-opus-4-7".into()),
                id: None,
                message_type: None,
                stop_reason: Some("end_turn".into()),
                stop_sequence: None,
                usage: None,
            }),
            extra: HashMap::new(),
        }
    }

    fn make_convo() -> Conversation {
        Conversation {
            session_id: "sess-1abc".into(),
            project_path: Some("/tmp/proj".into()),
            entries: vec![
                user_entry("u1", None, "Fix bug", "/tmp/proj"),
                assistant_entry("a1", Some("u1"), "Done"),
            ],
            preamble: vec![],
            started_at: None,
            last_activity: None,
            session_ids: vec![],
        }
    }

    #[test]
    fn derive_path_basic_shape() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-claude-code-"));
        // Base populated from first entry's cwd / git_branch.
        let base = path.path.base.as_ref().expect("base");
        assert_eq!(base.uri, "file:///tmp/proj");
        assert_eq!(base.branch.as_deref(), Some("main"));
    }

    #[test]
    fn derive_path_producer_in_meta_extra() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let producer = path.meta.as_ref().unwrap().extra.get("producer").unwrap();
        assert_eq!(producer["name"], "claude-code");
        assert_eq!(producer["version"], "1.0.0");
    }

    #[test]
    fn derive_path_actors_populated() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:claude-opus-4-7"));
    }

    #[test]
    fn derive_path_validates_as_single_path_graph() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let pp = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&pp.steps, &pp.path.head);
        assert_eq!(anc.len(), pp.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_path_preamble_steps_carry_valid_timestamps() {
        // A headless (`claude -p`) session caches its prompt as a headerless
        // `last-prompt` line with no `timestamp` on the wire. The derived
        // step must still satisfy the schema's `format: date-time` —
        // regression test for `"timestamp": ""` on claude-preamble-* steps
        // failing `path p validate`.
        let mut convo = make_convo();
        convo.preamble = vec![
            serde_json::json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "content": "queued while busy",
                "timestamp": "2026-01-01T00:00:02Z",
                "sessionId": "sess-1abc",
            }),
            serde_json::json!({
                "type": "last-prompt",
                "lastPrompt": "Fix bug",
                "leafUuid": "u1",
                "sessionId": "sess-1abc",
            }),
        ];

        let path = derive_path(&convo, &DeriveConfig::default());

        let last_prompt = path
            .steps
            .iter()
            .find(|s| s.step.id == "claude-preamble-1")
            .expect("last-prompt preamble step");
        // leafUuid resolves to the referenced user entry's timestamp.
        assert_eq!(last_prompt.step.timestamp, "2026-01-01T00:00:00Z");

        for step in &path.steps {
            assert!(
                chrono::DateTime::parse_from_rfc3339(&step.step.timestamp).is_ok(),
                "step {} timestamp {:?} is not RFC 3339",
                step.step.id,
                step.step.timestamp
            );
        }
    }
}
