//! Derive Toolpath documents from opencode sessions.
//!
//! Thin wrapper around the shared [`toolpath_convo::derive_path`]. All
//! opencode-specific work (snapshot git2 tree↔tree diffs, tool-input
//! fallback for gitignored paths, producer/base population) happens in
//! [`crate::provider::to_view_with_resolver`]; nothing provider-specific
//! lives in this module.

use crate::paths::PathResolver;
use crate::provider::{to_view, to_view_with_resolver};
use crate::types::Session;
use toolpath::v1::Path;

/// Configuration for deriving a Toolpath `Path` from an opencode session.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to `file://<session.directory>`.
    pub project_path: Option<String>,
    /// Skip the git2 snapshot-diff IO. Useful for tests with no
    /// snapshot repo on disk.
    pub no_snapshot_diffs: bool,
}

/// Derive a [`Path`] from an opencode [`Session`]. Snapshot diffs need
/// a resolver: use [`derive_path_with_resolver`] for them.
pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    derive_from_view(to_view(session), session, config)
}

/// Like [`derive_path`] but with a `PathResolver`, so the snapshot git
/// repository supplies file diffs.
pub fn derive_path_with_resolver(
    session: &Session,
    config: &DeriveConfig,
    resolver: &PathResolver,
) -> Path {
    let view = if config.no_snapshot_diffs {
        to_view(session)
    } else {
        to_view_with_resolver(session, resolver)
    };
    derive_from_view(view, session, config)
}

fn derive_from_view(
    view: toolpath_convo::ConversationView,
    session: &Session,
    config: &DeriveConfig,
) -> Path {
    let base_uri = config.project_path.as_ref().map(|p| {
        if p.starts_with('/') {
            format!("file://{}", p)
        } else {
            p.clone()
        }
    });
    let cfg = toolpath_convo::DeriveConfig {
        base_uri,
        title: Some(format!("opencode session: {}", session.title)),
        ..Default::default()
    };
    toolpath_convo::derive_path(&view, &cfg)
}

/// Derive a `Path` from multiple sessions.
pub fn derive_project(sessions: &[Session], config: &DeriveConfig) -> Vec<Path> {
    sessions.iter().map(|s| derive_path(s, config)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpencodeConvo;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;
    use toolpath::v1::Graph;

    /// Fixture with the real opencode schema (matches what the SQLite
    /// reader expects) but no snapshot git repo on disk. Tests run with
    /// `no_snapshot_diffs: true` so the tool-input fallback kicks in.
    fn fixture(body_sql: &str) -> (TempDir, OpencodeConvo, PathResolver) {
        let temp = TempDir::new().unwrap();
        let data_dir = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();
        let db_path = data_dir.join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE project (
              id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
              icon_url text, icon_color text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_initialized integer, sandboxes text NOT NULL, commands text
            );
            CREATE TABLE session (
              id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
              slug text NOT NULL, directory text NOT NULL, title text NOT NULL,
              version text NOT NULL, share_url text,
              summary_additions integer, summary_deletions integer,
              summary_files integer, summary_diffs text, revert text, permission text,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              time_compacting integer, time_archived integer, workspace_id text
            );
            CREATE TABLE message (
              id text PRIMARY KEY, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            CREATE TABLE part (
              id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
              time_created integer NOT NULL, time_updated integer NOT NULL,
              data text NOT NULL
            );
            {body_sql}
            "#
        ))
        .unwrap();
        drop(conn);
        let resolver = PathResolver::new(temp.path()).with_data_dir(&data_dir);
        let mgr = OpencodeConvo::with_resolver(resolver.clone());
        (temp, mgr, resolver)
    }

    const BASIC_SQL: &str = r#"
        INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
          VALUES ('proj_sha', '/tmp/proj', 1000, 3000, '[]');
        INSERT INTO session (id, project_id, slug, directory, title, version,
                             time_created, time_updated)
          VALUES ('ses_abc123', 'proj_sha', 'pickle-a-thing', '/tmp/proj', 'Pickle a thing', '0.10.0', 1000, 1100);
        INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
          ('m1', 'ses_abc123', 1001, 1001, '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
          ('m2', 'ses_abc123', 1002, 1100, '{"parentID":"m1","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1002,"completed":1100},"finish":"stop"}');
        INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES
          ('p1','m1','ses_abc123',1001,1001,'{"type":"text","text":"make a pickle"}'),
          ('p2','m2','ses_abc123',1002,1002,'{"type":"step-start","snapshot":"snap_a"}'),
          ('p3','m2','ses_abc123',1005,1005,'{"type":"tool","tool":"write","callID":"c1","state":{"status":"completed","input":{"filePath":"/tmp/proj/main.cpp","content":"int main(){}\n"},"output":"wrote","title":"Write","metadata":{"bytes":13},"time":{"start":1005,"end":1006}}}'),
          ('p4','m2','ses_abc123',1007,1007,'{"type":"text","text":"done"}'),
          ('p5','m2','ses_abc123',1010,1010,'{"type":"step-finish","reason":"stop","snapshot":"snap_b","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"cost":0.01}');
    "#;

    #[test]
    fn derive_basic_shape() {
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let s = mgr.read_session("ses_abc123").unwrap();
        let p = derive_path_with_resolver(
            &s,
            &DeriveConfig {
                no_snapshot_diffs: true,
                ..Default::default()
            },
            &resolver,
        );

        assert!(p.path.id.starts_with("path-opencode-"));
        assert_eq!(p.path.base.as_ref().unwrap().uri, "file:///tmp/proj");
        assert_eq!(
            p.path.base.as_ref().unwrap().ref_str.as_deref(),
            Some("proj_sha")
        );
        // 2 messages → 2 turns (both have content/tool calls).
        assert_eq!(
            p.steps
                .iter()
                .filter(|s| {
                    s.change.values().any(|c| {
                        c.structural
                            .as_ref()
                            .is_some_and(|sc| sc.change_type == "conversation.append")
                    })
                })
                .count(),
            2
        );
    }

    #[test]
    fn derive_emits_producer() {
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let s = mgr.read_session("ses_abc123").unwrap();
        let p = derive_path_with_resolver(
            &s,
            &DeriveConfig {
                no_snapshot_diffs: true,
                ..Default::default()
            },
            &resolver,
        );
        let producer = p.meta.as_ref().unwrap().extra.get("producer").unwrap();
        assert_eq!(producer["name"], "opencode");
        assert_eq!(producer["version"], "0.10.0");
    }

    #[test]
    fn derive_fallback_file_mutation_from_tool() {
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let s = mgr.read_session("ses_abc123").unwrap();
        let p = derive_path_with_resolver(
            &s,
            &DeriveConfig {
                no_snapshot_diffs: true,
                ..Default::default()
            },
            &resolver,
        );
        // The assistant turn's `write` tool produces a sibling `file.write`
        // entry via the tool-input fallback (no snapshot repo on disk).
        let file_step = p
            .steps
            .iter()
            .find(|s| s.change.contains_key("/tmp/proj/main.cpp"))
            .expect("no step carries the file artifact");
        let change = &file_step.change["/tmp/proj/main.cpp"];
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "file.write");
        assert_eq!(structural.extra["operation"], "add");
        // tool_id links back to the write tool.
        assert_eq!(structural.extra["tool_id"], "c1");
    }

    #[test]
    fn derive_validates_as_single_path_graph() {
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let s = mgr.read_session("ses_abc123").unwrap();
        let p = derive_path_with_resolver(
            &s,
            &DeriveConfig {
                no_snapshot_diffs: true,
                ..Default::default()
            },
            &resolver,
        );
        let doc = Graph::from_path(p);
        let json = doc.to_json().unwrap();
        let parsed = Graph::from_json(&json).unwrap();
        let pp = parsed.single_path().expect("single-path graph");
        let anc = toolpath::v1::query::ancestors(&pp.steps, &pp.path.head);
        assert_eq!(anc.len(), pp.steps.len(), "all steps on head ancestry");
    }
}
