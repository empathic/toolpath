//! Fixtures the projection tests share.

use std::collections::HashMap;

/// A `toolpath::v1::Path` with a single `conversation.append` step on
/// `artifact_key` (for example `claude-code://my-session`). The
/// projectors read `view.id` from the first `<provider>://<id>` artifact
/// key they see, so this gives them a non-empty session id.
pub(crate) fn make_convo_path(artifact_key: &str) -> toolpath::v1::Path {
    let mut extra = HashMap::new();
    extra.insert("role".to_string(), serde_json::json!("user"));
    extra.insert("text".to_string(), serde_json::json!("hello"));
    let step = toolpath::v1::Step {
        step: toolpath::v1::StepIdentity {
            id: "s1".to_string(),
            parents: vec![],
            actor: "human:test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        },
        change: {
            let mut m = HashMap::new();
            m.insert(
                artifact_key.to_string(),
                toolpath::v1::ArtifactChange {
                    raw: None,
                    structural: Some(toolpath::v1::StructuralChange {
                        change_type: "conversation.append".to_string(),
                        extra,
                    }),
                },
            );
            m
        },
        meta: None,
    };
    toolpath::v1::Path {
        path: toolpath::v1::PathIdentity {
            id: "test-path".to_string(),
            base: None,
            head: "s1".to_string(),
            graph_ref: None,
        },
        steps: vec![step],
        meta: None,
    }
}
