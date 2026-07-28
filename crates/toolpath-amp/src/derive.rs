//! Derive Toolpath documents from Amp threads.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]: convert
//! the session to a provider-agnostic [`toolpath_convo::ConversationView`]
//! via [`crate::provider::to_view`] and hand off. All Amp-specific data
//! (cwd, file mutations, producer, per-message tokens) is captured during
//! `to_view`; this module only sets the title and any CLI overrides.

use crate::provider::to_view;
use crate::types::Session;
use toolpath::v1::Path;

/// Configuration for deriving a Toolpath Path from an Amp session.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to the thread's working tree
    /// (`env.initial.trees[0].uri`).
    pub project_path: Option<String>,
}

/// Derive a [`Path`] from an Amp [`Session`].
///
/// The path id and title carry the **whole** thread id, not the shared
/// helper's 8-character prefix. An Amp id is `T-<uuidv7>`, so those 8
/// characters are `"T-"` plus only 6 hex digits of the 48-bit millisecond
/// timestamp — a 2²⁴ ms ≈ 4.7-hour bucket in which every thread collides.
/// That collision reaches the document id, the title, and (through
/// `cache::make_id`) the cache filename, where `path share`'s
/// `force = true` write silently overwrites the earlier thread's document.
pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    let view = to_view(session);
    let base_uri = config.project_path.as_ref().map(|p| {
        if p.starts_with('/') {
            format!("file://{}", p)
        } else {
            p.clone()
        }
    });
    let cfg = toolpath_convo::DeriveConfig {
        base_uri,
        path_id: Some(format!("path-amp-{}", view.id)),
        title: Some(format!("Amp session: {}", view.id)),
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
    use crate::reader::ExportReader;
    use toolpath::v1::Graph;

    const REAL: &str = include_str!("../tests/fixtures/real-session.json");

    fn real_session() -> Session {
        Session::from_export(ExportReader::parse_export(REAL).unwrap())
    }

    fn synthetic_session() -> Session {
        let json = r#"{
            "id": "T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0000",
            "v": 5,
            "title": "Synthetic",
            "created": 1785177254351,
            "updatedAt": "2026-07-27T18:35:20.054Z",
            "env": {"initial": {"trees": [{"uri": "file:///tmp/proj", "displayName": "proj"}],
                     "platform": {"clientVersion": "0.0.1785170481-ga5b614"}}},
            "meta": {"visibility": "private"},
            "messages": [
                {"role": "user", "meta": {"sentAt": 1785177255723}, "messageId": 1,
                 "protocolMessageID": "M-u1",
                 "content": [{"type": "text", "text": "add a main"}]},
                {"role": "assistant", "messageId": 2, "protocolMessageID": "M-a1",
                 "state": {"type": "complete", "stopReason": "tool_use"},
                 "usage": {"model": "gpt-5.6-sol", "timestamp": "2026-07-27T18:34:20.295Z",
                           "inputTokens": 0, "outputTokens": 42, "maxInputTokens": 272000,
                           "totalInputTokens": 100, "cacheReadInputTokens": 60,
                           "cacheCreationInputTokens": 40},
                 "content": [
                    {"type": "thinking", "provider": "openai", "thinking": "plan the write"},
                    {"type": "text", "text": "Writing the file."},
                    {"type": "tool_use", "id": "TU-1", "name": "apply_patch",
                     "input": {"patchText": "*** Begin Patch\n*** Add File: a.rs\n+fn main() {}\n*** End Patch"}}]},
                {"role": "user", "messageId": 3, "protocolMessageID": "M-r1",
                 "content": [{"type": "tool_result", "toolUseID": "TU-1",
                    "run": {"result": {"files": [{"uri": "file:///tmp/proj/a.rs",
                        "diff": "Index: /tmp/proj/a.rs\n===================================================================\n--- /tmp/proj/a.rs\n+++ /tmp/proj/a.rs\n@@ -0,0 +1,1 @@\n+fn main() {}\n",
                        "type": "add", "additions": 1, "deletions": 0}],
                        "summary": "add: /tmp/proj/a.rs (+1/-0)"}, "status": "done"}}]},
                {"role": "assistant", "messageId": 4, "protocolMessageID": "M-a2",
                 "state": {"type": "complete", "stopReason": "end_turn"},
                 "usage": {"model": "gpt-5.6-sol", "timestamp": "2026-07-27T18:34:24.672Z",
                           "inputTokens": 0, "outputTokens": 7, "maxInputTokens": 272000,
                           "totalInputTokens": 140, "cacheReadInputTokens": 100,
                           "cacheCreationInputTokens": 40},
                 "content": [{"type": "text", "text": "Done."}]}
            ]
        }"#;
        Session::from_export(ExportReader::parse_export_with(json, true).unwrap())
    }

    #[test]
    fn derive_path_basic() {
        let path = derive_path(&synthetic_session(), &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-amp-"));
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///tmp/proj");
        assert_eq!(
            path.meta.as_ref().unwrap().title.as_deref(),
            Some("Amp session: T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0000")
        );
    }

    /// Two threads created inside the same ~4.7-hour UUIDv7 window must
    /// derive distinct path ids. The shared helper's 8-char prefix is
    /// `"T-"` + 6 hex of the millisecond timestamp, so it cannot tell them
    /// apart — and the collision propagates to the cache filename, where
    /// `path share` overwrites with `force = true`.
    #[test]
    fn threads_in_the_same_time_bucket_get_distinct_path_ids() {
        let with_id = |id: &str| {
            let json = format!(
                r#"{{"id":"{id}","messages":[{{"role":"user","protocolMessageID":"M-1",
                    "content":[{{"type":"text","text":"hi"}}]}}]}}"#
            );
            derive_path(
                &Session::from_export(ExportReader::parse_export(&json).unwrap()),
                &DeriveConfig::default(),
            )
        };
        // Real ids from the capture machine: the trivial thread and the
        // feature-elicit thread, 3 minutes apart. Both truncate to
        // "T-019fa4".
        let a = with_id("T-019fa4d8-e7f1-750a-bf58-553bdf92f0e1");
        let b = with_id("T-019fa4db-29cf-70c9-8d9b-81524df70e52");
        assert_ne!(a.path.id, b.path.id, "distinct threads, distinct path ids");
        assert_ne!(
            a.meta.as_ref().unwrap().title,
            b.meta.as_ref().unwrap().title,
            "distinct threads, distinct titles"
        );
    }

    #[test]
    fn derive_path_kind_is_agent_coding_session() {
        let path = derive_path(&synthetic_session(), &DeriveConfig::default());
        assert_eq!(
            path.meta.as_ref().unwrap().kind.as_deref(),
            Some(toolpath::v1::PATH_KIND_AGENT_CODING_SESSION)
        );
    }

    #[test]
    fn derive_path_producer_in_canonical_slot() {
        let path = derive_path(&synthetic_session(), &DeriveConfig::default());
        let producer = path
            .meta
            .as_ref()
            .unwrap()
            .extra
            .get("producer")
            .and_then(|v| v.as_object())
            .expect("meta.extra.producer object");
        assert_eq!(producer.get("name").and_then(|v| v.as_str()), Some("amp"));
        assert_eq!(
            producer.get("version").and_then(|v| v.as_str()),
            Some("0.0.1785170481-ga5b614")
        );
    }

    #[test]
    fn derive_path_file_write_emits_sibling_with_raw() {
        let path = derive_path(&synthetic_session(), &DeriveConfig::default());
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
        let path = derive_path(&real_session(), &DeriveConfig::default());
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let p = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
        assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_path_project_path_override() {
        let path = derive_path(
            &synthetic_session(),
            &DeriveConfig {
                project_path: Some("/elsewhere".into()),
            },
        );
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///elsewhere");
    }

    #[test]
    fn maximal_real_path_conforms_to_agent_coding_session_kind() {
        // The real feature-elicit capture exercises everything the kind
        // constrains: user + assistant turns, tool calls with results (one
        // errored), file mutations with raw diffs, a delegation, per-message
        // token usage, and envelope events. Its derived path must satisfy
        // the hosted kind schema (mirrors toolpath-convo/src/derive.rs's
        // kind-conformance test).
        let path = derive_path(&real_session(), &DeriveConfig::default());
        let value = serde_json::to_value(&path).unwrap();
        let schema_src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../path-cli/kinds/agent-coding-session/v1.1.0/schema.json"
        ))
        .expect("read kind schema");
        let schema: serde_json::Value = serde_json::from_str(&schema_src).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| format!("at {}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "kind-schema violations:\n{}",
            errors.join("\n")
        );
    }

    #[test]
    fn empty_thread_derives_no_steps() {
        // A fresh `amp threads new` thread exports with no messages; pin
        // that the derivation is empty (path-cli's import guard turns this
        // into a clear error instead of caching a stepless document).
        let s = Session::from_export(ExportReader::parse_export(r#"{"id":"T-empty"}"#).unwrap());
        let path = derive_path(&s, &DeriveConfig::default());
        assert!(path.steps.is_empty());
    }

    #[test]
    fn derive_path_actors_populated() {
        let path = derive_path(&real_session(), &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:gpt-5.6-sol"));
    }
}
