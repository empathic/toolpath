//! [`GeminiProjector`] — maps a [`ConversationView`] back to a Gemini
//! [`Conversation`].
//!
//! This is the inverse of [`crate::provider::to_view`]: where `to_view`
//! reads a Gemini session directory into a provider-agnostic view,
//! `GeminiProjector` serializes that view back into the on-disk chat
//! format. Round-tripping is best-effort and lossy on Gemini-only
//! fields (full token breakdown, per-thought timestamps, per-tool-call
//! status/displayName) — the IR no longer carries them.

use std::collections::HashMap;

use serde_json::{Map, Value};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, DelegatedWork, Result, Role, TokenUsage,
    ToolCategory, ToolInvocation, Turn,
};

use crate::types::{
    ChatFile, Conversation, FunctionResponse, FunctionResponseBody, GeminiContent, GeminiMessage,
    GeminiRole, TextPart, Thought, Tokens, ToolCall,
};

// ── GeminiProjector ───────────────────────────────────────────────────

/// Project a [`ConversationView`] into a Gemini [`Conversation`].
///
/// Config fields are optional — pass them when you want to populate
/// file-level metadata that doesn't live on `ConversationView` (the
/// project hash and absolute project path). None-valued fields fall
/// through to empty strings / defaults, which Gemini CLI accepts.
///
/// # Example
///
/// ```rust
/// use toolpath_gemini::project::GeminiProjector;
/// use toolpath_convo::{ConversationProjector, ConversationView};
///
/// let view = ConversationView {
///     id: "session-uuid".into(),
///     provider_id: Some("gemini-cli".into()),
///     ..Default::default()
/// };
///
/// let projector = GeminiProjector::default();
/// let convo = projector.project(&view).unwrap();
/// assert_eq!(convo.session_uuid, "session-uuid");
/// ```
#[derive(Debug, Clone, Default)]
pub struct GeminiProjector {
    /// SHA-256 hex of the absolute project path. Round-trip callers
    /// should preserve the original value; new sessions can leave it `None`.
    pub project_hash: Option<String>,
    /// Absolute project path for [`Conversation::project_path`].
    pub project_path: Option<String>,
}

impl GeminiProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_project_hash(mut self, hash: impl Into<String>) -> Self {
        self.project_hash = Some(hash.into());
        self
    }

    pub fn with_project_path(mut self, path: impl Into<String>) -> Self {
        self.project_path = Some(path.into());
        self
    }
}

impl ConversationProjector for GeminiProjector {
    type Output = Conversation;

    fn project(&self, view: &ConversationView) -> Result<Conversation> {
        project_view(self, view).map_err(ConvoError::Provider)
    }
}

// ── Projection logic ─────────────────────────────────────────────────

fn project_view(
    cfg: &GeminiProjector,
    view: &ConversationView,
) -> std::result::Result<Conversation, String> {
    let project_hash = cfg.project_hash.clone().unwrap_or_default();

    let mut main_messages: Vec<GeminiMessage> = Vec::with_capacity(view.turns.len());
    let mut sub_agents: Vec<ChatFile> = Vec::new();

    for turn in &view.turns {
        main_messages.push(turn_to_message(turn));

        for delegation in &turn.delegations {
            sub_agents.push(delegation_to_chat_file(delegation, &project_hash));
        }
    }

    // Gemini's main chat file carries the session UUID in `sessionId`,
    // `kind: "main"`, and any project directories the session ran
    // against. Real `--resume <uuid>` matches against this `sessionId`
    // field — so the value here must be the full UUID, not a derived
    // short slug.
    let directories = cfg
        .project_path
        .as_ref()
        .map(|p| vec![std::path::PathBuf::from(p)]);

    let main = ChatFile {
        session_id: view.id.clone(),
        project_hash: project_hash.clone(),
        start_time: view.started_at,
        last_updated: view.last_activity,
        directories,
        kind: Some("main".to_string()),
        summary: None,
        messages: main_messages,
        extra: HashMap::new(),
    };

    Ok(Conversation {
        session_uuid: view.id.clone(),
        project_path: cfg.project_path.clone(),
        main,
        sub_agents,
        started_at: view.started_at,
        last_activity: view.last_activity,
    })
}

// ── Turn → GeminiMessage ─────────────────────────────────────────────

fn turn_to_message(turn: &Turn) -> GeminiMessage {
    // `Turn.extra` is gone; previously the Gemini projector pulled
    // `extra["gemini"]` for structured thought meta, full tokens, and
    // per-tool-call status. With that source removed, `build_thoughts` /
    // `build_tokens` / `build_tool_calls` fall back to the typed IR
    // fields (`Turn.thinking` as a string, `Turn.token_usage`, etc.).
    let gemini_extras: Map<String, Value> = Map::new();
    let msg_extras: HashMap<String, Value> = HashMap::new();

    GeminiMessage {
        id: turn.id.clone(),
        timestamp: turn.timestamp.clone(),
        role: role_to_gemini_role(&turn.role),
        content: build_content(turn),
        thoughts: build_thoughts(turn, &gemini_extras),
        tokens: build_tokens(turn, &gemini_extras),
        model: turn.model.clone(),
        tool_calls: build_tool_calls(turn, &gemini_extras),
        extra: msg_extras,
    }
}

fn role_to_gemini_role(role: &Role) -> GeminiRole {
    match role {
        Role::User => GeminiRole::User,
        Role::Assistant => GeminiRole::Gemini,
        Role::System => GeminiRole::Info,
        Role::Other(s) => GeminiRole::Other(s.clone()),
    }
}

/// Pick the wire-format content shape based on role.
///
/// Gemini's real sessions carry user turns as `Parts([{text}])` and
/// assistant turns as `Text(String)` (sometimes empty, when the payload
/// lives in `toolCalls`). Info/Other turns use `Text`.
fn build_content(turn: &Turn) -> GeminiContent {
    match turn.role {
        Role::User => GeminiContent::Parts(vec![TextPart {
            text: Some(turn.text.clone()),
            extra: HashMap::new(),
        }]),
        _ => GeminiContent::Text(turn.text.clone()),
    }
}

/// Rebuild `Thought[]`.
///
/// Preferred source: `extra["gemini"]["thoughts_meta"]`, which carries
/// `{subject, description, timestamp}` triples written by the forward
/// path. Falls back to splitting `Turn.thinking` on `"\n\n"` and
/// extracting subject/description from the `**subject**\n{description}`
/// shape used by `flatten_thoughts`.
fn build_thoughts(turn: &Turn, gemini_extras: &Map<String, Value>) -> Option<Vec<Thought>> {
    if let Some(Value::Array(arr)) = gemini_extras.get("thoughts_meta") {
        let thoughts: Vec<Thought> = arr
            .iter()
            .filter_map(|v| {
                let obj = v.as_object()?;
                Some(Thought {
                    subject: obj
                        .get("subject")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    description: obj
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp: obj
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect();
        return if thoughts.is_empty() {
            None
        } else {
            Some(thoughts)
        };
    }

    // Fallback: parse the flattened string.
    let thinking = turn.thinking.as_deref()?;
    let chunks: Vec<&str> = thinking.split("\n\n").collect();
    if chunks.is_empty() {
        return None;
    }
    let thoughts: Vec<Thought> = chunks
        .iter()
        .filter(|c| !c.is_empty())
        .map(|chunk| split_flattened_thought(chunk))
        .collect();
    if thoughts.is_empty() {
        None
    } else {
        Some(thoughts)
    }
}

fn split_flattened_thought(chunk: &str) -> Thought {
    // `**subject**\n{description}` or just `{description}`/`{subject}`.
    if let Some(rest) = chunk.strip_prefix("**")
        && let Some(end) = rest.find("**")
    {
        let subject = &rest[..end];
        let after = &rest[end + 2..];
        let description = after.strip_prefix('\n').unwrap_or(after);
        return Thought {
            subject: Some(subject.to_string()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            },
            timestamp: None,
        };
    }
    Thought {
        subject: None,
        description: Some(chunk.to_string()),
        timestamp: None,
    }
}

/// Rebuild `Tokens`.
///
/// Preferred source: `extra["gemini"]["tokens"]` (the full struct).
/// Fallback: derive a partial struct from `Turn.token_usage`.
fn build_tokens(turn: &Turn, gemini_extras: &Map<String, Value>) -> Option<Tokens> {
    if let Some(v) = gemini_extras.get("tokens")
        && let Ok(t) = serde_json::from_value::<Tokens>(v.clone())
    {
        return Some(t);
    }
    turn.token_usage.as_ref().map(tokens_from_common)
}

fn tokens_from_common(u: &TokenUsage) -> Tokens {
    Tokens {
        input: u.input_tokens,
        output: u.output_tokens,
        cached: u.cache_read_tokens,
        thoughts: None,
        tool: None,
        total: None,
    }
}

/// Rebuild `toolCalls[]` by zipping `Turn.tool_uses` with
/// `extra["gemini"]["tool_call_meta"]`. Missing meta entries fall back
/// to a minimal `ToolCall` with `status` derived from `result.is_error`.
fn build_tool_calls(turn: &Turn, gemini_extras: &Map<String, Value>) -> Option<Vec<ToolCall>> {
    if turn.tool_uses.is_empty() {
        return None;
    }

    let meta_by_id: HashMap<String, &Value> = gemini_extras
        .get("tool_call_meta")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let id = v.get("id")?.as_str()?.to_string();
                    Some((id, v))
                })
                .collect()
        })
        .unwrap_or_default();

    let calls: Vec<ToolCall> = turn
        .tool_uses
        .iter()
        .map(|tu| {
            tool_invocation_to_tool_call(tu, meta_by_id.get(&tu.id).copied(), &turn.timestamp)
        })
        .collect();

    Some(calls)
}

fn tool_invocation_to_tool_call(
    tu: &ToolInvocation,
    meta: Option<&Value>,
    fallback_timestamp: &str,
) -> ToolCall {
    let meta_obj = meta.and_then(Value::as_object);

    // Pick the output tool name. If the source tool is already a known
    // Gemini tool (e.g. for Gemini→Path→Gemini round-trips), keep the
    // source name verbatim. Otherwise — when projecting from a foreign
    // harness like Claude — route through the category to get Gemini's
    // canonical name, with the call's args informing FileWrite/FileRead
    // disambiguation. Falls back to the source name when the category
    // is None or has no Gemini analog.
    let name = if crate::provider::tool_category(&tu.name).is_some() {
        tu.name.clone()
    } else if let Some(cat) = tu.category
        && let Some(remapped) = crate::provider::native_name(cat, &tu.input)
    {
        remapped.to_string()
    } else {
        tu.name.clone()
    };

    let status = meta_obj
        .and_then(|m| m.get("status").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| match &tu.result {
            Some(r) if r.is_error => "error".to_string(),
            Some(_) => "success".to_string(),
            None => "pending".to_string(),
        });

    let description = meta_obj
        .and_then(|m| m.get("description").and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| synthesize_description(&name, &tu.input));

    let display_name = meta_obj
        .and_then(|m| m.get("display_name").and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| synthesize_display_name(&name, tu.category));

    let result_display = meta_obj
        .and_then(|m| m.get("result_display"))
        .and_then(|v| if v.is_null() { None } else { Some(v.clone()) })
        .or_else(|| synthesize_result_display(tu.result.as_ref()));

    let result = tu
        .result
        .as_ref()
        .map(|r| {
            vec![FunctionResponse {
                function_response: FunctionResponseBody {
                    // Use the (possibly remapped) name for consistency
                    // with the outer ToolCall.name.
                    id: tu.id.clone(),
                    name: name.clone(),
                    response: serde_json::json!({ "output": r.content }),
                },
            }]
        })
        .unwrap_or_default();

    // Real Gemini sets `renderOutputAsMarkdown: true` on every call;
    // mirror that on synthesized calls. (Carries through the existing
    // `extra` map so it serializes as a top-level field via serde flatten.)
    let mut extra = HashMap::new();
    extra.insert("renderOutputAsMarkdown".to_string(), Value::Bool(true));

    ToolCall {
        id: tu.id.clone(),
        name,
        args: tu.input.clone(),
        status,
        timestamp: fallback_timestamp.to_string(),
        result,
        result_display,
        description,
        display_name,
        extra,
    }
}

/// Synthesize a human-readable description of a tool call from the
/// args alone. Real Gemini sessions populate this with the model's
/// rationale; for projected calls we fall back to a path/command
/// summary so the UI has something useful to show.
fn synthesize_description(name: &str, args: &Value) -> Option<String> {
    let pick = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let by_name = match name {
        "run_shell_command" => pick("description").or_else(|| pick("command")),
        "read_file" | "list_directory" | "get_internal_docs" => {
            pick("file_path").or_else(|| pick("path"))
        }
        "read_many_files" => args
            .get("file_paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty()),
        "write_file" | "replace" | "edit" => pick("file_path"),
        "glob" | "grep_search" | "search_file_content" => pick("pattern"),
        "web_fetch" => pick("url"),
        "google_web_search" => pick("query"),
        "task" | "activate_skill" => pick("description")
            .or_else(|| pick("prompt"))
            .or_else(|| pick("subagent_type")),
        _ => None,
    };
    by_name.or_else(|| generic_description_fallback(args))
}

/// Last-resort description synthesizer for tools we don't have a
/// per-name template for (foreign harness tools without a Gemini analog,
/// MCP tools with arbitrary names, etc.). Picks the first plausible
/// human-readable string from a small list of conventional arg keys.
fn generic_description_fallback(args: &Value) -> Option<String> {
    static FALLBACK_KEYS: &[&str] = &[
        "description",
        "subject",
        "summary",
        "title",
        "prompt",
        "command",
        "query",
        "pattern",
        "url",
        "path",
        "file_path",
        "task_id",
        "taskId",
        "id",
        "name",
    ];
    for key in FALLBACK_KEYS {
        if let Some(s) = args.get(*key).and_then(Value::as_str)
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Friendly UI label for Gemini's tool palette. Real Gemini sessions
/// carry these on every call.
fn synthesize_display_name(name: &str, category: Option<ToolCategory>) -> Option<String> {
    let by_name = match name {
        "run_shell_command" => Some("Shell"),
        "read_file" => Some("ReadFile"),
        "read_many_files" => Some("ReadManyFiles"),
        "list_directory" => Some("ListDirectory"),
        "get_internal_docs" => Some("GetInternalDocs"),
        "write_file" => Some("WriteFile"),
        "replace" => Some("Replace"),
        "edit" => Some("Edit"),
        "glob" => Some("Glob"),
        "grep_search" | "search_file_content" => Some("SearchText"),
        "web_fetch" => Some("WebFetch"),
        "google_web_search" => Some("GoogleSearch"),
        "task" => Some("Task"),
        "activate_skill" => Some("ActivateSkill"),
        _ => None,
    };
    if let Some(s) = by_name {
        return Some(s.to_string());
    }
    // Category fallback for foreign tools the projector recognized
    // categorically but didn't get a Gemini-vocabulary name remap.
    if let Some(c) = category {
        return Some(
            match c {
                ToolCategory::Shell => "Shell",
                ToolCategory::FileRead => "ReadFile",
                ToolCategory::FileSearch => "Search",
                ToolCategory::FileWrite => "WriteFile",
                ToolCategory::Network => "Web",
                ToolCategory::Delegation => "Task",
            }
            .to_string(),
        );
    }
    // Last resort: use the source tool name verbatim. Better than `None`
    // so the UI has *something* to label the call with.
    if !name.is_empty() {
        Some(name.to_string())
    } else {
        None
    }
}

/// Bare-string `resultDisplay`. Gemini-native tools sometimes carry
/// structured shapes (fileDiff objects, styled-text arrays); we only
/// have the result text from `ToolResult`, so we render it as a plain
/// string. Gemini accepts any JSON value here.
fn synthesize_result_display(result: Option<&toolpath_convo::ToolResult>) -> Option<Value> {
    result.map(|r| Value::String(r.content.clone()))
}

// ── Delegation → sub-agent ChatFile ───────────────────────────────────

fn delegation_to_chat_file(d: &DelegatedWork, project_hash: &str) -> ChatFile {
    let messages: Vec<GeminiMessage> = d.turns.iter().map(turn_to_message).collect();

    let start_time = d
        .turns
        .first()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t.timestamp).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let last_updated = d
        .turns
        .last()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t.timestamp).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    ChatFile {
        session_id: d.agent_id.clone(),
        project_hash: project_hash.to_string(),
        start_time,
        last_updated,
        directories: None,
        kind: Some("subagent".to_string()),
        summary: d.result.clone(),
        messages,
        extra: HashMap::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use toolpath_convo::{EnvironmentSnapshot, ToolCategory, ToolResult};

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            message_id: None,
            role: Role::User,
            timestamp: "2026-04-17T15:00:00Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn assistant_turn(id: &str, text: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: None,
            message_id: None,
            role: Role::Assistant,
            timestamp: "2026-04-17T15:00:01Z".into(),
            text: text.into(),
            thinking: None,
            tool_uses: vec![],
            model: Some("gemini-3-flash-preview".into()),
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: Vec::new(),
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "session-uuid".into(),
            started_at: None,
            last_activity: None,
            turns,
            total_usage: None,
            provider_id: Some("gemini-cli".into()),
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn test_empty_view_projects_cleanly() {
        let view = view_with(vec![]);
        let convo = GeminiProjector::default().project(&view).unwrap();
        assert_eq!(convo.session_uuid, "session-uuid");
        assert!(convo.main.messages.is_empty());
        assert!(convo.sub_agents.is_empty());
    }

    #[test]
    fn test_user_content_becomes_parts() {
        let view = view_with(vec![user_turn("u1", "Hello")]);
        let convo = GeminiProjector::default().project(&view).unwrap();
        let msg = &convo.main.messages[0];
        assert_eq!(msg.role, GeminiRole::User);
        match &msg.content {
            GeminiContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].text.as_deref(), Some("Hello"));
            }
            other => panic!("expected Parts, got {:?}", other),
        }
    }

    #[test]
    fn test_assistant_content_becomes_text() {
        let view = view_with(vec![assistant_turn("a1", "Hi")]);
        let convo = GeminiProjector::default().project(&view).unwrap();
        let msg = &convo.main.messages[0];
        assert_eq!(msg.role, GeminiRole::Gemini);
        assert_eq!(msg.model.as_deref(), Some("gemini-3-flash-preview"));
        match &msg.content {
            GeminiContent::Text(s) => assert_eq!(s, "Hi"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn test_system_role_maps_to_info() {
        let mut t = user_turn("s1", "cancelled");
        t.role = Role::System;
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        assert_eq!(convo.main.messages[0].role, GeminiRole::Info);
    }

    #[test]
    fn test_thoughts_fallback_from_flattened_string() {
        // No gemini.thoughts_meta — projector should still un-flatten the string.
        let mut t = assistant_turn("a1", "");
        t.thinking = Some("**Searching**\nlooking in /auth\n\n**Plan**\ntry token path".into());
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let thoughts = convo.main.messages[0].thoughts.as_ref().unwrap();
        assert_eq!(thoughts.len(), 2);
        assert_eq!(thoughts[0].subject.as_deref(), Some("Searching"));
        assert_eq!(thoughts[0].description.as_deref(), Some("looking in /auth"));
    }

    #[test]
    fn test_tokens_fallback_from_common_token_usage() {
        let mut t = assistant_turn("a1", "Done.");
        t.token_usage = Some(TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_tokens: Some(20),
            cache_write_tokens: None,
        });
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let tokens = convo.main.messages[0].tokens.as_ref().unwrap();
        assert_eq!(tokens.input, Some(100));
        assert_eq!(tokens.output, Some(50));
        assert_eq!(tokens.cached, Some(20));
        // thoughts/tool/total unknown on the fallback path
        assert!(tokens.total.is_none());
    }

    #[test]
    fn test_tool_call_with_success_result_wraps_into_function_response() {
        let mut t = assistant_turn("a1", "Reading.");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            result: Some(ToolResult {
                content: "fn main(){}".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let calls = convo.main.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.name, "read_file");
        assert_eq!(call.status, "success");
        assert_eq!(call.result.len(), 1);
        assert_eq!(call.result[0].function_response.id, "tc1");
        assert_eq!(call.result[0].function_response.name, "read_file");
        assert_eq!(
            call.result[0].function_response.response["output"],
            serde_json::json!("fn main(){}")
        );
    }

    #[test]
    fn test_tool_call_with_error_result_sets_error_status() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "run_shell_command".into(),
            input: serde_json::json!({"command": "nope"}),
            result: Some(ToolResult {
                content: "boom".into(),
                is_error: true,
            }),
            category: Some(ToolCategory::Shell),
        }];
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        let call = &convo.main.messages[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.status, "error");
    }

    #[test]
    fn test_delegation_becomes_subagent_chat_file() {
        let mut t = assistant_turn("a1", "delegating");
        t.delegations = vec![DelegatedWork {
            agent_id: "helper-session".into(),
            prompt: "search for the bug".into(),
            turns: vec![user_turn("su1", "search for the bug"), {
                let mut r = assistant_turn("sa1", "found it");
                r.timestamp = "2026-04-17T15:10:00Z".into();
                r
            }],
            result: Some("fixed line 42".into()),
        }];
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        assert_eq!(convo.sub_agents.len(), 1);
        let sub = &convo.sub_agents[0];
        assert_eq!(sub.session_id, "helper-session");
        assert_eq!(sub.kind.as_deref(), Some("subagent"));
        assert_eq!(sub.summary.as_deref(), Some("fixed line 42"));
        assert_eq!(sub.messages.len(), 2);
    }

    #[test]
    fn test_environment_does_not_appear_on_message() {
        // `environment` is a ConversationView-level concern, not a
        // per-message concern on the Gemini wire format. Projector
        // should simply ignore it (it'll be None on the output).
        let mut t = user_turn("u1", "hi");
        t.environment = Some(EnvironmentSnapshot {
            working_dir: Some("/abs/myrepo".into()),
            vcs_branch: Some("main".into()),
            vcs_revision: None,
        });
        let convo = GeminiProjector::default()
            .project(&view_with(vec![t]))
            .unwrap();
        // The projected main file has no directories field by default.
        assert!(convo.main.directories.is_none());
    }

    #[test]
    fn test_project_hash_and_path_propagate() {
        let view = view_with(vec![user_turn("u1", "hi")]);
        let projector = GeminiProjector::new()
            .with_project_hash("deadbeef")
            .with_project_path("/abs/myrepo");
        let convo = projector.project(&view).unwrap();
        assert_eq!(convo.main.project_hash, "deadbeef");
        assert_eq!(convo.project_path.as_deref(), Some("/abs/myrepo"));
    }

    #[test]
    fn test_output_chat_file_serde_roundtrip() {
        // The projected ChatFile must survive a JSON round-trip
        // (i.e. load fine back into Gemini's type).
        let mut t = assistant_turn("a1", "Hi there.");
        t.token_usage = Some(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_tokens: None,
            cache_write_tokens: None,
        });
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/a.rs"}),
            result: Some(ToolResult {
                content: "fn a(){}".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        }];

        let convo = GeminiProjector::default()
            .project(&view_with(vec![user_turn("u1", "Read src/a.rs"), t]))
            .unwrap();

        let json = serde_json::to_string(&convo.main).unwrap();
        let back: ChatFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].tool_calls().len(), 1);
        assert_eq!(back.messages[1].tool_calls()[0].result_text(), "fn a(){}");
    }
}
