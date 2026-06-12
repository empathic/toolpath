//! Derive Toolpath documents from Cursor sessions.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]. All
//! Cursor-specific work (content-blob lookup, tool classification,
//! producer/base population) happens in
//! [`crate::provider::session_to_view`]; nothing provider-specific
//! lives in this module.

use crate::provider::session_to_view;
use crate::types::CursorSession;
use toolpath::v1::Path;

/// Configuration for deriving a Toolpath `Path` from a Cursor session.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to `file://<workspace>` when
    /// the composer's workspace identifier is known.
    pub project_path: Option<String>,
    /// Override `path.meta.title`. Defaults to
    /// `"cursor session: <composer-id-prefix>"` (via the shared
    /// derive's default).
    pub title: Option<String>,
}

/// Derive a [`Path`] from a Cursor [`CursorSession`].
pub fn derive_path(session: &CursorSession, config: &DeriveConfig) -> toolpath_convo::Result<Path> {
    let view = session_to_view(session);
    let base_uri = config.project_path.as_ref().map(|p| {
        if p.starts_with('/') {
            format!("file://{}", p)
        } else {
            p.clone()
        }
    });
    let title = config
        .title
        .clone()
        .or_else(|| session.title().map(|t| format!("cursor session: {t}")));
    let cfg = toolpath_convo::DeriveConfig {
        base_uri,
        title,
        ..Default::default()
    };
    toolpath_convo::derive_path(&view, &cfg)
}

/// Derive a `Path` from each of several Cursor sessions.
pub fn derive_project(
    sessions: &[CursorSession],
    config: &DeriveConfig,
) -> toolpath_convo::Result<Vec<Path>> {
    sessions.iter().map(|s| derive_path(s, config)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CursorConvo;
    use crate::reader::tests::{BASIC_FIXTURE, fixture_db};
    use std::fs;
    use tempfile::TempDir;
    use toolpath::v1::Graph;

    fn setup() -> (TempDir, CursorConvo) {
        let temp = TempDir::new().unwrap();
        let user_data = temp.path().join("UserData");
        let global = user_data.join("User").join("globalStorage");
        fs::create_dir_all(&global).unwrap();
        let src = fixture_db(BASIC_FIXTURE);
        fs::copy(src.path(), global.join("state.vscdb")).unwrap();
        let resolver = crate::PathResolver::new()
            .with_home(temp.path())
            .with_anysphere_dir(temp.path().join(".cursor"))
            .with_user_data_dir(user_data);
        (temp, CursorConvo::with_resolver(resolver))
    }

    #[test]
    fn derive_basic_shape() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");

        assert!(path.path.id.starts_with("path-cursor-"));
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///p");

        let conversation_steps = path
            .steps
            .iter()
            .filter(|s| {
                s.change.values().any(|c| {
                    c.structural
                        .as_ref()
                        .is_some_and(|sc| sc.change_type == "conversation.append")
                })
            })
            .count();
        // 3 bubbles → 3 conversation.append steps.
        assert_eq!(conversation_steps, 3);
    }

    #[test]
    fn derive_emits_producer() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");
        let producer = path.meta.as_ref().unwrap().extra.get("producer").unwrap();
        assert_eq!(producer["name"], "cursor");
        assert_eq!(producer["version"], "cursor-agent");
    }

    #[test]
    fn derive_emits_file_write_with_real_diff() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");
        let file_step = path
            .steps
            .iter()
            .find(|s| s.change.contains_key("/p/x.rs"))
            .expect("no step carries /p/x.rs");
        let change = &file_step.change["/p/x.rs"];
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "file.write");
        assert_eq!(structural.extra["operation"], "add");
        assert_eq!(structural.extra["tool_id"], "tool_a");
        assert_eq!(structural.extra["tool"], "edit_file_v2");
        // unified_diff was generated from the resolved blobs.
        let raw = change.raw.as_ref().expect("expected raw unified diff");
        assert!(raw.contains("+fn main() {}"));
    }

    #[test]
    fn derive_emits_path_kind_marker() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");
        let kind = path.meta.as_ref().unwrap().kind.as_deref().unwrap();
        assert_eq!(kind, toolpath::v1::PATH_KIND_AGENT_CODING_SESSION);
    }

    #[test]
    fn derive_validates_as_single_path_graph() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(&session, &DeriveConfig::default()).expect("derive");
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let pp = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&pp.steps, &pp.path.head);
        assert_eq!(anc.len(), pp.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_title_override() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let path = derive_path(
            &session,
            &DeriveConfig {
                title: Some("explicit".into()),
                ..Default::default()
            },
        )
        .expect("derive");
        assert_eq!(path.meta.unwrap().title.unwrap(), "explicit");
    }
}
