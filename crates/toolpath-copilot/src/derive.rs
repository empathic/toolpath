//! Derive Toolpath documents from GitHub Copilot CLI sessions.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]: convert the
//! session to a provider-agnostic [`toolpath_convo::ConversationView`] via
//! [`crate::provider::to_view`] and hand off. All Copilot-specific data (cwd,
//! file mutations, producer) is captured during `to_view`; this module only
//! sets the title and any CLI overrides.

use crate::provider::to_view;
use crate::types::Session;
use toolpath::v1::Path;

/// Configuration for deriving a Toolpath Path from a Copilot session.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to the cwd from `session.start`.
    pub project_path: Option<String>,
}

/// Derive a [`Path`] from a Copilot [`Session`].
pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    let view = to_view(session);
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
        title: Some(format!("Copilot session: {}", prefix)),
        ..Default::default()
    };
    toolpath_convo::derive_path(&view, &cfg)
}

/// Derive a [`Path`] from multiple sessions. Used for bulk exports.
pub fn derive_project(sessions: &[Session], config: &DeriveConfig) -> Vec<Path> {
    sessions.iter().map(|s| derive_path(s, config)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::EventReader;
    use std::fs;
    use tempfile::TempDir;
    use toolpath::v1::Graph;

    fn fixture_session(body: &str) -> (TempDir, Session) {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("session-state").join("sess-019dabc6");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), body).unwrap();
        let session = EventReader::read_session_dir(&dir).unwrap();
        (temp, session)
    }

    fn minimal_body() -> String {
        [
            r#"{"type":"session.start","timestamp":"2026-06-30T10:00:00.000Z","data":{"copilotVersion":"1.0.66","model":"gpt-5-copilot","context":{"cwd":"/tmp/proj"}}}"#,
            r#"{"type":"user.message","timestamp":"2026-06-30T10:00:01.000Z","data":{"content":"build me a thing"}}"#,
            r#"{"type":"assistant.turn_start","timestamp":"2026-06-30T10:00:02.000Z","data":{}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:03.000Z","data":{"content":"creating"}}"#,
            r#"{"type":"tool.execution_start","timestamp":"2026-06-30T10:00:04.000Z","data":{"toolCallId":"c2","toolName":"create_file","arguments":{"path":"a.rs","content":"fn main() {}\n"}}}"#,
            r#"{"type":"tool.execution_complete","timestamp":"2026-06-30T10:00:05.000Z","data":{"toolCallId":"c2","success":true}}"#,
            r#"{"type":"assistant.message","timestamp":"2026-06-30T10:00:06.000Z","data":{"content":"done"}}"#,
            r#"{"type":"assistant.turn_end","timestamp":"2026-06-30T10:00:07.000Z","data":{}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn derive_path_basic() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-copilot-"));
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///tmp/proj");
    }

    #[test]
    fn derive_path_actors_populated() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:gpt-5-copilot"));
    }

    #[test]
    fn derive_path_producer_in_canonical_slot() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        let meta_extra = &path.meta.as_ref().unwrap().extra;
        let producer = meta_extra
            .get("producer")
            .and_then(|v| v.as_object())
            .expect("meta.extra.producer object");
        assert_eq!(
            producer.get("name").and_then(|v| v.as_str()),
            Some("copilot-cli")
        );
        assert_eq!(
            producer.get("version").and_then(|v| v.as_str()),
            Some("1.0.66")
        );
    }

    #[test]
    fn derive_path_file_write_emits_sibling() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        let file_step = path
            .steps
            .iter()
            .find(|s| s.change.contains_key("a.rs"))
            .expect("no step carries the file artifact");
        let change = &file_step.change["a.rs"];
        assert!(change.raw.is_some(), "raw perspective must be populated");
        assert!(change.raw.as_ref().unwrap().contains("+fn main() {}"));
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "file.write");
        assert_eq!(structural.extra["operation"], "add");
    }

    #[test]
    fn derive_path_validates_as_single_path_graph() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let p = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
        assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_path_base_carries_workspace_git_context() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("session-state").join("sess-ws");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), minimal_body()).unwrap();
        fs::write(
            dir.join("workspace.yaml"),
            "git_root: /tmp/proj\nrepository: git@github.com:o/r.git\nbranch: main\ncommit: deadbeef\n",
        )
        .unwrap();
        let session = EventReader::read_session_dir(&dir).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let base = path.path.base.as_ref().unwrap();
        assert_eq!(base.uri, "file:///tmp/proj");
        assert_eq!(base.branch.as_deref(), Some("main"));
        assert_eq!(base.ref_str.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn derive_path_kind_is_agent_coding_session() {
        let (_t, session) = fixture_session(&minimal_body());
        let path = derive_path(&session, &DeriveConfig::default());
        assert_eq!(
            path.meta.as_ref().unwrap().kind.as_deref(),
            Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)
        );
    }
}
