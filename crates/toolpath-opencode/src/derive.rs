//! Derive Toolpath documents from opencode sessions.
//!
//! Each `Turn` becomes a `Step`. Every step's `change` map carries:
//!
//! - One entry at `opencode://<session-id>` with a
//!   `conversation.append` structural op describing the turn's text,
//!   thinking, and tool-call summaries.
//! - Sibling entries for each file touched between the turn's
//!   snapshot endpoints. When the snapshot git repo is on disk,
//!   `ArtifactChange.raw` is the real unified diff from git. Otherwise
//!   we fall back to file paths reported by tool inputs with no
//!   `raw` perspective.

use crate::paths::PathResolver;
use crate::provider::{to_view, tool_category};
use crate::types::Session;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Identity, Path, PathIdentity, PathMeta, Step,
    StepIdentity, StructuralChange,
};
use toolpath_convo::{ConversationView, Role, Turn};

/// Configuration for deriving a Toolpath `Path` from an opencode
/// session.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to `file://<session.directory>`.
    pub project_path: Option<String>,
    /// Disable snapshot-based file diff extraction even when the
    /// snapshot repo is on disk. Useful for tests / offline runs.
    pub no_snapshot_diffs: bool,
}

/// Derive a `Path` from a loaded opencode `Session`.
pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    let view = to_view(session);
    derive_path_from_view(session, &view, config, &PathResolver::new())
}

/// Like [`derive_path`] but with a custom `PathResolver` (useful for
/// tests with a temp data directory).
pub fn derive_path_with_resolver(
    session: &Session,
    config: &DeriveConfig,
    resolver: &PathResolver,
) -> Path {
    let view = to_view(session);
    derive_path_from_view(session, &view, config, resolver)
}

/// Derive a `Path` from multiple sessions.
pub fn derive_project(sessions: &[Session], config: &DeriveConfig) -> Vec<Path> {
    sessions.iter().map(|s| derive_path(s, config)).collect()
}

fn derive_path_from_view(
    session: &Session,
    view: &ConversationView,
    config: &DeriveConfig,
    resolver: &PathResolver,
) -> Path {
    let session_short: String = session
        .id
        .trim_start_matches("ses_")
        .chars()
        .take(8)
        .collect();
    let path_id = format!("path-opencode-{}", session_short);
    let convo_artifact = format!("opencode://{}", session.id);

    // Open the snapshot git repo if present. A single open for the
    // whole derive is fine — git2 is thread-local enough for our needs.
    let snapshot_repo: Option<git2::Repository> = if config.no_snapshot_diffs {
        None
    } else {
        resolver
            .snapshot_gitdir(&session.project_id, &session.directory)
            .ok()
            .and_then(|gd| git2::Repository::open(gd).ok())
    };

    let mut steps: Vec<Step> = Vec::with_capacity(view.turns.len());
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();
    let mut last_step_id: Option<String> = None;
    let mut prev_snapshot_after: Option<String> = None;
    let mut all_files: Vec<String> = Vec::new();
    let mut files_seen = std::collections::HashSet::<String>::new();

    for (turn_idx, turn) in view.turns.iter().enumerate() {
        let Some(step) = build_step(
            turn_idx,
            turn,
            &convo_artifact,
            last_step_id.as_deref(),
            &mut actors,
            &snapshot_repo,
            &mut prev_snapshot_after,
            &mut all_files,
            &mut files_seen,
        ) else {
            continue;
        };
        last_step_id = Some(step.step.id.clone());
        steps.push(step);
    }

    let head = last_step_id.unwrap_or_else(|| "empty".to_string());

    // Base: CLI-override wins; otherwise session.directory; fall back
    // to the first turn's working_dir.
    let base_uri = config
        .project_path
        .clone()
        .or_else(|| Some(session.directory.to_string_lossy().to_string()))
        .map(|p| {
            if p.starts_with('/') {
                format!("file://{}", p)
            } else {
                p
            }
        });
    // Base ref: first-root-commit SHA (== project_id) is a stable
    // ancestor identifier.
    let base_ref = Some(session.project_id.clone());
    let base = base_uri.map(|uri| Base {
        uri,
        ref_str: base_ref,
        branch: None,
    });

    // Top-level path meta: actors, title, source, opencode metadata.
    let mut path_extra: HashMap<String, Value> = HashMap::new();
    let mut oc: Map<String, Value> = Map::new();
    oc.insert("session_id".into(), Value::String(session.id.clone()));
    oc.insert(
        "project_id".into(),
        Value::String(session.project_id.clone()),
    );
    oc.insert("slug".into(), Value::String(session.slug.clone()));
    oc.insert("version".into(), Value::String(session.version.clone()));
    if let Some(total) = view.total_usage.as_ref() {
        oc.insert(
            "total_tokens".into(),
            serde_json::to_value(total).unwrap_or(Value::Null),
        );
    }
    if !all_files.is_empty() {
        oc.insert(
            "files_changed".into(),
            Value::Array(all_files.iter().map(|p| Value::String(p.clone())).collect()),
        );
    }
    path_extra.insert("opencode".into(), Value::Object(oc));

    Path {
        path: PathIdentity {
            id: path_id,
            base,
            head,
            graph_ref: None,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!("opencode session: {}", session.title)),
            source: Some("opencode".to_string()),
            actors: if actors.is_empty() {
                None
            } else {
                Some(actors)
            },
            extra: path_extra,
            ..Default::default()
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_step(
    turn_idx: usize,
    turn: &Turn,
    convo_artifact: &str,
    parent_id: Option<&str>,
    actors: &mut HashMap<String, ActorDefinition>,
    snapshot_repo: &Option<git2::Repository>,
    prev_snapshot_after: &mut Option<String>,
    all_files: &mut Vec<String>,
    files_seen: &mut std::collections::HashSet<String>,
) -> Option<Step> {
    // Skip empty carrier turns.
    if turn.text.is_empty() && turn.tool_uses.is_empty() && turn.thinking.is_none() {
        return None;
    }

    let (actor, role_str) = resolve_actor(turn, actors);

    let mut convo_extra: HashMap<String, Value> = HashMap::new();
    convo_extra.insert("role".into(), json!(role_str));
    if !turn.text.is_empty() {
        convo_extra.insert("text".into(), json!(turn.text));
    }
    if let Some(th) = turn.thinking.as_deref()
        && !th.is_empty()
    {
        convo_extra.insert("thinking".into(), json!(th));
    }
    if !turn.tool_uses.is_empty() {
        let calls: Vec<Value> = turn
            .tool_uses
            .iter()
            .map(|tu| {
                json!({
                    "name": tu.name,
                    "call_id": tu.id,
                    "category": tu.category,
                    "summary": tool_call_summary(tu),
                    "status": if let Some(r) = tu.result.as_ref() {
                        if r.is_error { "error" } else { "success" }
                    } else { "pending" },
                })
            })
            .collect();
        convo_extra.insert("tool_calls".into(), Value::Array(calls));
    }
    if let Some(u) = turn.token_usage.as_ref() {
        convo_extra.insert("token_usage".into(), json!(u));
    }
    if let Some(sr) = turn.stop_reason.as_deref()
        && !sr.is_empty()
    {
        convo_extra.insert("stop_reason".into(), json!(sr));
    }

    let convo_change = ArtifactChange {
        raw: None,
        structural: Some(StructuralChange {
            change_type: "conversation.append".to_string(),
            extra: convo_extra,
        }),
    };

    let mut changes: HashMap<String, ArtifactChange> = HashMap::new();
    changes.insert(convo_artifact.to_string(), convo_change);

    // Extract snapshot pair (before, after) for this turn.
    let snapshots = turn
        .extra
        .get("opencode")
        .and_then(|oc| oc.get("snapshots"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (before, after) = match (snapshots.first(), snapshots.last()) {
        (Some(first), Some(last)) => {
            // The "before" state is whichever is earlier: the snapshot
            // the previous turn ended on, or the first snapshot of
            // this turn (which usually match). Prefer the prior-turn's
            // ending snapshot — it captures the pre-step state even
            // when this turn's first step-start is missing.
            let b = prev_snapshot_after.clone().unwrap_or_else(|| first.clone());
            (Some(b), Some(last.clone()))
        }
        _ => (None, None),
    };

    // First pass: pull real unified diffs from the snapshot repo for
    // files opencode could see (i.e. not gitignored).
    if let (Some(b), Some(a), Some(repo)) = (&before, &after, snapshot_repo.as_ref())
        && b != a
    {
        match diff_trees(repo, b, a) {
            Ok(file_changes) => {
                for (file_path, artifact_change) in file_changes {
                    if files_seen.insert(file_path.clone()) {
                        all_files.push(file_path.clone());
                    }
                    changes.insert(file_path, artifact_change);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: snapshot diff {}..{} failed: {}",
                    &b[..b.len().min(8)],
                    &a[..a.len().min(8)],
                    e
                );
            }
        }
    }

    // Second pass: catch files opencode could NOT see in the snapshot
    // repo — either because there's no snapshot repo, no snapshot pair,
    // or the pair's diff was empty (common when the target files are
    // under a .gitignored path). Use tool inputs as the best available
    // evidence; no `raw` perspective, but the path and operation still
    // land on the step.
    for tu in &turn.tool_uses {
        let Some(path) = tool_input_file_path(tu) else {
            continue;
        };
        if changes.contains_key(&path) {
            continue;
        }
        if files_seen.insert(path.clone()) {
            all_files.push(path.clone());
        }
        let op = tool_to_operation(&tu.name);
        let mut extra = HashMap::new();
        extra.insert("operation".into(), json!(op));
        extra.insert("tool".into(), json!(tu.name));
        extra.insert(
            "source".into(),
            json!(if snapshot_repo.is_some() {
                "tool_input_gitignored"
            } else {
                "tool_input"
            }),
        );
        changes.insert(
            path,
            ArtifactChange {
                raw: None,
                structural: Some(StructuralChange {
                    change_type: format!("opencode.{}", op),
                    extra,
                }),
            },
        );
    }

    // Advance prev_snapshot_after for the next turn.
    if let Some(a) = &after {
        *prev_snapshot_after = Some(a.clone());
    }

    let step_id = format!("step-{:04}", turn_idx + 1);
    let parents = parent_id.map(|p| vec![p.to_string()]).unwrap_or_default();

    Some(Step {
        step: StepIdentity {
            id: step_id,
            parents,
            actor,
            timestamp: turn.timestamp.clone(),
        },
        change: changes,
        meta: None,
    })
}

fn resolve_actor(
    turn: &Turn,
    actors: &mut HashMap<String, ActorDefinition>,
) -> (String, &'static str) {
    match &turn.role {
        Role::User => {
            actors
                .entry("human:user".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("User".to_string()),
                    ..Default::default()
                });
            ("human:user".to_string(), "user")
        }
        Role::Assistant => {
            let (key, model_str) = match &turn.model {
                Some(m) if !m.is_empty() => (format!("agent:{}", m), m.clone()),
                _ => ("agent:opencode".to_string(), "opencode".to_string()),
            };
            let provider = turn
                .extra
                .get("opencode")
                .and_then(|oc| oc.get("providerID"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            actors
                .entry(key.clone())
                .or_insert_with(|| ActorDefinition {
                    name: Some("opencode".to_string()),
                    provider: provider.clone(),
                    model: Some(model_str.clone()),
                    identities: provider
                        .map(|p| {
                            vec![Identity {
                                system: p,
                                id: model_str,
                            }]
                        })
                        .unwrap_or_default(),
                    ..Default::default()
                });
            (key, "assistant")
        }
        Role::System => {
            actors
                .entry("system:opencode".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("opencode system".to_string()),
                    ..Default::default()
                });
            ("system:opencode".to_string(), "system")
        }
        Role::Other(s) => {
            let key = format!("other:{}", s);
            actors
                .entry(key.clone())
                .or_insert_with(|| ActorDefinition {
                    name: Some(s.clone()),
                    ..Default::default()
                });
            (key, "other")
        }
    }
}

fn tool_call_summary(tu: &toolpath_convo::ToolInvocation) -> String {
    let pick = |k: &str| -> Option<String> {
        tu.input.get(k).and_then(|v| v.as_str()).map(str::to_string)
    };
    let s = match tu.name.as_str() {
        "bash" | "shell" | "exec" => pick("command").or_else(|| pick("cmd")),
        "read" | "list" | "view" | "ls" => pick("filePath").or_else(|| pick("path")),
        "write" | "edit" | "multiedit" | "patch" => pick("filePath")
            .or_else(|| pick("file_path"))
            .or_else(|| pick("path")),
        "glob" | "grep" | "search" => pick("pattern").or_else(|| pick("query")),
        "webfetch" | "fetch" => pick("url"),
        "websearch" => pick("query"),
        "task" | "agent" | "spawn_agent" => pick("prompt").or_else(|| pick("task")),
        _ => None,
    };
    s.unwrap_or_default()
}

fn tool_input_file_path(tu: &toolpath_convo::ToolInvocation) -> Option<String> {
    tu.input
        .get("filePath")
        .or_else(|| tu.input.get("file_path"))
        .or_else(|| tu.input.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn tool_to_operation(name: &str) -> &'static str {
    match name {
        "write" => "add",
        "edit" | "multiedit" | "patch" => "update",
        "delete" | "rm" => "delete",
        _ => "touch",
    }
}

fn diff_trees(
    repo: &git2::Repository,
    before: &str,
    after: &str,
) -> Result<Vec<(String, ArtifactChange)>, git2::Error> {
    let before_obj = repo.revparse_single(before)?;
    let after_obj = repo.revparse_single(after)?;
    let before_tree = before_obj.peel_to_tree()?;
    let after_tree = after_obj.peel_to_tree()?;

    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    opts.include_ignored(false);
    opts.ignore_submodules(true);
    let diff = repo.diff_tree_to_tree(Some(&before_tree), Some(&after_tree), Some(&mut opts))?;

    // Collect unified-diff text + typed op per file.
    let mut by_path: HashMap<PathBuf, (String, &'static str, Option<PathBuf>)> = HashMap::new();

    diff.print(git2::DiffFormat::Patch, |delta, _hunk, line| {
        let Some(new_path) = delta.new_file().path() else {
            // Handle delete: old_file path, no new
            if let Some(old) = delta.old_file().path() {
                let buf = by_path
                    .entry(old.to_path_buf())
                    .or_insert_with(|| (String::new(), "delete", None));
                append_diff_line(&mut buf.0, line);
            }
            return true;
        };
        let op = classify_delta(&delta);
        let entry = by_path.entry(new_path.to_path_buf()).or_insert_with(|| {
            (
                String::new(),
                op,
                delta.old_file().path().map(|p| p.to_path_buf()),
            )
        });
        append_diff_line(&mut entry.0, line);
        true
    })?;

    let mut out: Vec<(String, ArtifactChange)> = Vec::new();
    for (path, (raw_diff, op, old_path)) in by_path {
        let file_str = path.to_string_lossy().to_string();
        let mut extra = HashMap::new();
        extra.insert("operation".into(), json!(op));
        if op == "rename"
            && let Some(old) = &old_path
        {
            extra.insert("from".into(), json!(old.to_string_lossy()));
        }
        out.push((
            file_str,
            ArtifactChange {
                raw: if raw_diff.is_empty() {
                    None
                } else {
                    Some(raw_diff)
                },
                structural: Some(StructuralChange {
                    change_type: format!("opencode.{}", op),
                    extra,
                }),
            },
        ));
    }
    // Stable ordering for reproducibility.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn classify_delta(delta: &git2::DiffDelta) -> &'static str {
    use git2::Delta;
    match delta.status() {
        Delta::Added => "add",
        Delta::Deleted => "delete",
        Delta::Modified => "update",
        Delta::Renamed => "rename",
        Delta::Copied => "copy",
        Delta::Typechange => "update",
        _ => "update",
    }
}

fn append_diff_line(buf: &mut String, line: git2::DiffLine<'_>) {
    use git2::DiffLineType;
    let prefix = match line.origin_value() {
        DiffLineType::Context => " ",
        DiffLineType::Addition => "+",
        DiffLineType::Deletion => "-",
        DiffLineType::ContextEOFNL | DiffLineType::AddEOFNL | DiffLineType::DeleteEOFNL => "",
        _ => "",
    };
    buf.push_str(prefix);
    if let Ok(s) = std::str::from_utf8(line.content()) {
        buf.push_str(s);
    }
}

// Keep tool_category reachable — the match in provider.rs is what
// populates categories, but consumers importing `derive` only may
// want the classifier too.
#[allow(dead_code)]
fn _use_tool_category(name: &str) -> Option<toolpath_convo::ToolCategory> {
    tool_category(name)
}

#[allow(dead_code)]
fn _use_stdpath(_: &StdPath) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpencodeConvo;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;
    use toolpath::v1::Graph;

    fn fixture(body_sql: &str) -> (TempDir, OpencodeConvo, PathResolver) {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&data).unwrap();
        let conn = Connection::open(data.join("opencode.db")).unwrap();
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE project (id text PRIMARY KEY, worktree text NOT NULL, vcs text, name text,
                icon_url text, icon_color text, time_created integer NOT NULL, time_updated integer NOT NULL,
                time_initialized integer, sandboxes text NOT NULL, commands text);
            CREATE TABLE session (id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
                slug text NOT NULL, directory text NOT NULL, title text NOT NULL, version text NOT NULL,
                share_url text, summary_additions integer, summary_deletions integer, summary_files integer,
                summary_diffs text, revert text, permission text,
                time_created integer NOT NULL, time_updated integer NOT NULL,
                time_compacting integer, time_archived integer, workspace_id text);
            CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
            CREATE TABLE part (id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL,
                time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL);
            {body_sql}
        "#
        ))
        .unwrap();
        drop(conn);
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_data_dir(&data);
        (
            temp,
            OpencodeConvo::with_resolver(resolver.clone()),
            resolver,
        )
    }

    const BASIC_SQL: &str = r#"
        INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
          VALUES ('proj_sha', '/tmp/proj', 1000, 3000, '[]');
        INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
          VALUES ('ses_abc123', 'proj_sha', 'slug', '/tmp/proj', 'Build pickle', '1.3.10', 1000, 3000);
        INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES
          ('m1','ses_abc123',1001,1001,
           '{"role":"user","time":{"created":1001},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}'),
          ('m2','ses_abc123',1002,1100,
           '{"parentID":"m1","role":"assistant","mode":"build","agent":"build","path":{"cwd":"/tmp/proj","root":"/tmp/proj"},"cost":0.01,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"modelID":"claude-sonnet-4-6","providerID":"anthropic","time":{"created":1002,"completed":1100},"finish":"stop"}');
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
        // 2 messages → 2 steps.
        assert_eq!(p.steps.len(), 2);
        // Head matches last step.
        assert_eq!(p.path.head, p.steps.last().unwrap().step.id);
    }

    #[test]
    fn derive_validates() {
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

    #[test]
    fn derive_actors_populated() {
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
        let actors = p.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:claude-sonnet-4-6"));
    }

    #[test]
    fn derive_fallback_file_artifact_from_tool() {
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let s = mgr.read_session("ses_abc123").unwrap();
        // With no_snapshot_diffs, derive uses the tool-input fallback
        // to record which files were touched.
        let p = derive_path_with_resolver(
            &s,
            &DeriveConfig {
                no_snapshot_diffs: true,
                ..Default::default()
            },
            &resolver,
        );
        let file_step = p
            .steps
            .iter()
            .find(|s| s.change.contains_key("/tmp/proj/main.cpp"))
            .expect("file artifact missing");
        let change = &file_step.change["/tmp/proj/main.cpp"];
        assert!(
            change.raw.is_none(),
            "no snapshot repo → no raw perspective"
        );
        assert_eq!(
            change.structural.as_ref().unwrap().change_type,
            "opencode.add"
        );
    }

    #[test]
    fn derive_uses_snapshot_git_when_available() {
        // Build a real snapshot git repo on disk with two trees (before
        // and after) and check that derive populates the raw unified diff.
        let (_t, mgr, resolver) = fixture(BASIC_SQL);
        let session = mgr.read_session("ses_abc123").unwrap();

        let gitdir = resolver
            .snapshot_gitdir(&session.project_id, &session.directory)
            .unwrap();
        fs::create_dir_all(&gitdir).unwrap();
        let repo = git2::Repository::init_bare(&gitdir).unwrap();

        // Build "before" tree with only a README.
        let before_tree = {
            let mut tb = repo.treebuilder(None).unwrap();
            let blob = repo.blob(b"hello\n").unwrap();
            tb.insert("README", blob, 0o100644).unwrap();
            tb.write().unwrap()
        };
        // Build "after" tree with README + main.cpp.
        let after_tree = {
            let mut tb = repo.treebuilder(None).unwrap();
            let readme = repo.blob(b"hello\n").unwrap();
            tb.insert("README", readme, 0o100644).unwrap();
            let main = repo.blob(b"int main(){ return 0; }\n").unwrap();
            tb.insert("main.cpp", main, 0o100644).unwrap();
            tb.write().unwrap()
        };

        // Rewrite the session's snapshot SHAs in the DB to point at
        // these real trees. Easier: point snap_a/snap_b at them by
        // writing refs.
        repo.reference("refs/snapshots/snap_a", before_tree, true, "before")
            .unwrap();
        repo.reference("refs/snapshots/snap_b", after_tree, true, "after")
            .unwrap();

        // Edit the SQLite to replace snap_a/snap_b part data with
        // strings that git2's revparse can resolve directly. Use the
        // raw tree SHA hex strings.
        let conn = rusqlite::Connection::open(resolver.db_path().unwrap()).unwrap();
        conn.execute(
            "UPDATE part SET data = REPLACE(data, 'snap_a', ?1) WHERE id = 'p2'",
            rusqlite::params![before_tree.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE part SET data = REPLACE(data, 'snap_b', ?1) WHERE id = 'p5'",
            rusqlite::params![after_tree.to_string()],
        )
        .unwrap();
        drop(conn);

        let session = mgr.read_session("ses_abc123").unwrap();
        let p = derive_path_with_resolver(&session, &DeriveConfig::default(), &resolver);

        let file_step = p
            .steps
            .iter()
            .find(|s| s.change.contains_key("main.cpp"))
            .expect("main.cpp artifact missing");
        let change = &file_step.change["main.cpp"];
        assert!(
            change.raw.is_some(),
            "raw unified diff should be populated from the snapshot repo"
        );
        assert!(
            change
                .raw
                .as_ref()
                .unwrap()
                .contains("+int main(){ return 0; }"),
            "diff must include the new content"
        );
        assert_eq!(
            change.structural.as_ref().unwrap().change_type,
            "opencode.add"
        );
    }
}
