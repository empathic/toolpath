//! Thin wrapper: [`OpenClawSession`] -> [`ConversationView`] ->
//! [`toolpath_convo::derive_path`].
//!
//! Beyond the shared derivation, this layer (1) sets a channel-aware
//! `user_actor` from the session's routing key and (2) stashes OpenClaw's
//! channel/peer/session-kind metadata on `path.meta.extra["openclaw"]`.

use serde_json::Value;
use toolpath::v1::{Graph, GraphIdentity, GraphMeta, Path, PathOrRef};

pub use toolpath_convo::DeriveConfig;

use crate::provider::{openclaw_meta_extra, session_to_view, user_actor_for};
use crate::reader::OpenClawSession;

/// Derive a Toolpath [`Path`] from a single OpenClaw session.
pub fn derive_path(session: &OpenClawSession, config: &DeriveConfig) -> Path {
    let view = session_to_view(session);

    let mut cfg = config.clone();
    if cfg.user_actor.is_none() {
        cfg.user_actor = user_actor_for(session.parsed_key.as_ref());
    }

    let mut path = toolpath_convo::derive_path(&view, &cfg);

    let extra = openclaw_meta_extra(session);
    if !extra.is_empty() {
        let meta = path.meta.get_or_insert_with(Default::default);
        meta.extra
            .insert("openclaw".to_string(), Value::Object(extra));
    }

    path
}

/// Derive a Toolpath [`Graph`] from multiple OpenClaw sessions (one path each).
pub fn derive_graph(
    sessions: &[OpenClawSession],
    title: Option<&str>,
    config: &DeriveConfig,
) -> Graph {
    let id_suffix = sessions
        .first()
        .map(|s| s.header.id.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "empty".to_string());
    let graph_id = format!("graph-openclaw-{id_suffix}");

    let paths: Vec<PathOrRef> = sessions
        .iter()
        .map(|s| PathOrRef::Path(Box::new(derive_path(s, config))))
        .collect();

    let meta = title.map(|t| GraphMeta {
        title: Some(t.to_string()),
        ..Default::default()
    });

    Graph {
        graph: GraphIdentity { id: graph_id },
        paths,
        meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read_session_from_file;
    use std::path::Path as FsPath;
    use toolpath::v1::PATH_KIND_AGENT_CODING_SESSION;

    fn fixture(with_key: bool) -> OpenClawSession {
        let mut s = read_session_from_file(FsPath::new("tests/fixtures/dm_session.jsonl")).unwrap();
        if with_key {
            s.attach_routing_key();
        }
        s
    }

    #[test]
    fn derive_sets_channel_actor_kind_and_meta() {
        let s = fixture(true);
        let p = derive_path(&s, &DeriveConfig::default());

        let user_step = p
            .steps
            .iter()
            .find(|st| st.step.actor.starts_with("human:"))
            .expect("a human step");
        assert_eq!(user_step.step.actor, "human:whatsapp/15555550123");

        let meta = p.meta.as_ref().unwrap();
        assert_eq!(meta.source.as_deref(), Some("openclaw"));
        assert_eq!(meta.kind.as_deref(), Some(PATH_KIND_AGENT_CODING_SESSION));
        assert_eq!(meta.extra["openclaw"]["channel"], "whatsapp");
        assert_eq!(meta.extra["openclaw"]["sessionKind"], "direct");
    }

    #[test]
    fn derive_without_routing_key_defaults_to_human_user() {
        let s = fixture(false);
        let p = derive_path(&s, &DeriveConfig::default());
        let user_step = p
            .steps
            .iter()
            .find(|st| st.step.actor.starts_with("human:"))
            .expect("a human step");
        assert_eq!(user_step.step.actor, "human:user");
        // No routing key → no openclaw channel metadata, but sessionKind present.
        let meta = p.meta.as_ref().unwrap();
        assert_eq!(meta.extra["openclaw"]["sessionKind"], "main");
    }

    #[test]
    fn derive_graph_basics() {
        let s = fixture(true);
        let g = derive_graph(std::slice::from_ref(&s), Some("Release"), &DeriveConfig::default());
        assert_eq!(g.paths.len(), 1);
        assert_eq!(g.meta.as_ref().and_then(|m| m.title.as_deref()), Some("Release"));
        assert!(g.graph.id.starts_with("graph-openclaw-"));

        let empty = derive_graph(&[], None, &DeriveConfig::default());
        assert!(empty.paths.is_empty());
        assert_eq!(empty.graph.id, "graph-openclaw-empty");
    }
}
