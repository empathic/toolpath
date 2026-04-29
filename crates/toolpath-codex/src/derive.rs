//! Derive Toolpath documents from Codex CLI sessions.
//!
//! Each `Turn` in the assembled `ConversationView` becomes a `Step`.
//! Every step's `change` map carries:
//!
//! - One entry at `codex://<session-id>` with a `conversation.append`
//!   structural op holding the turn's text and tool-call summaries.
//! - Sibling entries for each file touched by a `patch_apply_end`
//!   whose `call_id` landed in this turn. Codex's structured patch
//!   output gives us the unified diff verbatim for updates, and the
//!   full file content for adds — both are surfaced as
//!   `ArtifactChange.raw` so nothing is lost.

use crate::provider::to_view;
use crate::types::{PatchChange, Session};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Identity, Path, PathIdentity, PathMeta, Step,
    StepIdentity, StructuralChange,
};
use toolpath_convo::{ConversationView, Role, Turn};

/// Configuration for deriving a Toolpath Path from a Codex session.
///
/// Note: there's no `include_thinking` toggle like the other providers
/// have. Codex's reasoning is almost always encrypted ciphertext from
/// OpenAI's servers — not useful in a human-readable digest. Plaintext
/// reasoning summaries (rare) land on `Turn.thinking` automatically
/// and surface in the derived path without a flag. The raw ciphertext
/// is preserved under `Turn.extra["codex"]["reasoning_encrypted"]` for
/// round-trip fidelity but never rendered.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override `path.base.uri`. Defaults to the cwd from session_meta.
    pub project_path: Option<String>,
}

/// Derive a [`Path`] from a Codex [`Session`].
pub fn derive_path(session: &Session, config: &DeriveConfig) -> Path {
    let view = to_view(session);
    derive_path_from_view(session, &view, config)
}

/// Derive a [`Path`] from multiple sessions. Used for bulk exports.
pub fn derive_project(sessions: &[Session], config: &DeriveConfig) -> Vec<Path> {
    sessions.iter().map(|s| derive_path(s, config)).collect()
}

/// Internal: build the Path from a pre-built `ConversationView` plus
/// the source `Session` (for session_meta fields like `git`).
fn derive_path_from_view(
    session: &Session,
    view: &ConversationView,
    config: &DeriveConfig,
) -> Path {
    let meta = session.meta();
    let session_short: String = session.id.chars().take(8).collect();
    let path_id = format!("path-codex-{}", session_short);
    let convo_artifact = format!("codex://{}", session.id);

    let mut steps: Vec<Step> = Vec::with_capacity(view.turns.len());
    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();
    let mut last_step_id: Option<String> = None;

    for (turn_idx, turn) in view.turns.iter().enumerate() {
        let Some(step) = build_step(
            turn_idx,
            turn,
            &convo_artifact,
            last_step_id.as_deref(),
            &mut actors,
        ) else {
            continue;
        };
        last_step_id = Some(step.step.id.clone());
        steps.push(step);
    }

    let head = last_step_id.unwrap_or_else(|| "empty".to_string());

    // Base: CLI-override wins; otherwise session_meta.cwd; fall back to
    // the first turn's environment.working_dir.
    let base_uri = config
        .project_path
        .clone()
        .or_else(|| meta.as_ref().map(|m| m.cwd.to_string_lossy().to_string()))
        .or_else(|| {
            view.turns
                .first()
                .and_then(|t| t.environment.as_ref()?.working_dir.clone())
        })
        .map(|p| {
            if p.starts_with('/') {
                format!("file://{}", p)
            } else {
                p
            }
        });

    // Base ref: git commit if session_meta carries one.
    let base_ref = meta
        .as_ref()
        .and_then(|m| m.git.as_ref().and_then(|g| g.commit_hash.clone()));
    let base_branch = meta
        .as_ref()
        .and_then(|m| m.git.as_ref().and_then(|g| g.branch.clone()));

    let base = base_uri.map(|uri| Base {
        uri,
        ref_str: base_ref,
        branch: base_branch,
    });

    // Top-level path meta: actors, title, source, and a Codex extras
    // bucket with the session-level metadata so every consumer sees
    // it (git origin, cli version, model provider).
    let mut path_extra: HashMap<String, Value> = HashMap::new();
    let mut codex_meta: Map<String, Value> = Map::new();
    if let Some(m) = meta.as_ref() {
        codex_meta.insert("session_id".into(), Value::String(session.id.clone()));
        codex_meta.insert("originator".into(), Value::String(m.originator.clone()));
        codex_meta.insert("cli_version".into(), Value::String(m.cli_version.clone()));
        codex_meta.insert("source".into(), Value::String(m.source.clone()));
        if let Some(model_provider) = &m.model_provider {
            codex_meta.insert(
                "model_provider".into(),
                Value::String(model_provider.clone()),
            );
        }
        if let Some(forked) = &m.forked_from_id {
            codex_meta.insert("forked_from_id".into(), Value::String(forked.clone()));
        }
        if let Some(git) = &m.git {
            let mut g: Map<String, Value> = Map::new();
            if let Some(v) = &git.commit_hash {
                g.insert("commit_hash".into(), Value::String(v.clone()));
            }
            if let Some(v) = &git.branch {
                g.insert("branch".into(), Value::String(v.clone()));
            }
            if let Some(v) = &git.repository_url {
                g.insert("repository_url".into(), Value::String(v.clone()));
            }
            if !g.is_empty() {
                codex_meta.insert("git".into(), Value::Object(g));
            }
        }
    }
    if !view.files_changed.is_empty() {
        codex_meta.insert(
            "files_changed".into(),
            Value::Array(
                view.files_changed
                    .iter()
                    .map(|p| Value::String(p.clone()))
                    .collect(),
            ),
        );
    }
    if !codex_meta.is_empty() {
        path_extra.insert("codex".into(), Value::Object(codex_meta));
    }

    Path {
        path: PathIdentity {
            id: path_id,
            base,
            head,
            graph_ref: None,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!("Codex session: {}", session_short)),
            source: Some("codex".to_string()),
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

fn build_step(
    turn_idx: usize,
    turn: &Turn,
    convo_artifact: &str,
    parent_id: Option<&str>,
    actors: &mut HashMap<String, ActorDefinition>,
) -> Option<Step> {
    // Skip empty carrier turns (all-tool, no content) UNLESS they have
    // tool invocations with captured data, in which case we keep them.
    if turn.text.is_empty()
        && turn.tool_uses.is_empty()
        && turn.thinking.is_none()
        && extract_patch_changes(turn).is_empty()
    {
        return None;
    }

    let (actor, role_str) = resolve_actor(turn, actors);

    // Build conversation.append structural
    let mut convo_extra: HashMap<String, Value> = HashMap::new();
    convo_extra.insert("role".into(), json!(role_str));
    if !turn.text.is_empty() {
        convo_extra.insert("text".into(), json!(turn.text));
    }
    // Plaintext reasoning summaries land here automatically; encrypted
    // ciphertext never does (lives under turn.extra["codex"]).
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
                    "status": tool_call_status(turn, &tu.id),
                })
            })
            .collect();
        convo_extra.insert("tool_calls".into(), Value::Array(calls));
    }
    if let Some(u) = turn.token_usage.as_ref() {
        convo_extra.insert("token_usage".into(), json!(u));
    }
    if let Some(ph) = turn
        .extra
        .get("codex")
        .and_then(|c| c.get("phase"))
        .and_then(|v| v.as_str())
    {
        convo_extra.insert("phase".into(), json!(ph));
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

    // File changes from patch_apply_end attached to this turn.
    for (path, patch) in extract_patch_changes(turn) {
        changes.insert(path, patch);
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
            let (actor_key, model_str) = match &turn.model {
                Some(m) if !m.is_empty() => (format!("agent:{}", m), m.clone()),
                _ => ("agent:codex".to_string(), "codex".to_string()),
            };
            actors
                .entry(actor_key.clone())
                .or_insert_with(|| ActorDefinition {
                    name: Some("Codex CLI".to_string()),
                    provider: Some("openai".to_string()),
                    model: Some(model_str.clone()),
                    identities: vec![Identity {
                        system: "openai".to_string(),
                        id: model_str,
                    }],
                    ..Default::default()
                });
            (actor_key, "assistant")
        }
        Role::System => {
            actors
                .entry("system:codex".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("Codex CLI system".to_string()),
                    provider: Some("openai".to_string()),
                    ..Default::default()
                });
            ("system:codex".to_string(), "developer")
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

fn tool_call_status(turn: &Turn, call_id: &str) -> String {
    turn.extra
        .get("codex")
        .and_then(|c| c.get("tool_extras"))
        .and_then(|t| t.get(call_id))
        .and_then(|te| te.get("status").or_else(|| te.get("exit_code")))
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| {
            turn.tool_uses
                .iter()
                .find(|tu| tu.id == call_id)
                .and_then(|tu| tu.result.as_ref())
                .map(|r| {
                    if r.is_error {
                        "error".to_string()
                    } else {
                        "success".to_string()
                    }
                })
                .unwrap_or_default()
        })
}

/// Compact human-readable summary of a tool invocation's salient args.
fn tool_call_summary(tu: &toolpath_convo::ToolInvocation) -> String {
    let pick = |k: &str| -> Option<String> {
        tu.input.get(k).and_then(|v| v.as_str()).map(str::to_string)
    };
    let summary = match tu.name.as_str() {
        "exec_command" | "shell" | "unified_exec" => pick("cmd").or_else(|| pick("command")),
        "write_stdin" => pick("chars").or_else(|| pick("session_id")),
        "read_file" | "read_many_files" | "list_dir" | "view_image" => pick("path"),
        "write_file" | "replace" | "edit" => pick("file_path"),
        "apply_patch" => {
            // input is a raw patch string; surface the first change line
            tu.input.as_str().and_then(|s| {
                s.lines()
                    .find(|l| {
                        l.starts_with("*** Add File:")
                            || l.starts_with("*** Update File:")
                            || l.starts_with("*** Delete File:")
                    })
                    .map(str::to_string)
            })
        }
        "glob" | "grep_search" | "search_file_content" => pick("pattern").or_else(|| pick("query")),
        "web_fetch" => pick("url"),
        "web_search" | "google_web_search" => pick("query"),
        "spawn_agent" | "task" | "activate_skill" => pick("prompt").or_else(|| pick("task")),
        _ => None,
    };
    summary.unwrap_or_default()
}

/// Pull `patch_apply_end.changes` off a turn's extras and turn each into
/// a toolpath `ArtifactChange` with both perspectives populated.
fn extract_patch_changes(turn: &Turn) -> Vec<(String, ArtifactChange)> {
    let Some(codex) = turn.extra.get("codex") else {
        return Vec::new();
    };
    let Some(Value::Array(patches)) = codex.get("patch_changes") else {
        return Vec::new();
    };

    let mut out: Vec<(String, ArtifactChange)> = Vec::new();
    for patch in patches {
        let Some(Value::Object(changes)) = patch.get("changes") else {
            continue;
        };
        for (path, change_val) in changes {
            let Some(change) = parse_patch_change(change_val) else {
                continue;
            };
            let (raw, structural) = patch_change_to_perspectives(&change, path);
            out.push((
                path.clone(),
                ArtifactChange {
                    raw,
                    structural: Some(structural),
                },
            ));
        }
    }
    out
}

fn parse_patch_change(v: &Value) -> Option<PatchChange> {
    serde_json::from_value::<PatchChange>(v.clone()).ok()
}

fn patch_change_to_perspectives(
    change: &PatchChange,
    file_path: &str,
) -> (Option<String>, StructuralChange) {
    let mut extra: HashMap<String, Value> = HashMap::new();
    match change {
        PatchChange::Add { content, .. } => {
            extra.insert("operation".into(), json!("add"));
            extra.insert("byte_count".into(), json!(content.len()));
            extra.insert("line_count".into(), json!(content.lines().count()));
            let raw = synth_add_diff(file_path, content);
            (
                Some(raw),
                StructuralChange {
                    change_type: "codex.add".into(),
                    extra,
                },
            )
        }
        PatchChange::Update {
            unified_diff,
            move_path,
            ..
        } => {
            extra.insert("operation".into(), json!("update"));
            if let Some(mp) = move_path {
                extra.insert("move_path".into(), json!(mp));
            }
            (
                Some(unified_diff.clone()),
                StructuralChange {
                    change_type: "codex.update".into(),
                    extra,
                },
            )
        }
        PatchChange::Delete {
            original_content, ..
        } => {
            extra.insert("operation".into(), json!("delete"));
            let raw = original_content
                .as_ref()
                .map(|c| synth_delete_diff(file_path, c));
            (
                raw,
                StructuralChange {
                    change_type: "codex.delete".into(),
                    extra,
                },
            )
        }
        PatchChange::Unknown => {
            extra.insert("operation".into(), json!("unknown"));
            (
                None,
                StructuralChange {
                    change_type: "codex.unknown".into(),
                    extra,
                },
            )
        }
    }
}

fn synth_add_diff(_path: &str, content: &str) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    // Git-style add: all lines prefixed with +.
    // Strip trailing empty element from the trailing newline, if any.
    let effective: &[&str] = if lines.last() == Some(&"") {
        &lines[..lines.len().saturating_sub(1)]
    } else {
        &lines[..]
    };
    let mut buf = format!("@@ -0,0 +1,{} @@\n", effective.len());
    for l in effective {
        buf.push('+');
        buf.push_str(l);
        buf.push('\n');
    }
    buf
}

fn synth_delete_diff(_path: &str, original: &str) -> String {
    let lines: Vec<&str> = original.split('\n').collect();
    let effective: &[&str] = if lines.last() == Some(&"") {
        &lines[..lines.len().saturating_sub(1)]
    } else {
        &lines[..]
    };
    let mut buf = format!("@@ -1,{} +0,0 @@\n", effective.len());
    for l in effective {
        buf.push('-');
        buf.push_str(l);
        buf.push('\n');
    }
    buf
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
            r#"{"timestamp":"2026-04-20T16:44:38.200Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}","call_id":"c1"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.300Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"/tmp/proj\n"}}"#,
            r#"{"timestamp":"2026-04-20T16:44:38.400Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"c1","command":["/bin/bash","-lc","pwd"],"stdout":"/tmp/proj\n","exit_code":0,"aggregated_output":"/tmp/proj\n"}}"#,
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
        // 3 turns → 3 steps (user, assistant 1, assistant 2).
        assert_eq!(path.steps.len(), 3);
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
    fn derive_path_preserves_conversation_artifact() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let artifact = format!("codex://{}", session.id);
        for step in &path.steps {
            assert!(
                step.change.contains_key(&artifact),
                "step {} missing convo artifact",
                step.step.id
            );
        }
    }

    #[test]
    fn derive_path_surfaces_apply_patch_as_file_artifact() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        // Find the step with the file artifact.
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
        assert_eq!(structural.change_type, "codex.add");
        assert_eq!(structural.extra["operation"], "add");
    }

    #[test]
    fn derive_path_update_perspectives_preserved() {
        // Session with an `update` change carrying a real unified_diff.
        let body = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"s","timestamp":"t","cwd":"/p","originator":"x","cli_version":"1","source":"cli"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"edit"}]}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c","name":"apply_patch","input":"*** Update File: /p/a.rs\n@@"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"c","success":true,"changes":{"/p/a.rs":{"type":"update","unified_diff":"@@ -1 +1 @@\n-old\n+new"}}}}"#,
        ].join("\n");
        let (_t, mgr, id) = fixture_session(&body);
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let file_change = path
            .steps
            .iter()
            .find_map(|s| s.change.get("/p/a.rs"))
            .expect("update should land as file artifact");
        assert_eq!(file_change.raw.as_deref(), Some("@@ -1 +1 @@\n-old\n+new"));
        let structural = file_change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "codex.update");
    }

    #[test]
    fn derive_path_validates() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let doc = Graph::from_path(path);
        let json = doc.to_json().unwrap();
        // Round-trip through the validator (it just needs to parse).
        let parsed = Graph::from_json(&json).unwrap();
        let p = parsed.single_path().expect("single-path graph");
        assert!(!p.steps.is_empty());
        let ancestors = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
        assert_eq!(ancestors.len(), p.steps.len(), "all steps on head ancestry");
    }

    #[test]
    fn derive_path_shell_summary() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let convo_artifact = format!("codex://{}", session.id);
        // Find the step that had exec_command
        let step = path
            .steps
            .iter()
            .find(|s| {
                s.change
                    .get(&convo_artifact)
                    .and_then(|c| c.structural.as_ref())
                    .and_then(|sc| sc.extra.get("tool_calls"))
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().any(|v| v["name"] == "exec_command"))
                    .unwrap_or(false)
            })
            .expect("no step with exec_command");
        let calls = step.change[&convo_artifact]
            .structural
            .as_ref()
            .unwrap()
            .extra["tool_calls"]
            .as_array()
            .unwrap();
        let exec = &calls[0];
        assert_eq!(exec["summary"], "pwd");
    }

    #[test]
    fn derive_path_meta_carries_git() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let path = derive_path(&session, &DeriveConfig::default());
        let codex_meta = &path.meta.as_ref().unwrap().extra["codex"];
        let git = &codex_meta["git"];
        assert_eq!(git["commit_hash"], "abc");
        assert_eq!(git["branch"], "main");
    }

    #[test]
    fn derive_project_multi() {
        let (_t, mgr, id) = fixture_session(&minimal_body());
        let session = mgr.read_session(&id).unwrap();
        let paths = derive_project(&[session.clone(), session], &DeriveConfig::default());
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].path.id, paths[1].path.id);
    }

    #[test]
    fn synth_add_diff_has_plus_lines() {
        let diff = synth_add_diff("a.rs", "hello\nworld\n");
        assert!(diff.contains("+hello"));
        assert!(diff.contains("+world"));
        assert!(diff.starts_with("@@ -0,0 +1,2 @@"));
    }

    #[test]
    fn synth_delete_diff_has_minus_lines() {
        let diff = synth_delete_diff("a.rs", "gone\n");
        assert!(diff.contains("-gone"));
        assert!(diff.starts_with("@@ -1,1 +0,0 @@"));
    }
}
