//! Thin wrapper: PiSession → ConversationView → toolpath_convo::derive.
//!
//! Converts Pi sessions into Toolpath `Path` and `Graph` documents by
//! delegating to [`toolpath_convo::derive_path`] once the session has been
//! mapped to a provider-agnostic [`toolpath_convo::ConversationView`] via
//! [`crate::provider::session_to_view`].

use crate::PiConvo;
use crate::provider::session_to_view;
use crate::reader::PiSession;
use toolpath::v1::{Graph, GraphIdentity, GraphMeta, Path, PathOrRef};
use toolpath_convo::DeriveConfig;

/// Derive a Toolpath [`Path`] from a single Pi session.
///
/// Thin wrapper: converts the session to a provider-agnostic
/// `ConversationView` and hands off to [`toolpath_convo::derive_path`].
pub fn derive_path(session: &PiSession, config: &DeriveConfig) -> toolpath_convo::Result<Path> {
    toolpath_convo::derive_path(&session_to_view(session), config)
}

/// Derive a Toolpath [`Graph`] from multiple Pi sessions.
///
/// Each session becomes one `PathOrRef::Path` entry in the graph. `title`
/// becomes `graph.meta.title`; empty input produces a graph with no paths
/// and `graph.id == "graph-pi-empty"`.
pub fn derive_graph(
    sessions: &[PiSession],
    title: Option<&str>,
    config: &DeriveConfig,
) -> crate::Result<Graph> {
    let id_suffix = sessions
        .first()
        .map(|s| s.header.id.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "empty".to_string());
    let graph_id = format!("graph-pi-{}", id_suffix);

    let paths: Vec<PathOrRef> = sessions
        .iter()
        .map(|s| Ok(PathOrRef::Path(Box::new(derive_path(s, config)?))))
        .collect::<crate::Result<_>>()?;

    let meta = title.map(|t| GraphMeta {
        title: Some(t.to_string()),
        ..Default::default()
    });

    Ok(Graph {
        graph: GraphIdentity { id: graph_id },
        paths,
        meta,
    })
}

/// Derive a [`Graph`] from all sessions in a project.
pub fn derive_project(
    manager: &PiConvo,
    project: &str,
    title: Option<&str>,
    config: &DeriveConfig,
) -> crate::Result<Graph> {
    let sessions = manager.read_all_sessions(project)?;
    derive_graph(&sessions, title, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentMessage, Entry, EntryBase, MessageContent, SessionHeader};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_session(id: &str) -> PiSession {
        let header = SessionHeader {
            version: 3,
            id: id.into(),
            timestamp: "2026-04-16T00:00:00Z".into(),
            cwd: "/tmp/p".into(),
            parent_session: None,
            extra: HashMap::new(),
        };
        let msg = Entry::Message {
            base: EntryBase {
                id: "u1".into(),
                parent_id: None,
                timestamp: "2026-04-16T00:00:01Z".into(),
            },
            message: AgentMessage::User {
                content: MessageContent::Text("hi".into()),
                timestamp: 1,
                extra: HashMap::new(),
            },
            extra: HashMap::new(),
        };
        PiSession {
            header: header.clone(),
            entries: vec![Entry::Session(header), msg],
            file_path: PathBuf::from("/tmp/fake.jsonl"),
            parent: None,
        }
    }

    #[test]
    fn test_derive_path_wraps_provider() {
        let session = make_session("abcd1234xxxx");
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");
        assert_eq!(path.steps.len(), 1);
        assert!(
            path.path.id.starts_with("path-pi-"),
            "got: {}",
            path.path.id
        );
        // The shared derivation tags conversation paths with `meta.kind`.
        assert_eq!(
            path.meta.as_ref().unwrap().kind.as_deref(),
            Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)
        );
    }

    #[test]
    fn test_derive_path_respects_config_overrides() {
        let session = make_session("abcd1234");
        let cfg = DeriveConfig {
            path_id: Some("custom-id".into()),
            ..Default::default()
        };
        let path = derive_path(&session, &cfg).expect("derive");
        assert_eq!(path.path.id, "custom-id");
    }

    #[test]
    fn test_derive_graph_empty_sessions() {
        let g = derive_graph(&[], None, &DeriveConfig::default()).expect("derive");
        assert!(g.paths.is_empty());
        assert_eq!(g.graph.id, "graph-pi-empty");
    }

    #[test]
    fn test_derive_graph_single_session() {
        let s = make_session("sess-alpha");
        let g =
            derive_graph(std::slice::from_ref(&s), None, &DeriveConfig::default()).expect("derive");
        assert_eq!(g.paths.len(), 1);
        assert!(matches!(&g.paths[0], PathOrRef::Path(_)));
    }

    #[test]
    fn test_derive_graph_multiple_sessions() {
        let s1 = make_session("sess-one");
        let s2 = make_session("sess-two");
        let g = derive_graph(&[s1, s2], None, &DeriveConfig::default()).expect("derive");
        assert_eq!(g.paths.len(), 2);
    }

    #[test]
    fn test_derive_graph_with_title() {
        let s = make_session("sess-alpha");
        let g = derive_graph(&[s], Some("My Release"), &DeriveConfig::default()).expect("derive");
        assert_eq!(
            g.meta.as_ref().and_then(|m| m.title.as_deref()),
            Some("My Release")
        );
    }

    #[test]
    fn test_derive_graph_no_title() {
        let s = make_session("sess-alpha");
        let g = derive_graph(&[s], None, &DeriveConfig::default()).expect("derive");
        assert!(g.meta.is_none());
    }
}
