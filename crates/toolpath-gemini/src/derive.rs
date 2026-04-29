//! Derive Toolpath documents from Gemini CLI conversation logs.
//!
//! The conversation is modeled as an artifact at
//! `gemini://<session-id>`. Each turn appends to that artifact via a
//! `conversation.append` structural change. File mutations from
//! `write_file` and `replace` tool calls appear as sibling artifacts in
//! the same step's `change` map.
//!
//! Sub-agent chats are linearized into the path as additional steps
//! parented to the main assistant step whose `task` tool invocation
//! spawned them (document order, matching [`crate::provider`]).

use crate::provider::{file_path_from_args, tool_category};
use crate::types::{ChatFile, Conversation, GeminiMessage, GeminiRole, ToolCall};
use serde_json::json;
use std::collections::HashMap;
use toolpath::v1::{
    ActorDefinition, ArtifactChange, Base, Identity, Path, PathIdentity, PathMeta, Step,
    StepIdentity, StructuralChange,
};
use toolpath_convo::ToolCategory;

/// Configuration for deriving Toolpath documents from Gemini conversations.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Override the project path used for `path.base.uri`.
    pub project_path: Option<String>,
    /// Include thinking blocks in the `conversation.append` text payload.
    pub include_thinking: bool,
}

/// Derive a single Toolpath [`Path`] from a Gemini conversation.
pub fn derive_path(conversation: &Conversation, config: &DeriveConfig) -> Path {
    let session_short = safe_prefix(&conversation.main.session_id, 8);
    let path_id = if session_short.is_empty() {
        format!("path-gemini-{}", safe_prefix(&conversation.session_uuid, 8))
    } else {
        format!("path-gemini-{}", session_short)
    };
    let convo_artifact = convo_artifact_uri(&conversation.main);

    let mut actors: HashMap<String, ActorDefinition> = HashMap::new();
    let mut steps: Vec<Step> = Vec::new();

    // Index sub-agents deterministically by start_time so we attach them
    // in the same order as the provider.
    let mut sub_order: Vec<&ChatFile> = conversation.sub_agents.iter().collect();
    sub_order.sort_by_key(|s| s.start_time);
    let mut sub_iter = sub_order.into_iter();

    let mut last_step_id: Option<String> = None;

    for msg in &conversation.main.messages {
        let Some(step) = build_step(
            msg,
            &convo_artifact,
            last_step_id.as_deref(),
            &mut actors,
            config,
        ) else {
            continue;
        };
        let step_id = step.step.id.clone();
        steps.push(step);

        // For each delegation-category tool call, pull the next sub-agent
        // off the queue and append its messages as steps parented under
        // this main step.
        let delegation_calls: Vec<&ToolCall> = msg
            .tool_calls()
            .iter()
            .filter(|t| tool_category(&t.name) == Some(ToolCategory::Delegation))
            .collect();
        for _ in &delegation_calls {
            if let Some(sub) = sub_iter.next() {
                append_sub_agent_steps(sub, &step_id, &mut steps, &mut actors, config);
            }
        }

        last_step_id = Some(step_id);
    }

    // Leftover sub-agents attach to the last step we emitted.
    let leftover: Vec<&ChatFile> = sub_iter.collect();
    if !leftover.is_empty()
        && let Some(parent) = last_step_id.clone()
    {
        for sub in leftover {
            append_sub_agent_steps(sub, &parent, &mut steps, &mut actors, config);
        }
    }

    let head = last_step_id.unwrap_or_else(|| "empty".to_string());

    let base_uri = config
        .project_path
        .clone()
        .or_else(|| conversation.project_path.clone())
        .or_else(|| {
            conversation
                .main
                .directories()
                .first()
                .map(|p| p.to_string_lossy().to_string())
        })
        .map(|p| format!("file://{}", p));

    Path {
        path: PathIdentity {
            id: path_id,
            base: base_uri.map(|uri| Base {
                uri,
                ref_str: None,
                branch: None,
            }),
            head,
            graph_ref: None,
        },
        steps,
        meta: Some(PathMeta {
            title: Some(format!(
                "Gemini session: {}",
                if session_short.is_empty() {
                    safe_prefix(&conversation.session_uuid, 8)
                } else {
                    session_short
                }
            )),
            source: Some("gemini-cli".to_string()),
            actors: if actors.is_empty() {
                None
            } else {
                Some(actors)
            },
            ..Default::default()
        }),
    }
}

/// Derive Toolpath Paths from multiple conversations.
pub fn derive_project(conversations: &[Conversation], config: &DeriveConfig) -> Vec<Path> {
    conversations
        .iter()
        .map(|c| derive_path(c, config))
        .collect()
}

// ── Step construction ────────────────────────────────────────────────

fn build_step(
    msg: &GeminiMessage,
    convo_artifact: &str,
    parent_id: Option<&str>,
    actors: &mut HashMap<String, ActorDefinition>,
    config: &DeriveConfig,
) -> Option<Step> {
    if msg.id.is_empty() {
        return None;
    }

    let (actor, role_str) = resolve_actor(msg, actors);

    let mut file_changes: HashMap<String, ArtifactChange> = HashMap::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls_meta: Vec<serde_json::Value> = Vec::new();

    let content_text = msg.content.text();
    if !content_text.trim().is_empty() {
        text_parts.push(content_text);
    }
    if config.include_thinking && !msg.thoughts().is_empty() {
        for t in msg.thoughts() {
            let subject = t.subject.as_deref().unwrap_or("");
            let description = t.description.as_deref().unwrap_or("");
            let combined = match (subject.is_empty(), description.is_empty()) {
                (false, false) => format!("[thinking: {}] {}", subject, description),
                (false, true) => format!("[thinking] {}", subject),
                (true, false) => format!("[thinking] {}", description),
                (true, true) => continue,
            };
            text_parts.push(combined);
        }
    }

    for call in msg.tool_calls() {
        tool_calls_meta.push(serde_json::json!({
            "name": call.name,
            "status": call.status,
            "summary": tool_call_summary(call),
        }));
        if matches!(tool_category(&call.name), Some(ToolCategory::FileWrite))
            && let Some(fp) = file_path_from_args(&call.args)
        {
            let new_change = build_file_write_change(call);
            // If the same file is touched twice by one message (rare but
            // possible), prefer the first; downstream steps show the
            // later edit distinctly.
            file_changes.entry(fp).or_insert(new_change);
        }
    }

    if text_parts.is_empty() && tool_calls_meta.is_empty() && file_changes.is_empty() {
        return None;
    }

    let mut convo_extra = HashMap::new();
    convo_extra.insert("role".to_string(), json!(role_str));
    if !text_parts.is_empty() {
        let combined = text_parts.join("\n\n");
        convo_extra.insert("text".to_string(), json!(combined));
    }
    if !tool_calls_meta.is_empty() {
        convo_extra.insert("tool_calls".to_string(), json!(tool_calls_meta));
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
    changes.extend(file_changes);

    let step_id = format!("step-{}", safe_prefix(&msg.id, 8));
    let parents = parent_id.map(|p| vec![p.to_string()]).unwrap_or_default();

    Some(Step {
        step: StepIdentity {
            id: step_id,
            parents,
            actor,
            timestamp: msg.timestamp.clone(),
        },
        change: changes,
        meta: None,
    })
}

/// Build an `ArtifactChange` for a single file-write tool invocation.
///
/// Always populates at least one perspective (per RFC §"Change
/// Perspectives"): `raw` is preferred when Gemini's `resultDisplay`
/// carries a `fileDiff`; otherwise we fall back to a hand-rolled
/// unified-diff hunk for `replace`, or a "new file" hunk for
/// `write_file`. `structural` mirrors the tool name and captures the
/// raw args (trimmed) so downstream consumers have machine-readable
/// detail.
fn build_file_write_change(call: &ToolCall) -> ArtifactChange {
    let raw = call.file_diff().or_else(|| fallback_raw_diff(call));
    let structural = Some(StructuralChange {
        change_type: format!("gemini.{}", call.name),
        extra: structural_extra_for(call),
    });
    ArtifactChange { raw, structural }
}

/// Compact human-readable summary of a tool call's salient args. Used
/// in `conversation.append` structural payloads so shell commands,
/// grep patterns, read targets, etc. aren't dropped during derivation.
fn tool_call_summary(call: &ToolCall) -> String {
    let pick = |k: &str| -> Option<&str> { call.args.get(k).and_then(|v| v.as_str()) };
    let summary = match call.name.as_str() {
        "run_shell_command" => pick("command").map(str::to_string),
        "read_file" | "read_many_files" | "list_directory" => pick("file_path")
            .or_else(|| pick("path"))
            .map(str::to_string),
        "write_file" | "replace" | "edit" => pick("file_path").map(str::to_string),
        "glob" => pick("pattern").map(str::to_string),
        "grep_search" | "search_file_content" => pick("pattern").map(str::to_string),
        "web_fetch" => pick("url").map(str::to_string),
        "google_web_search" => pick("query").map(str::to_string),
        "task" | "activate_skill" => pick("prompt").map(str::to_string),
        "get_internal_docs" => pick("path").map(str::to_string),
        _ => None,
    };
    summary.unwrap_or_default()
}

fn structural_extra_for(call: &ToolCall) -> HashMap<String, serde_json::Value> {
    let mut extra = HashMap::new();
    match call.name.as_str() {
        "write_file" => {
            let content = call
                .args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            extra.insert("operation".into(), json!("write"));
            extra.insert("byte_count".into(), json!(content.len()));
            extra.insert("line_count".into(), json!(content.lines().count()));
        }
        "replace" => {
            let old_s = call
                .args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_s = call
                .args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let instruction = call
                .args
                .get("instruction")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            extra.insert("operation".into(), json!("replace"));
            extra.insert("old_string".into(), json!(old_s));
            extra.insert("new_string".into(), json!(new_s));
            if !instruction.is_empty() {
                extra.insert("instruction".into(), json!(instruction));
            }
        }
        "edit" => {
            extra.insert("operation".into(), json!("edit"));
        }
        _ => {
            extra.insert("operation".into(), json!(call.name.clone()));
        }
    }
    extra.insert("status".into(), json!(call.status));
    extra
}

/// Construct a unified-diff hunk when Gemini's `resultDisplay.fileDiff`
/// is absent. Not pixel-perfect but good enough to give readers a
/// change perspective.
fn fallback_raw_diff(call: &ToolCall) -> Option<String> {
    match call.name.as_str() {
        "replace" => {
            let old_s = call.args.get("old_string").and_then(|v| v.as_str())?;
            let new_s = call.args.get("new_string").and_then(|v| v.as_str())?;
            let old_lines: Vec<&str> = old_s.split('\n').collect();
            let new_lines: Vec<&str> = new_s.split('\n').collect();
            let mut buf = format!("@@ -1,{} +1,{} @@\n", old_lines.len(), new_lines.len());
            for l in old_lines {
                buf.push('-');
                buf.push_str(l);
                buf.push('\n');
            }
            for l in new_lines {
                buf.push('+');
                buf.push_str(l);
                buf.push('\n');
            }
            Some(buf)
        }
        "write_file" => {
            let content = call.args.get("content").and_then(|v| v.as_str())?;
            let lines: Vec<&str> = content.split('\n').collect();
            let mut buf = format!("@@ -0,0 +1,{} @@\n", lines.len());
            for l in lines {
                buf.push('+');
                buf.push_str(l);
                buf.push('\n');
            }
            Some(buf)
        }
        _ => None,
    }
}

/// Append every message in a sub-agent chat as a step parented under
/// `parent_step_id`, linearizing internally.
fn append_sub_agent_steps(
    sub: &ChatFile,
    parent_step_id: &str,
    steps: &mut Vec<Step>,
    actors: &mut HashMap<String, ActorDefinition>,
    config: &DeriveConfig,
) {
    let convo_artifact = convo_artifact_uri(sub);
    let mut local_parent = parent_step_id.to_string();

    for msg in &sub.messages {
        if let Some(mut step) =
            build_step(msg, &convo_artifact, Some(&local_parent), actors, config)
        {
            // Prefix sub-agent step IDs to avoid collisions with main-chat
            // step IDs (which are derived from the message UUID prefix).
            let session_tag = if sub.session_id.is_empty() {
                "sub".to_string()
            } else {
                safe_prefix(&sub.session_id, 6)
            };
            step.step.id = format!("sub-{}-{}", session_tag, safe_prefix(&msg.id, 8));
            step.step.parents = vec![local_parent.clone()];
            local_parent = step.step.id.clone();
            steps.push(step);
        }
    }
}

fn resolve_actor(
    msg: &GeminiMessage,
    actors: &mut HashMap<String, ActorDefinition>,
) -> (String, &'static str) {
    match &msg.role {
        GeminiRole::User => {
            actors
                .entry("human:user".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("User".to_string()),
                    ..Default::default()
                });
            ("human:user".to_string(), "user")
        }
        GeminiRole::Gemini => {
            let (actor_key, model_str) = match &msg.model {
                Some(m) if !m.is_empty() => (format!("agent:{}", m), m.clone()),
                _ => ("agent:gemini-cli".to_string(), "gemini-cli".to_string()),
            };
            actors
                .entry(actor_key.clone())
                .or_insert_with(|| ActorDefinition {
                    name: Some("Gemini CLI".to_string()),
                    provider: Some("google".to_string()),
                    model: Some(model_str.clone()),
                    identities: vec![Identity {
                        system: "google".to_string(),
                        id: model_str,
                    }],
                    ..Default::default()
                });
            (actor_key, "gemini")
        }
        GeminiRole::Info => {
            actors
                .entry("system:gemini-cli".to_string())
                .or_insert_with(|| ActorDefinition {
                    name: Some("Gemini CLI system".to_string()),
                    provider: Some("google".to_string()),
                    ..Default::default()
                });
            ("system:gemini-cli".to_string(), "info")
        }
        GeminiRole::Other(s) => {
            let key = format!("other:{}", s);
            actors
                .entry(key.clone())
                .or_insert_with(|| ActorDefinition {
                    name: Some(s.clone()),
                    ..Default::default()
                });
            // Static string only — unknown roles render as "other" in the
            // conversation.append payload for readability.
            (key, "other")
        }
    }
}

fn convo_artifact_uri(chat: &ChatFile) -> String {
    let sid = if chat.session_id.is_empty() {
        "unknown".to_string()
    } else {
        chat.session_id.clone()
    };
    format!("gemini://{}", sid)
}

fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatFile;
    use serde_json::Value;

    fn parse_chat(s: &str) -> ChatFile {
        serde_json::from_str(s).unwrap()
    }

    fn main_only_convo() -> Conversation {
        let chat = parse_chat(
            r#"{
  "sessionId":"sess1",
  "projectHash":"h",
  "startTime":"2026-04-17T10:00:00Z",
  "lastUpdated":"2026-04-17T10:10:00Z",
  "directories":["/abs/project"],
  "messages":[
    {"id":"user-1111aaaa","timestamp":"2026-04-17T10:00:00Z","type":"user","content":[{"text":"Fix the bug"}]},
    {"id":"ai-2222bbbb","timestamp":"2026-04-17T10:00:01Z","type":"gemini","content":"I'll look.","model":"gemini-3-flash-preview"},
    {"id":"ai-3333cccc","timestamp":"2026-04-17T10:01:00Z","type":"gemini","content":"Writing fix.","model":"gemini-3-flash-preview","toolCalls":[
      {"id":"w1","name":"write_file","args":{"file_path":"/abs/project/src/main.rs","content":"fn main(){}"},"status":"success","timestamp":"2026-04-17T10:01:00Z","result":[{"functionResponse":{"id":"w1","name":"write_file","response":{"output":"ok"}}}]}
    ]}
  ]
}"#,
        );
        let mut convo = Conversation::new("uuid-1".to_string(), chat);
        convo.project_path = Some("/abs/project".to_string());
        convo
    }

    #[test]
    fn test_derive_path_basic() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-gemini-"));
        assert_eq!(path.steps.len(), 3);
        assert_eq!(path.steps[0].step.actor, "human:user");
        assert!(path.steps[1].step.actor.starts_with("agent:"));
    }

    #[test]
    fn test_derive_path_head_is_last_step() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        assert_eq!(path.path.head, path.steps.last().unwrap().step.id);
    }

    #[test]
    fn test_derive_path_parents_chain() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.steps[0].step.parents.is_empty());
        assert_eq!(
            path.steps[1].step.parents,
            vec![path.steps[0].step.id.clone()]
        );
        assert_eq!(
            path.steps[2].step.parents,
            vec![path.steps[1].step.id.clone()]
        );
    }

    #[test]
    fn test_derive_path_conversation_artifact() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let artifact = "gemini://sess1";
        assert!(path.steps[0].change.contains_key(artifact));
        let structural = path.steps[0].change[artifact].structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "conversation.append");
        assert_eq!(structural.extra["role"], "user");
    }

    #[test]
    fn test_derive_path_file_write_artifact() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let write_step = &path.steps[2];
        assert!(write_step.change.contains_key("/abs/project/src/main.rs"));
    }

    #[test]
    fn test_derive_path_actors_populated() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let actors = path.meta.as_ref().unwrap().actors.as_ref().unwrap();
        assert!(actors.contains_key("human:user"));
        assert!(actors.contains_key("agent:gemini-3-flash-preview"));
    }

    #[test]
    fn test_derive_path_base_from_project_path() {
        let convo = main_only_convo();
        let path = derive_path(
            &convo,
            &DeriveConfig {
                project_path: Some("/override".to_string()),
                include_thinking: false,
            },
        );
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///override");
    }

    #[test]
    fn test_derive_path_base_from_directories_fallback() {
        // Scrub project_path from conversation: should fall back to directories[0]
        let mut convo = main_only_convo();
        convo.project_path = None;
        let path = derive_path(&convo, &DeriveConfig::default());
        assert_eq!(path.path.base.as_ref().unwrap().uri, "file:///abs/project");
    }

    #[test]
    fn test_derive_path_no_base_when_unknown() {
        let mut convo = main_only_convo();
        convo.project_path = None;
        convo.main.directories = None;
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.path.base.is_none());
    }

    #[test]
    fn test_derive_path_skips_empty_messages() {
        let chat = parse_chat(
            r#"{
  "sessionId":"x","projectHash":"","messages":[
    {"id":"m1","timestamp":"ts","type":"user","content":""},
    {"id":"m2","timestamp":"ts","type":"user","content":[{"text":"   "}]},
    {"id":"m3","timestamp":"ts","type":"user","content":[{"text":"hello"}]}
  ]
}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.steps[0].step.id, "step-m3");
    }

    #[test]
    fn test_derive_path_falls_back_to_gemini_cli_actor() {
        let chat = parse_chat(
            r#"{
  "sessionId":"x","projectHash":"","messages":[
    {"id":"m1","timestamp":"ts","type":"gemini","content":"hello"}
  ]
}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        assert_eq!(path.steps[0].step.actor, "agent:gemini-cli");
    }

    #[test]
    fn test_derive_path_with_replace_tool() {
        let chat = parse_chat(
            r#"{
  "sessionId":"x","projectHash":"","messages":[
    {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
      {"id":"r","name":"replace","args":{"file_path":"src/a.rs","oldString":"x","newString":"y"},"status":"success","timestamp":"ts"}
    ]}
  ]
}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.steps[0].change.contains_key("src/a.rs"));
    }

    #[test]
    fn test_derive_path_thinking_included_when_enabled() {
        let chat = parse_chat(
            r#"{
  "sessionId":"x","projectHash":"","messages":[
    {"id":"m1","timestamp":"ts","type":"gemini","content":"plan","thoughts":[{"subject":"s","description":"deep thought","timestamp":"ts"}]}
  ]
}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(
            &convo,
            &DeriveConfig {
                project_path: None,
                include_thinking: true,
            },
        );
        let text = path.steps[0].change["gemini://x"]
            .structural
            .as_ref()
            .unwrap()
            .extra["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("deep thought"));
    }

    #[test]
    fn test_derive_path_thinking_omitted_by_default() {
        let chat = parse_chat(
            r#"{
  "sessionId":"x","projectHash":"","messages":[
    {"id":"m1","timestamp":"ts","type":"gemini","content":"plan","thoughts":[{"subject":"s","description":"deep thought","timestamp":"ts"}]}
  ]
}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        let text = path.steps[0].change["gemini://x"]
            .structural
            .as_ref()
            .unwrap()
            .extra["text"]
            .as_str()
            .unwrap();
        assert!(!text.contains("deep thought"));
        assert!(text.contains("plan"));
    }

    #[test]
    fn test_derive_path_sub_agent_steps() {
        // Main chat delegates via `task`; sub-agent messages become extra
        // steps parented under the main step.
        let main_chat = parse_chat(
            r#"{
  "sessionId":"m","projectHash":"","messages":[
    {"id":"u1","timestamp":"ts","type":"user","content":[{"text":"go"}]},
    {"id":"a1","timestamp":"ts","type":"gemini","content":"delegating","model":"gemini-3-flash-preview","toolCalls":[
      {"id":"t","name":"task","args":{"prompt":"search"},"status":"success","timestamp":"ts"}
    ]}
  ]
}"#,
        );
        let sub_chat = parse_chat(
            r#"{
  "sessionId":"subby","projectHash":"","kind":"subagent","summary":"found","startTime":"2026-04-17T10:00:00Z","messages":[
    {"id":"sa","timestamp":"ts","type":"user","content":[{"text":"sub prompt"}]},
    {"id":"sb","timestamp":"ts","type":"gemini","content":"sub response","model":"gemini-3-flash-preview"}
  ]
}"#,
        );
        let mut convo = Conversation::new("uuid".into(), main_chat);
        convo.sub_agents.push(sub_chat);

        let path = derive_path(&convo, &DeriveConfig::default());

        // 2 main steps + 2 sub steps
        assert_eq!(path.steps.len(), 4);
        // Sub steps have IDs starting with "sub-"
        assert!(path.steps[2].step.id.starts_with("sub-"));
        assert!(path.steps[3].step.id.starts_with("sub-"));
        // First sub step is parented under the main assistant step (a1 -> step-a1)
        assert_eq!(path.steps[2].step.parents, vec!["step-a1".to_string()]);
        // Second sub step is parented under the first sub step
        assert_eq!(
            path.steps[3].step.parents,
            vec![path.steps[2].step.id.clone()]
        );
        // Sub-agent artifact URI distinct from main
        assert!(path.steps[2].change.contains_key("gemini://subby"));
        assert!(path.steps[0].change.contains_key("gemini://m"));
    }

    #[test]
    fn test_derive_path_leftover_subagent_attaches_to_last() {
        // No `task` invocation, but a sub-agent file exists.
        let main_chat = parse_chat(
            r#"{
  "sessionId":"m","projectHash":"","messages":[
    {"id":"u1","timestamp":"ts","type":"user","content":[{"text":"go"}]}
  ]
}"#,
        );
        let sub_chat = parse_chat(
            r#"{
  "sessionId":"unlinked","projectHash":"","kind":"subagent","startTime":"2026-04-17T10:00:00Z","messages":[
    {"id":"sx","timestamp":"ts","type":"user","content":[{"text":"something"}]}
  ]
}"#,
        );
        let mut convo = Conversation::new("uuid".into(), main_chat);
        convo.sub_agents.push(sub_chat);

        let path = derive_path(&convo, &DeriveConfig::default());
        // One main + one sub
        assert_eq!(path.steps.len(), 2);
        assert!(path.steps[1].step.id.starts_with("sub-"));
        // Attached to the last main step (step-u1)
        assert_eq!(path.steps[1].step.parents, vec!["step-u1".to_string()]);
    }

    #[test]
    fn test_derive_project_multiple() {
        let a = main_only_convo();
        let b = {
            let mut c = main_only_convo();
            c.main.session_id = "sess2".into();
            c.session_uuid = "uuid-2".into();
            c
        };
        let paths = derive_project(&[a, b], &DeriveConfig::default());
        assert_eq!(paths.len(), 2);
        assert!(paths[0].path.id.contains("sess1"));
        assert!(paths[1].path.id.contains("sess2"));
    }

    #[test]
    fn test_safe_prefix_behaviour() {
        assert_eq!(safe_prefix("abc", 8), "abc");
        assert_eq!(safe_prefix("abcdefghij", 8), "abcdefgh");
        assert_eq!(safe_prefix("日本語", 2), "日本");
    }

    #[test]
    fn test_convo_artifact_uri_unknown_fallback() {
        let chat = parse_chat(r#"{"sessionId":"","projectHash":"","messages":[]}"#);
        assert_eq!(convo_artifact_uri(&chat), "gemini://unknown");
    }

    #[test]
    fn test_path_id_falls_back_to_session_uuid() {
        let chat = parse_chat(
            r#"{"sessionId":"","projectHash":"","messages":[{"id":"m","timestamp":"ts","type":"user","content":[{"text":"hi"}]}]}"#,
        );
        let convo = Conversation::new("long-session-uuid-123".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        assert!(path.path.id.starts_with("path-gemini-"));
        // Should use a prefix of the session UUID when sessionId is empty
        assert!(path.path.id.contains("long-ses"));
    }

    #[test]
    fn test_conversation_artifact_extra_fields() {
        let convo = main_only_convo();
        let path = derive_path(&convo, &DeriveConfig::default());
        let structural = path.steps[2].change["gemini://sess1"]
            .structural
            .as_ref()
            .unwrap();
        assert_eq!(structural.extra["role"], "gemini");
        let calls = structural.extra["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["name"], Value::String("write_file".to_string()));
        assert_eq!(calls[0]["summary"], "/abs/project/src/main.rs");
    }

    #[test]
    fn test_info_message_becomes_system_step() {
        let chat = parse_chat(
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"u1","timestamp":"ts","type":"user","content":[{"text":"hi"}]},
  {"id":"i1","timestamp":"ts","type":"info","content":"Request cancelled."}
]}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        assert_eq!(path.steps.len(), 2);
        assert_eq!(path.steps[1].step.actor, "system:gemini-cli");
    }

    #[test]
    fn test_file_write_change_has_perspectives() {
        // Verify at least one change perspective per RFC §"Change Perspectives"
        let chat = parse_chat(
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"w1","name":"write_file","args":{"file_path":"src/main.rs","content":"fn main() {}\n"},"status":"success","timestamp":"ts"}
  ]}
]}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        let change = &path.steps[0].change["src/main.rs"];
        assert!(
            change.raw.is_some() || change.structural.is_some(),
            "at least one perspective must be populated"
        );
        assert!(change.structural.is_some());
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.change_type, "gemini.write_file");
        assert_eq!(structural.extra["operation"], "write");
        assert_eq!(structural.extra["byte_count"], 13);
        // Fallback raw diff constructed from content
        assert!(change.raw.as_ref().unwrap().contains("+fn main() {}"));
    }

    #[test]
    fn test_replace_change_has_diff() {
        let chat = parse_chat(
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"r1","name":"replace","args":{"file_path":"src/main.rs","old_string":"hello","new_string":"world","instruction":"swap"},"status":"success","timestamp":"ts"}
  ]}
]}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        let change = &path.steps[0].change["src/main.rs"];
        let raw = change.raw.as_ref().unwrap();
        assert!(raw.contains("-hello"));
        assert!(raw.contains("+world"));
        let structural = change.structural.as_ref().unwrap();
        assert_eq!(structural.extra["operation"], "replace");
        assert_eq!(structural.extra["instruction"], "swap");
    }

    #[test]
    fn test_file_diff_preferred_over_fallback() {
        // When Gemini provides resultDisplay.fileDiff, it should be used as
        // the raw perspective verbatim.
        let chat = parse_chat(
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"r1","name":"replace","args":{"file_path":"a.rs","old_string":"x","new_string":"y"},"status":"success","timestamp":"ts","resultDisplay":{"fileDiff":"Index: a.rs\n...GEMINI DIFF..."}}
  ]}
]}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        let raw = path.steps[0].change["a.rs"].raw.as_ref().unwrap();
        assert!(raw.contains("GEMINI DIFF"));
    }

    #[test]
    fn test_tool_call_summary_preserves_shell_command() {
        let chat = parse_chat(
            r#"{"sessionId":"s","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"gemini","content":"building","toolCalls":[
    {"id":"s1","name":"run_shell_command","args":{"command":"cargo build --release"},"status":"success","timestamp":"ts"}
  ]}
]}"#,
        );
        let convo = Conversation::new("uuid".into(), chat);
        let path = derive_path(&convo, &DeriveConfig::default());
        let structural = path.steps[0].change["gemini://s"]
            .structural
            .as_ref()
            .unwrap();
        let calls = structural.extra["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["summary"], "cargo build --release");
    }
}
