//! Derive Toolpath documents from Gemini CLI conversation logs.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]. All
//! Gemini-specific work (sub-agent linearization, file-diff synthesis,
//! producer/base population) happens in
//! [`crate::provider::to_view`]; nothing provider-specific lives in
//! this module.

use crate::provider::to_view;
use crate::types::Conversation;
use toolpath::v1::Path;

/// Configuration for deriving Toolpath documents from Gemini conversations.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override the project path used for `path.base.uri`.
    pub project_path: Option<String>,
    /// Include thinking blocks in the `conversation.append` text payload.
    pub include_thinking: bool,
}

/// Derive a single Toolpath [`Path`] from a Gemini conversation.
pub fn derive_path(
    conversation: &Conversation,
    config: &DeriveConfig,
) -> Path {
    let view = to_view(conversation);
    let prefix: String = view.id.chars().take(8).collect();
    let base_uri = config.project_path.as_ref().map(|p| {
        if p.starts_with('/') {
            format!("file://{}", p)
        } else {
            p.clone()
        }
    });
    let cfg = toolpath_convo::DeriveConfig {
        base_uri,
        title: Some(format!("Gemini session: {}", prefix)),
        include_thinking: config.include_thinking,
        ..Default::default()
    };
    toolpath_convo::derive_path(&view, &cfg)
}

/// Derive Toolpath Paths from multiple conversations.
pub fn derive_project(
    conversations: &[Conversation],
    config: &DeriveConfig,
) -> Vec<Path> {
    conversations
        .iter()
        .map(|c| derive_path(c, config))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatFile, Conversation};
    use toolpath::v1::Graph;

    fn make_convo() -> Conversation {
        let main_json = r#"{
            "sessionId": "sess-1",
            "messages": [
                {"id":"u1","timestamp":"2026-04-17T15:23:55Z","type":"user","content":[{"text":"make a pickle"}]},
                {"id":"g1","timestamp":"2026-04-17T15:23:57Z","type":"gemini","content":"done","model":"gemini-3-flash-preview"}
            ]
        }"#;
        let main: ChatFile = serde_json::from_str(main_json).unwrap();
        Conversation {
            session_uuid: "abcdef01-2345-6789-abcd-ef0123456789".into(),
            main,
            sub_agents: vec![],
            project_path: Some("/tmp/proj".into()),
            started_at: None,
            last_activity: None,
        }
    }

    #[test]
    fn derive_path_basic_shape() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-gemini-cli-"));
        let base = path.path.base.as_ref().expect("base");
        assert_eq!(base.uri, "file:///tmp/proj");
    }

    #[test]
    fn derive_path_producer_in_meta_extra() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let producer = path.meta.as_ref().unwrap().extra.get("producer").unwrap();
        assert_eq!(producer["name"], "gemini-cli");
    }

    #[test]
    fn derive_path_actors_populated() {
        let convo = make_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:gemini-3-flash-preview"));
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
}
