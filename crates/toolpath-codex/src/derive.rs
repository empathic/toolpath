//! Derive Toolpath documents from Codex CLI sessions.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]: convert
//! the session to a provider-agnostic [`toolpath_convo::ConversationView`]
//! via [`crate::provider::to_view`] and hand off. All Codex-specific data
//! (cwd, git, file diffs from `patch_apply_end`, codex meta aggregates) is
//! captured during `to_view`; this module only sets the title and any
//! CLI overrides.

use crate::provider::to_view;
use crate::types::Session;
use toolpath::v1::Path;

/// Configuration for deriving a Toolpath Path from a Codex session.
///
/// Note: there's no `include_thinking` toggle like the other providers
/// have. Codex's reasoning is almost always encrypted ciphertext from
/// OpenAI's servers — not useful in a human-readable digest. Plaintext
/// reasoning summaries (rare) land on `Turn.thinking` automatically
/// and surface in the derived path without a flag. The raw ciphertext
/// is dropped: the IR carries no provider-namespaced extras field to
/// preserve it in, so it never reaches `Turn` and cannot be recovered
/// on a round-trip (see `encrypted_reasoning_does_not_land_on_thinking`).
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to the cwd from session_meta.
    pub project_path: Option<String>,
}

/// Derive a [`Path`] from a Codex [`Session`].
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
        title: Some(format!("Codex session: {}", prefix)),
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
    use crate::CodexConvo;
    use std::fs;
    use tempfile::TempDir;
    use toolpath::v1::Graph;

    fn fixture_session(body: &str) -> (TempDir, CodexConvo, String) {
        let temp = TempDir::new().unwrap();
        let codex = temp.path().join(".codex");
        let day = codex.join("sessions/2026/04/20");
        fs::create_dir_all(&day).unwrap();
        let name = "rollout-2026-04-20T10-00-00-019dabc6-8fef-7681-a054-b5bb75fcb97d";
        fs::write(day.join(format!("{}.jsonl", name)), body).unwrap();
        let resolver = crate::PathResolver::new().with_codex_dir(&codex);
        (temp, CodexConvo::with_resolver(resolver), name.into())
    }

    fn minimal_body() -> String {
        [
            r#"{"timestamp":"2026-04-20T16:44:37.772Z","type":"session_meta","payload":{"id":"019dabc6-8fef-7681-a054-b5bb75fcb97d","timestamp":"2026-04-20T16:43:30.171Z","cwd":"/tmp/proj","originator":"codex-tui","cli_version":"0.118.0","source":"cli","git":{"commit_hash":"abc","branch":"main","repository_url":"git@example:x/y.git"}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:37.773Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/tmp/proj","model":"gpt-5.4"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:37.800Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"build me a thing"}]}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.100Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"creating"}],"phase":"commentary"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.500Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c2","name":"apply_patch","input":"*** Begin Patch\n*** Add File: /tmp/proj/a.rs\n+fn main() {}\n*** End Patch"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.700Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"c2","success":true,"changes":{"/tmp/proj/a.rs":{"type":"add","content":"fn main() {}\n"}}}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.900Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}],"phase":"final","end_turn":true}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn derive_path_basic() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());

        assert!(path.path.id.starts_with("path-codex-"));
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///tmp/proj");
        assert_eq!(
            path.path.base.as_ref().unwrap().ref_str.as_deref(),
            Some("abc")
        );
        assert_eq!(
            path.path.base.as_ref().unwrap().branch.as_deref(),
            Some("main")
        );
    }

    #[test]
    fn derive_path_actors_populated() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:gpt-5.4"));
    }

    #[test]
    fn derive_path_producer_in_canonical_slot() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let meta_extra = &path.meta.as_ref().unwrap().extra;
        // Producer (originator + cli_version) lives in its canonical slot.
        let producer = meta_extra
            .get("producer")
            .and_then(|v| v.as_object())
            .expect("meta.extra.producer object");
        assert_eq!(
            producer.get("name").and_then(|v| v.as_str()),
            Some("codex-tui")
        );
        assert_eq!(
            producer.get("version").and_then(|v| v.as_str()),
            Some("0.118.0")
        );
        // Nothing else codex-specific is smuggled through meta.extra.
        assert!(!meta_extra.contains_key("codex"));
    }

    #[test]
    fn derive_path_apply_patch_emits_file_write_sibling() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        // The assistant turn that ran `apply_patch` carries a sibling
        // `file.write` entry keyed by the file path.
        let file_step = path
            .steps
            .iter()
            .find(|s| s.change.contains_key("/tmp/proj/a.rs"))
            .expect("no step carries the file artifact");
        let change = &file_step.change["/tmp/proj/a.rs"];
        assert!(change.raw.is_some(), "raw perspective must be populated");
        assert!(
            change.raw.as_ref().unwrap().contains("+fn main() {}"),
            "raw must be a unified diff"
        );
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "file.write");
        assert_eq!(structural.extra["operation"], "add");
    }

    #[test]
    fn derive_path_validates_as_single_path_graph() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let p = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
        assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_project_per_session() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let s1 = mgr.read_session(&id).unwrap();
        let paths = derive_project(std::slice::from_ref(&s1), &DeriveConfig::default());
        assert_eq!(paths.len(), 1);
    }
}
