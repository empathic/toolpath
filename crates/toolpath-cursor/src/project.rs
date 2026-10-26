//! Maps a [`ConversationView`] back to a [`CursorSession`].

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, FileMutation, Result, Role,
    ToolInvocation, Turn,
};

use crate::reader::CONTENT_PREFIX;
use crate::types::{
    BUBBLE_TYPE_ASSISTANT, BUBBLE_TYPE_USER, Bubble, BubbleGrouping, BubbleHeader, BubbleThinking,
    CAPABILITY_TOOL, ComposerData, ComposerHead, CursorSession, ModelConfig, ModelInfo,
    SelectedModel, TOOL_EDIT_FILE_V2, TOOL_GLOB_FILE_SEARCH, TOOL_READ_FILE_V2,
    TOOL_RUN_TERMINAL_COMMAND_V2, TOOL_TASK_V2, ThinkingBlock, TokenCount, ToolFormerData,
    WorkspaceIdentifier, WorkspaceUri,
};

const DEFAULT_MODEL_NAME: &str = "default";
const DEFAULT_UNIFIED_MODE: &str = "agent";
const DEFAULT_FORCE_MODE: &str = "edit";
// 18 is what Cursor 3.24 writes for an IDE chat. A `_v` 16 composer with
// `agentBackend: "cursor-agent"` is listed but opens empty in 3.24.
const COMPOSER_SCHEMA_V: u32 = 18;
const BUBBLE_SCHEMA_V: u32 = 3;
const UNIFIED_MODE_AGENT: u32 = 2;
const CAPABILITY_THINKING: u32 = crate::types::CAPABILITY_THINKING;

#[derive(Debug, Clone, Default)]
pub struct CursorProjector {
    pub composer_id: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<PathBuf>,
    pub agent_backend: Option<String>,
    pub default_model: Option<String>,
    pub title: Option<String>,
}

impl CursorProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_composer_id(mut self, id: impl Into<String>) -> Self {
        self.composer_id = Some(id.into());
        self
    }

    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    pub fn with_workspace_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.workspace_path = Some(p.into());
        self
    }

    pub fn with_agent_backend(mut self, b: impl Into<String>) -> Self {
        self.agent_backend = Some(b.into());
        self
    }

    pub fn with_default_model(mut self, m: impl Into<String>) -> Self {
        self.default_model = Some(m.into());
        self
    }

    pub fn with_title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
}

impl ConversationProjector for CursorProjector {
    type Output = CursorSession;

    fn project(&self, view: &ConversationView) -> Result<CursorSession> {
        project_view(self, view).map_err(ConvoError::Provider)
    }
}

fn is_projectable(turn: &Turn) -> bool {
    matches!(turn.role, Role::User | Role::Assistant)
}

fn project_view(
    cfg: &CursorProjector,
    view: &ConversationView,
) -> std::result::Result<CursorSession, String> {
    let composer_id = cfg
        .composer_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ensure_uuid_shape(&view.id));

    let workspace_path = cfg
        .workspace_path
        .clone()
        .or_else(|| {
            view.turns
                .iter()
                .find_map(|t| t.environment.as_ref()?.working_dir.clone())
                .map(PathBuf::from)
        })
        .or_else(|| {
            view.base
                .as_ref()
                .and_then(|b| b.working_dir.clone())
                .map(PathBuf::from)
        });
    // No directory named by the caller or the view: write none, never `/`.
    let workspace_path_str = workspace_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    let workspace_id = cfg
        .workspace_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| workspace_path_str.as_deref().map(workspace_hash));

    let title = cfg.title.clone().or_else(|| view.title(80));

    // None (the IDE's own agent) unless a caller asks for another backend.
    let agent_backend = cfg.agent_backend.clone();
    let default_model = cfg
        .default_model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_string());

    let created_at = view.started_at.map(|t| t.timestamp_millis()).or_else(|| {
        view.turns
            .first()
            .and_then(|t| parse_timestamp_ms(&t.timestamp))
    });
    let last_updated_at = view
        .last_activity
        .map(|t| t.timestamp_millis())
        .or_else(|| {
            view.turns
                .last()
                .and_then(|t| parse_timestamp_ms(&t.timestamp))
        })
        .or(created_at);

    let mut content_blobs: HashMap<String, String> = HashMap::new();
    let mut bubbles: Vec<Bubble> = Vec::with_capacity(view.turns.len());
    let mut headers: Vec<BubbleHeader> = Vec::with_capacity(view.turns.len());

    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    for turn in &view.turns {
        if !is_projectable(turn) {
            continue;
        }
        let bubble = build_bubble(turn, &mut content_blobs);

        if let Some(tc) = &bubble.token_count {
            total_input = total_input.saturating_add(tc.input_tokens.unwrap_or(0));
            total_output = total_output.saturating_add(tc.output_tokens.unwrap_or(0));
        }
        for fm in &turn.file_mutations {
            total_files.insert(fm.path.clone());
        }

        headers.push(header_for(&bubble, turn));
        bubbles.push(bubble);

        // A Cursor bubble carries one tool call (`toolFormerData`): a turn's further calls
        // each get a bubble of their own, as Cursor writes them, so none is dropped.
        for (k, tu) in turn.tool_uses.iter().enumerate().skip(1) {
            let mut extra = turn.clone();
            extra.id = format!("{}-tool-{k}", turn.id);
            extra.text = String::new();
            extra.thinking = None;
            extra.token_usage = None;
            extra.tool_uses = vec![tu.clone()];
            let bubble = build_bubble(&extra, &mut content_blobs);
            headers.push(header_for(&bubble, &extra));
            bubbles.push(bubble);
        }
    }

    // Cursor tolerates a head with no `workspaceIdentifier`, and one with an
    // id but no `uri` (a remote or folderless window).
    let workspace_identifier = workspace_id.map(|id| WorkspaceIdentifier {
        id,
        uri: workspace_path_str
            .filter(|p| p.starts_with('/'))
            .map(|p| WorkspaceUri {
                mid: Some(1),
                fs_path: Some(p.clone()),
                external: Some(format!("file://{p}")),
                path: Some(p),
                scheme: Some("file".into()),
            }),
    });

    let head = ComposerHead {
        kind: Some("head".into()),
        composer_id: composer_id.clone(),
        name: title.clone(),
        subtitle: None,
        created_at,
        last_updated_at,
        conversation_checkpoint_last_updated_at: last_updated_at,
        unified_mode: Some(DEFAULT_UNIFIED_MODE.into()),
        force_mode: Some(DEFAULT_FORCE_MODE.into()),
        is_archived: false,
        is_draft: false,
        has_unread_messages: false,
        total_lines_added: None,
        total_lines_removed: None,
        files_changed_count: if total_files.is_empty() {
            None
        } else {
            Some(total_files.len() as i64)
        },
        context_usage_percent: None,
        num_sub_composers: 0,
        workspace_identifier: workspace_identifier.clone(),
        extra: HashMap::new(),
    };

    let data = ComposerData {
        v: COMPOSER_SCHEMA_V,
        composer_id: composer_id.clone(),
        name: title,
        subtitle: None,
        created_at,
        last_updated_at,
        is_agentic: true,
        status: Some("completed".into()),
        unified_mode: Some(DEFAULT_UNIFIED_MODE.into()),
        force_mode: Some(DEFAULT_FORCE_MODE.into()),
        agent_backend,
        model_config: Some(ModelConfig {
            model_name: Some(default_model.clone()),
            max_mode: false,
            selected_models: vec![SelectedModel {
                model_id: default_model,
                parameters: vec![],
            }],
            extra: HashMap::new(),
        }),
        full_conversation_headers_only: headers,
        sub_composer_ids: vec![],
        subagent_composer_ids: vec![],
        latest_chat_generation_uuid: None,
        extra: composer_required_extras(),
    };

    // What Cursor sends the model: `conversationState` lists the history
    // blobs. Without it a resumed chat starts from nothing.
    let (conversation_state, agent_blobs) = model_history(view);
    let mut data = data;
    data.extra.insert(
        "conversationState".into(),
        Value::String(conversation_state),
    );
    data.extra.insert("hasLoaded".into(), Value::Bool(true));

    Ok(CursorSession {
        head: Some(head),
        data,
        bubbles,
        content_blobs,
        agent_blobs,
        transcript_path: None,
    })
}

fn build_bubble(turn: &Turn, content_blobs: &mut HashMap<String, String>) -> Bubble {
    let kind = match turn.role {
        Role::User => BUBBLE_TYPE_USER,
        Role::Assistant => BUBBLE_TYPE_ASSISTANT,
        Role::System | Role::Other(_) => return Bubble::default(),
    };

    let primary_tool = turn.tool_uses.first();

    let (tool_former_data, capability_type) = match primary_tool {
        Some(tu) => {
            let tf = tool_former_data_for(tu, &turn.file_mutations, content_blobs);
            (Some(tf), Some(CAPABILITY_TOOL))
        }
        None if turn.thinking.is_some() => (None, Some(CAPABILITY_THINKING)),
        _ => (None, None),
    };

    let all_thinking_blocks = match &turn.thinking {
        Some(t) if !t.is_empty() => vec![ThinkingBlock {
            text: t.clone(),
            extra: HashMap::new(),
        }],
        _ => Vec::new(),
    };

    let token_count = turn.token_usage.as_ref().map(|u| TokenCount {
        input_tokens: u.input_tokens.map(|n| n as u64),
        output_tokens: u.output_tokens.map(|n| n as u64),
        extra: HashMap::new(),
    });

    let is_tool_bubble = capability_type == Some(CAPABILITY_TOOL);
    let model_info = if is_tool_bubble {
        None
    } else {
        turn.model
            .as_deref()
            .filter(|m| !m.is_empty())
            .map(|m| ModelInfo {
                model_name: Some(m.to_string()),
                extra: HashMap::new(),
            })
    };

    // Empty-text bubbles must omit richText — emitting an empty
    // Lexical envelope throws in Cursor's renderer.
    let rich_text = if turn.text.is_empty() {
        None
    } else {
        Some(rich_text_envelope(&turn.text))
    };

    let created_at = if turn.timestamp.is_empty() {
        None
    } else {
        Some(turn.timestamp.clone())
    };

    Bubble {
        v: BUBBLE_SCHEMA_V,
        bubble_id: turn.id.clone(),
        kind,
        created_at,
        text: turn.text.clone(),
        rich_text,
        capability_type,
        conversation_state: Some("~".into()),
        unified_mode: Some(UNIFIED_MODE_AGENT),
        is_agentic: turn.role == Role::Assistant && capability_type != Some(CAPABILITY_TOOL),
        request_id: Some(String::new()),
        checkpoint_id: None,
        token_count,
        model_info,
        tool_former_data,
        // Cursor's own `thinking` object, as real bubbles carry it; the signature goes back only
        // when this turn's thinking is the text Cursor signed.
        thinking: turn
            .thinking
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|t| BubbleThinking {
                text: t.clone(),
                signature: Some(
                    turn.thinking_signature
                        .as_ref()
                        .and_then(|s| s.value_for("cursor", t))
                        .unwrap_or_default()
                        .to_string(),
                ),
                extra: HashMap::new(),
            }),
        all_thinking_blocks,
        tool_results: Vec::new(),
        extra: bubble_required_extras(is_tool_bubble),
    }
}

fn header_for(bubble: &Bubble, turn: &Turn) -> BubbleHeader {
    let has_text = !bubble.text.is_empty();
    let has_thinking = !bubble.all_thinking_blocks.is_empty();
    let mut grouping = BubbleGrouping {
        is_renderable: true,
        has_text: if has_text { Some(true) } else { None },
        has_thinking: if has_thinking { Some(true) } else { None },
        thinking_duration_ms: None,
        capability_type: bubble.capability_type,
        extra: HashMap::new(),
    };
    if has_text && bubble.text.lines().count() <= 1 && bubble.text.chars().count() <= 120 {
        grouping
            .extra
            .insert("isShortPlainText".into(), Value::Bool(true));
    }
    let _ = turn;
    BubbleHeader {
        bubble_id: bubble.bubble_id.clone(),
        kind: bubble.kind,
        grouping: Some(grouping),
        content_height_hint: None,
    }
}

fn tool_former_data_for(
    tu: &ToolInvocation,
    file_mutations: &[FileMutation],
    content_blobs: &mut HashMap<String, String>,
) -> ToolFormerData {
    let (tool_id, internal_name) = tool_id_and_name(tu);
    // Historical projection: never emit "running" — that would hang
    // Cursor's UI waiting for a result that already happened.
    let status = match &tu.result {
        Some(r) if r.is_error => "error".to_string(),
        _ => "completed".to_string(),
    };
    let normalized_input = normalize_input_for_cursor(tool_id, &tu.input);
    let params = serde_json::to_string(&normalized_input).unwrap_or_else(|_| "{}".to_string());
    let result = build_result_payload(tu, tool_id, file_mutations, content_blobs);
    let additional_data = match tool_id {
        TOOL_EDIT_FILE_V2 => Some(Value::Object(serde_json::Map::new())),
        _ => None,
    };
    ToolFormerData {
        tool: tool_id,
        tool_index: Some(0),
        model_call_id: Some(String::new()),
        tool_call_id: tu.id.clone(),
        status,
        name: internal_name,
        params,
        result,
        additional_data,
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@")?.trim_start();
    let close = rest.find("@@")?;
    let body = rest[..close].trim();
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse::<u32>().ok()?;
    let new_start = new.split(',').next()?.parse::<u32>().ok()?;
    Some((old_start, new_start))
}

// Cursor's protobuf parsers use strict decoding: unknown keys throw
// inside the chat renderer and the bubble fails to display. Rename
// foreign keys to Cursor's vocabulary and drop unrecognized ones.
fn normalize_input_for_cursor(tool_id: u32, input: &Value) -> Value {
    let Value::Object(obj) = input else {
        return input.clone();
    };

    let renames: &[(&str, &str)] = match tool_id {
        crate::types::TOOL_RUN_TERMINAL_COMMAND_V2 => &[("description", "commandDescription")],
        crate::types::TOOL_EDIT_FILE_V2 => &[
            ("file_path", "relativeWorkspacePath"),
            ("filePath", "relativeWorkspacePath"),
            ("path", "relativeWorkspacePath"),
        ],
        crate::types::TOOL_READ_FILE_V2 => &[
            ("file_path", "targetFile"),
            ("filePath", "targetFile"),
            ("path", "targetFile"),
        ],
        crate::types::TOOL_GLOB_FILE_SEARCH => &[
            ("pattern", "globPattern"),
            ("glob_pattern", "globPattern"),
            ("path", "targetDirectory"),
            ("target_directory", "targetDirectory"),
        ],
        crate::types::TOOL_RIPGREP_RAW_SEARCH => &[
            ("query", "pattern"),
            ("regex", "pattern"),
            ("target_directory", "path"),
            ("case_insensitive", "caseInsensitive"),
        ],
        crate::types::TOOL_TASK_V2 => &[("subagent_type", "subagentType")],
        _ => &[],
    };

    let mut out = serde_json::Map::new();
    let mut renamed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (foreign, cursor) in renames {
        if let Some(v) = obj.get(*foreign) {
            out.entry((*cursor).to_string())
                .or_insert_with(|| v.clone());
            renamed.insert(*foreign);
        }
    }
    let allowed: Option<&[&str]> = match tool_id {
        crate::types::TOOL_RUN_TERMINAL_COMMAND_V2 => Some(&[
            "command",
            "commandDescription",
            "cwd",
            "options",
            "parsingResult",
            "requestedSandboxPolicy",
        ]),
        crate::types::TOOL_EDIT_FILE_V2 => {
            Some(&["relativeWorkspacePath", "noCodeblock", "cloudAgentEdit"])
        }
        crate::types::TOOL_READ_FILE_V2 => Some(&[
            "targetFile",
            "effectiveUri",
            "charsLimit",
            "shouldReadEntireFile",
            "startLineOneIndexed",
            "endLineOneIndexedInclusive",
        ]),
        crate::types::TOOL_GLOB_FILE_SEARCH => Some(&["globPattern", "targetDirectory"]),
        crate::types::TOOL_RIPGREP_RAW_SEARCH => {
            Some(&["pattern", "path", "caseInsensitive", "includes", "excludes"])
        }
        crate::types::TOOL_TASK_V2 => {
            Some(&["description", "prompt", "subagentType", "model", "name"])
        }
        _ => None,
    };
    for (k, v) in obj {
        if renamed.contains(k.as_str()) {
            continue;
        }
        let keep = match allowed {
            None => true,
            Some(list) => list.contains(&k.as_str()),
        };
        if keep && !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    if tool_id == crate::types::TOOL_EDIT_FILE_V2 {
        out.entry("noCodeblock".to_string())
            .or_insert(Value::Bool(true));
        out.entry("cloudAgentEdit".to_string())
            .or_insert(Value::Bool(false));
    }
    Value::Object(out)
}

fn tool_id_and_name(tu: &ToolInvocation) -> (u32, String) {
    use toolpath_convo::ToolCategory;
    if let Some(id) = crate::types::tool_id_for_name(&tu.name) {
        return (id, tu.name.clone());
    }
    match tu.category {
        Some(ToolCategory::Shell) => (
            TOOL_RUN_TERMINAL_COMMAND_V2,
            "run_terminal_command_v2".into(),
        ),
        Some(ToolCategory::FileWrite) => (TOOL_EDIT_FILE_V2, "edit_file_v2".into()),
        Some(ToolCategory::FileRead) => (TOOL_READ_FILE_V2, "read_file_v2".into()),
        Some(ToolCategory::FileSearch) => (TOOL_GLOB_FILE_SEARCH, "glob_file_search".into()),
        Some(ToolCategory::Network) => (crate::types::TOOL_WEB_SEARCH, "web_search".into()),
        Some(ToolCategory::Delegation) => (TOOL_TASK_V2, "task_v2".into()),
        None => (crate::types::TOOL_UNSPECIFIED, tu.name.clone()),
    }
}

fn build_result_payload(
    tu: &ToolInvocation,
    tool_id: u32,
    file_mutations: &[FileMutation],
    content_blobs: &mut HashMap<String, String>,
) -> Option<String> {
    if matches!(&tu.result, Some(r) if r.is_error) {
        return None;
    }

    match tool_id {
        TOOL_EDIT_FILE_V2 => {
            // Cursor's edit-bubble renderer requires both
            // beforeContentId and afterContentId to fetch and diff
            // the blobs.
            let fm = file_mutations
                .iter()
                .find(|fm| fm.tool_id.as_deref() == Some(tu.id.as_str()))
                .or_else(|| file_mutations.iter().find(|fm| fm.path.starts_with('/')));
            let (before_hash, after_hash) = match fm {
                Some(fm) => {
                    let (before_src, after_src) = source_blob_pair(fm);
                    (
                        register_blob(content_blobs, before_src.as_deref()),
                        register_blob(content_blobs, after_src.as_deref()),
                    )
                }
                None => (None, None),
            };
            let mut obj = Map::new();
            if let Some(h) = before_hash {
                obj.insert(
                    "beforeContentId".into(),
                    Value::String(format!("{CONTENT_PREFIX}{h}")),
                );
            }
            if let Some(h) = after_hash {
                obj.insert(
                    "afterContentId".into(),
                    Value::String(format!("{CONTENT_PREFIX}{h}")),
                );
            }
            Some(serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".into()))
        }
        TOOL_RUN_TERMINAL_COMMAND_V2 => {
            let output = tu
                .result
                .as_ref()
                .map(|r| r.content.clone())
                .unwrap_or_default();
            let payload = json!({
                "output": output,
                "exitCode": 0,
                "rejected": false,
                "notInterrupted": true,
            });
            Some(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()))
        }
        _ => tu.result.as_ref().map(|r| {
            let payload = json!({ "output": r.content });
            serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into())
        }),
    }
}

fn source_blob_pair(fm: &FileMutation) -> (Option<String>, Option<String>) {
    let before = fm.before.clone().filter(|s| !s.is_empty());
    let after = fm.after.clone().filter(|s| !s.is_empty());
    if before.is_some() || after.is_some() {
        return (
            before.or_else(|| Some(String::new())),
            after.or_else(|| Some(String::new())),
        );
    }
    if let Some(diff) = fm.raw_diff.as_deref() {
        let (b, a) = reconstruct_hunks_from_diff(diff);
        if !b.is_empty() || !a.is_empty() {
            return (Some(b), Some(a));
        }
    }
    (None, None)
}

fn reconstruct_hunks_from_diff(diff: &str) -> (String, String) {
    let mut before = String::new();
    let mut after = String::new();
    let mut in_hunk = false;
    for raw in diff.lines() {
        if raw.starts_with("@@") {
            in_hunk = parse_hunk_header(raw).is_some();
            continue;
        }
        if !in_hunk {
            continue;
        }
        match raw.chars().next() {
            Some('+') => {
                after.push_str(&raw[1..]);
                after.push('\n');
            }
            Some('-') => {
                before.push_str(&raw[1..]);
                before.push('\n');
            }
            Some(' ') => {
                before.push_str(&raw[1..]);
                before.push('\n');
                after.push_str(&raw[1..]);
                after.push('\n');
            }
            Some('\\') => continue,
            None => {
                before.push('\n');
                after.push('\n');
            }
            _ => continue,
        }
    }
    (before, after)
}

fn register_blob(blobs: &mut HashMap<String, String>, body: Option<&str>) -> Option<String> {
    let body = body?;
    let hash = sha256_hex(body.as_bytes());
    blobs
        .entry(hash.clone())
        .or_insert_with(|| body.to_string());
    Some(hash)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn workspace_hash(path: &str) -> String {
    let full = sha256_hex(path.as_bytes());
    full.chars().take(32).collect()
}

pub(crate) fn parse_timestamp_ms(ts: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
        .or_else(|| {
            ts.parse::<DateTime<Utc>>()
                .ok()
                .map(|dt| dt.timestamp_millis())
        })
        .or_else(|| ts.parse::<i64>().ok())
}

pub(crate) fn ensure_uuid_shape(id: &str) -> String {
    if looks_like_uuid(id) {
        return id.to_string();
    }
    let h = sha256_hex(id.as_bytes());
    format!(
        "{}-{}-4{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32],
    )
}

fn looks_like_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let dashes = [8usize, 13, 18, 23];
    for (i, &b) in bytes.iter().enumerate() {
        let want_dash = dashes.contains(&i);
        if want_dash {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

// Empty text must serialize as `children: []` — Lexical rejects
// empty `{text: ""}` nodes and Cursor's renderer hangs on "Loading chat".
fn rich_text_envelope(text: &str) -> String {
    let children = if text.is_empty() {
        Value::Array(Vec::new())
    } else {
        json!([{
            "detail": 0,
            "format": 0,
            "mode": "normal",
            "style": "",
            "text": text,
            "type": "text",
            "version": 1,
        }])
    };
    let doc = json!({
        "root": {
            "children": [{
                "children": children,
                "direction": "ltr",
                "format": "",
                "indent": 0,
                "type": "paragraph",
                "version": 1,
            }],
            "direction": "ltr",
            "format": "",
            "indent": 0,
            "type": "root",
            "version": 1,
        }
    });
    serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into())
}

// All keys must be present — Cursor's chat loader dereferences them
// unconditionally and missing keys throw `undefined.fileSelections`,
// hanging the chat panel.
fn empty_context_value() -> Value {
    json!({
        "composers": [],
        "selectedCommits": [],
        "selectedPullRequests": [],
        "selectedImages": [],
        "selectedDocuments": [],
        "selectedVideos": [],
        "folderSelections": [],
        "fileSelections": [],
        "terminalFiles": [],
        "selections": [],
        "terminalSelections": [],
        "selectedDocs": [],
        "externalLinks": [],
        "cursorRules": [],
        "cursorCommands": [],
        "gitPRDiffSelections": [],
        "subagentSelections": [],
        "browserSelections": [],
        "extraContext": [],
        "mentions": {
            "composers": {},
            "selectedCommits": {},
            "selectedPullRequests": {},
            "gitDiff": [],
            "gitDiffFromBranchToMain": [],
            "selectedImages": {},
            "selectedDocuments": {},
            "selectedVideos": {},
            "folderSelections": {},
            "fileSelections": {},
            "terminalFiles": {},
            "selections": {},
            "terminalSelections": {},
            "selectedDocs": {},
            "externalLinks": {},
            "diffHistory": [],
            "cursorRules": {},
            "cursorCommands": {},
            "uiElementSelections": [],
            "consoleLogs": [],
            "ideEditorsState": [],
            "gitPRDiffSelections": {},
            "subagentSelections": {},
            "browserSelections": {}
        }
    })
}

/// The model-visible history of a conversation: one AI SDK `ModelMessage`
/// per message, as JSON, keyed by the SHA-256 of its bytes, and the hashes
/// in order. Both Cursor writers carry it: the IDE lists the hashes in
/// `conversationState` (blobs under `agentKv:blob:<sha256>`), the
/// `cursor-agent` CLI in field 1 of its root blob (blobs in `store.db`).
pub(crate) struct ModelHistory {
    pub(crate) order: Vec<[u8; 32]>,
    pub(crate) blobs: HashMap<String, String>,
}

pub(crate) fn model_messages(view: &ConversationView) -> ModelHistory {
    let mut blobs: HashMap<String, String> = HashMap::new();
    let mut order: Vec<[u8; 32]> = Vec::new();
    let mut push = |msg: Value| {
        let body = msg.to_string();
        let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
        blobs.insert(hex_lower(&digest), body);
        order.push(digest);
    };
    for turn in &view.turns {
        match turn.role {
            Role::User => {
                if !turn.text.is_empty() {
                    push(json!({"role": "user", "content": [{"type": "text", "text": turn.text}]}));
                }
            }
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(t) = turn.thinking.as_ref().filter(|t| !t.is_empty()) {
                    parts.push(json!({"type": "reasoning", "text": t}));
                }
                if !turn.text.is_empty() {
                    parts.push(json!({"type": "text", "text": turn.text}));
                }
                for tu in &turn.tool_uses {
                    parts.push(json!({
                        "type": "tool-call",
                        "toolCallId": tu.id,
                        "toolName": tu.name,
                        "args": tu.input,
                    }));
                }
                if !parts.is_empty() {
                    push(json!({"role": "assistant", "id": turn.id, "content": parts}));
                }
                for tu in &turn.tool_uses {
                    if let Some(r) = &tu.result {
                        push(json!({
                            "role": "tool",
                            "content": [{
                                "type": "tool-result",
                                "toolCallId": tu.id,
                                "toolName": tu.name,
                                "result": r.content,
                                "isError": r.is_error,
                            }],
                        }));
                    }
                }
            }
            // The model history keeps a system message, as the CLI's own first blob does.
            Role::System => {
                if !turn.text.is_empty() {
                    push(json!({"role": "system", "content": turn.text}));
                }
            }
            Role::Other(_) => {}
        }
    }
    ModelHistory { order, blobs }
}

/// Cursor 3.24 IDE model history: `conversationState` is `"~"` + base64 of
/// a protobuf whose field 1 repeats the 32-byte hashes of
/// [`model_messages`] in order. Observed in Cursor 3.24.12
/// (docs/agents/formats/cursor.md).
fn model_history(view: &ConversationView) -> (String, HashMap<String, String>) {
    let ModelHistory { order, blobs } = model_messages(view);
    let mut proto: Vec<u8> = Vec::with_capacity(order.len() * 34);
    for h in &order {
        crate::proto::put_bytes(&mut proto, 1, h);
    }
    (format!("~{}", base64_std(&proto)), blobs)
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn base64_std(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// Each key is required by Cursor's chat loader — omitting any of
// them throws inside the renderer and hangs the chat panel.
fn composer_required_extras() -> HashMap<String, Value> {
    let empty_obj = Value::Object(serde_json::Map::new());
    let mut out: HashMap<String, Value> = HashMap::new();
    out.insert(
        "capabilities".into(),
        json!([
            {"type": 15, "data": {"bubbleDataMap": "{}"}},
            {"type": 19, "data": {}},
            {"type": 33, "data": {}},
            {"type": 32, "data": {}},
            {"type": 23, "data": {}},
            {"type": 16, "data": {}},
            {"type": 24, "data": {}},
            {"type": 21, "data": {}}
        ]),
    );
    out.insert("context".into(), empty_context_value());
    out.insert("conversationMap".into(), empty_obj.clone());
    out.insert("codeBlockData".into(), empty_obj.clone());
    out.insert("originalFileStates".into(), empty_obj.clone());
    out.insert("usageData".into(), empty_obj);
    out
}

// Each key is required — missing keys throw `Object.entries(undefined)`
// in Cursor's renderer and the bubble doesn't display.
fn bubble_required_extras(is_tool_bubble: bool) -> HashMap<String, Value> {
    let empty_arr = Value::Array(Vec::new());
    let mut out: HashMap<String, Value> = HashMap::new();
    for k in [
        "aiWebSearchResults",
        "approximateLintErrors",
        "assistantSuggestedDiffs",
        "attachedCodeChunks",
        "attachedFileCodeChunksMetadataOnly",
        "attachedFolders",
        "attachedFoldersListDirResults",
        "attachedFoldersNew",
        "capabilities",
        "capabilityContexts",
        "codeBlocks",
        "codebaseContextChunks",
        "commits",
        "consoleLogs",
        "contextPieces",
        "cursorCommands",
        "cursorRules",
        "deletedFiles",
        "diffHistories",
        "diffsForCompressingFiles",
        "diffsSinceLastApply",
        "docsReferences",
        "documentationSelections",
        "editTrailContexts",
        "externalLinks",
        "fileDiffTrajectories",
        "gitDiffs",
        "humanChanges",
        "images",
        "interpreterResults",
        "knowledgeItems",
        "lints",
        "mcpDescriptors",
        "multiFileLinterErrors",
        "notepads",
        "pastChats",
        "projectLayouts",
        "pullRequests",
        "recentLocationsHistory",
        "recentlyViewedFiles",
        "relevantFiles",
        "suggestedCodeBlocks",
        "summarizedComposers",
        "supportedTools",
        "uiElementPicked",
        "userResponsesToSuggestedCodeBlocks",
        "webReferences",
        "workspaceUris",
    ] {
        out.insert(k.into(), empty_arr.clone());
    }
    for k in [
        "attachedHumanChanges",
        "cursorCommandsExplicitlySet",
        "existedPreviousTerminalCommand",
        "existedSubsequentTerminalCommand",
        "isRefunded",
        "pastChatsExplicitlySet",
    ] {
        out.insert(k.into(), Value::Bool(false));
    }
    if !is_tool_bubble {
        out.insert("context".into(), empty_context_value());
    }
    out.insert("todos".into(), empty_arr);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use toolpath_convo::{
        EnvironmentSnapshot, ProducerInfo, SessionBase, TokenUsage, ToolCategory, ToolInvocation,
        ToolResult,
    };

    fn user_turn(id: &str, text: &str) -> Turn {
        Turn {
            provider: None,
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-06-01T00:00:00.000Z".into(),
            text: text.into(),
            thinking: None,
            thinking_signature: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: Some(EnvironmentSnapshot {
                working_dir: Some("/proj".into()),
                vcs_branch: None,
                vcs_revision: None,
            }),
            delegations: vec![],
            file_mutations: vec![],
        }
    }

    fn assistant_turn(id: &str, text: &str) -> Turn {
        Turn {
            provider: None,
            id: id.into(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-06-01T00:00:01.000Z".into(),
            text: text.into(),
            thinking: None,
            thinking_signature: None,
            tool_uses: vec![],
            model: Some("claude-opus-4-7".into()),
            stop_reason: None,
            token_usage: Some(TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cache_read_tokens: None,
                cache_write_tokens: None,
                ..Default::default()
            }),
            attributed_token_usage: None,
            environment: Some(EnvironmentSnapshot {
                working_dir: Some("/proj".into()),
                vcs_branch: None,
                vcs_revision: None,
            }),
            delegations: vec![],
            file_mutations: vec![],
        }
    }

    fn view_with(turns: Vec<Turn>) -> ConversationView {
        ConversationView {
            id: "comp-1".into(),
            started_at: None,
            last_activity: None,
            turns,
            total_usage: None,
            provider_id: Some("cursor".into()),
            files_changed: vec![],
            session_ids: vec![],
            events: vec![],
            base: Some(SessionBase {
                working_dir: Some("/proj".into()),
                vcs_revision: None,
                vcs_branch: None,
                vcs_remote: None,
            }),
            producer: Some(ProducerInfo {
                name: "cursor".into(),
                version: Some("cursor-agent".into()),
            }),
        }
    }

    #[test]
    fn empty_view_yields_session_with_no_bubbles() {
        let s = CursorProjector::new().project(&view_with(vec![])).unwrap();
        assert!(s.bubbles.is_empty());
        assert!(s.data.full_conversation_headers_only.is_empty());
        assert!(s.head.is_some());
        assert_eq!(s.data.v, COMPOSER_SCHEMA_V);
    }

    #[test]
    fn user_turn_becomes_user_bubble() {
        let s = CursorProjector::new()
            .project(&view_with(vec![user_turn("u1", "hello")]))
            .unwrap();
        assert_eq!(s.bubbles.len(), 1);
        let b = &s.bubbles[0];
        assert_eq!(b.kind, BUBBLE_TYPE_USER);
        assert_eq!(b.v, BUBBLE_SCHEMA_V);
        assert_eq!(b.text, "hello");
        assert_eq!(b.bubble_id, "u1");
        assert!(b.rich_text.is_some());
        assert!(b.capability_type.is_none());
    }

    #[test]
    fn assistant_turn_becomes_assistant_bubble_with_model_info() {
        let s = CursorProjector::new()
            .project(&view_with(vec![assistant_turn("a1", "ok")]))
            .unwrap();
        let b = &s.bubbles[0];
        assert_eq!(b.kind, BUBBLE_TYPE_ASSISTANT);
        assert_eq!(b.text, "ok");
        assert_eq!(
            b.model_info.as_ref().unwrap().model_name.as_deref(),
            Some("claude-opus-4-7")
        );
        assert_eq!(b.token_count.as_ref().unwrap().input_tokens, Some(10));
    }

    #[test]
    fn thinking_only_turn_marks_capability_30() {
        let mut t = assistant_turn("a1", "");
        t.thinking = Some("thinking about it".into());
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let b = &s.bubbles[0];
        assert_eq!(b.capability_type, Some(CAPABILITY_THINKING));
        assert_eq!(b.all_thinking_blocks.len(), 1);
        assert_eq!(b.all_thinking_blocks[0].text, "thinking about it");
    }

    #[test]
    fn shell_tool_use_round_trips_via_native_id() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            result: Some(ToolResult {
                content: "a\nb\n".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::Shell),
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let b = &s.bubbles[0];
        assert_eq!(b.capability_type, Some(CAPABILITY_TOOL));
        let tf = b.tool_former_data.as_ref().unwrap();
        assert_eq!(tf.tool, TOOL_RUN_TERMINAL_COMMAND_V2);
        assert_eq!(tf.name, "run_terminal_command_v2");
        assert_eq!(tf.tool_call_id, "tc1");
        assert_eq!(tf.status, "completed");
        let params = tf.parse_params().unwrap();
        assert_eq!(params["command"], "ls");
        let result = tf.parse_result().unwrap().unwrap();
        assert_eq!(result["output"], "a\nb\n");
    }

    #[test]
    fn file_write_tool_registers_blobs_and_emits_result() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc2".into(),
            name: "edit_file_v2".into(),
            input: json!({"relativeWorkspacePath": "/proj/x.rs"}),
            result: Some(ToolResult {
                content: "edited /proj/x.rs".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileWrite),
        }];
        t.file_mutations = vec![FileMutation {
            path: "/proj/x.rs".into(),
            tool_id: Some("tc2".into()),
            operation: Some("add".into()),
            raw_diff: None,
            before: Some(String::new()),
            after: Some("fn main() {}".into()),
            rename_to: None,
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();

        assert!(
            s.content_blobs
                .contains_key("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        let after_hash = sha256_hex("fn main() {}".as_bytes());
        assert_eq!(
            s.content_blobs.get(&after_hash).map(String::as_str),
            Some("fn main() {}")
        );

        let tf = s.bubbles[0].tool_former_data.as_ref().unwrap();
        let result = tf.parse_result().unwrap().unwrap();
        assert_eq!(
            result["beforeContentId"],
            "composer.content.e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            result["afterContentId"],
            format!("composer.content.{after_hash}")
        );
    }

    #[test]
    fn errored_tool_emits_null_result_and_status_error() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc3".into(),
            name: "edit_file_v2".into(),
            input: json!({"relativeWorkspacePath": "/proj/x.rs"}),
            result: Some(ToolResult {
                content: "boom".into(),
                is_error: true,
            }),
            category: Some(ToolCategory::FileWrite),
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let tf = s.bubbles[0].tool_former_data.as_ref().unwrap();
        assert_eq!(tf.status, "error");
        assert!(tf.result.is_none());
    }

    #[test]
    fn no_result_yet_emits_completed_status_not_running() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc4".into(),
            name: "read_file_v2".into(),
            input: json!({"target_file": "/proj/x.rs"}),
            result: None,
            category: Some(ToolCategory::FileRead),
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let tf = s.bubbles[0].tool_former_data.as_ref().unwrap();
        assert_eq!(tf.status, "completed");
        assert_ne!(tf.status, "running");
    }

    #[test]
    fn composer_metadata_populated() {
        let s = CursorProjector::new()
            .with_title("My session")
            .with_workspace_id("workspaceid")
            .project(&view_with(vec![user_turn("u1", "hi")]))
            .unwrap();
        assert_eq!(s.data.name.as_deref(), Some("My session"));
        assert_eq!(s.head.as_ref().unwrap().name.as_deref(), Some("My session"));
        assert!(s.data.is_agentic);
        assert_eq!(s.data.unified_mode.as_deref(), Some("agent"));
        // Cursor 3.24 IDE chats carry no agentBackend; "cursor-agent" made
        // the composer open empty.
        assert_eq!(s.data.agent_backend, None);
        assert_eq!(s.data.v, 18);
        assert_eq!(
            s.data.extra.get("hasLoaded"),
            Some(&serde_json::Value::Bool(true))
        );
        let ws = s
            .head
            .as_ref()
            .unwrap()
            .workspace_identifier
            .as_ref()
            .unwrap();
        assert_eq!(ws.id, "workspaceid");
        assert_eq!(ws.uri.as_ref().unwrap().fs_path.as_deref(), Some("/proj"));
    }

    #[test]
    fn header_manifest_matches_bubbles() {
        let s = CursorProjector::new()
            .project(&view_with(vec![
                user_turn("u1", "hi"),
                assistant_turn("a1", "ok"),
            ]))
            .unwrap();
        let headers = &s.data.full_conversation_headers_only;
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].bubble_id, "u1");
        assert_eq!(headers[0].kind, BUBBLE_TYPE_USER);
        assert_eq!(headers[1].bubble_id, "a1");
        assert_eq!(headers[1].kind, BUBBLE_TYPE_ASSISTANT);
    }

    #[test]
    fn ensure_uuid_shape_passes_through_real_uuids() {
        let id = "724686cd-875e-47da-a90b-dbc3e523efb8";
        assert_eq!(ensure_uuid_shape(id), id);
    }

    #[test]
    fn ensure_uuid_shape_synthesizes_v4_shape_from_other_ids() {
        let made = ensure_uuid_shape("not-a-uuid");
        assert!(looks_like_uuid(&made), "expected v4 shape, got {made}");
    }

    #[test]
    fn remote_workspace_emits_uri_none() {
        let s = CursorProjector::new()
            .with_workspace_path(PathBuf::from("untitled-workspace"))
            .project(&view_with(vec![user_turn("u1", "hi")]))
            .unwrap();
        let ws = s
            .head
            .as_ref()
            .unwrap()
            .workspace_identifier
            .as_ref()
            .unwrap();
        assert!(ws.uri.is_none());
    }

    #[test]
    fn no_working_dir_writes_no_workspace() {
        let mut turn = user_turn("u1", "hi");
        turn.environment = None;
        let mut view = view_with(vec![turn]);
        view.base = None;

        let s = CursorProjector::new().project(&view).unwrap();
        assert!(s.head.as_ref().unwrap().workspace_identifier.is_none());
        assert_eq!(s.workspace_path(), None);

        // A caller-given id with no directory keeps the id and names no folder.
        let s = CursorProjector::new()
            .with_workspace_id("workspaceid")
            .project(&view)
            .unwrap();
        let ws = s
            .head
            .as_ref()
            .unwrap()
            .workspace_identifier
            .as_ref()
            .unwrap();
        assert_eq!(ws.id, "workspaceid");
        assert!(ws.uri.is_none());
    }

    #[test]
    fn every_canonical_tool_name_resolves_to_its_canonical_id() {
        use crate::types::{TOOL_TABLE, TOOL_UNSPECIFIED};
        for &(id, name) in TOOL_TABLE {
            let tu = ToolInvocation {
                id: "test".into(),
                name: name.to_string(),
                input: json!({}),
                result: None,
                category: None,
            };
            let (resolved_id, resolved_name) = super::tool_id_and_name(&tu);
            assert_eq!(
                resolved_id, id,
                "tool name {name:?} should resolve to id {id}, got {resolved_id}"
            );
            assert_eq!(resolved_name, name);
            if name != "unspecified" {
                assert_ne!(resolved_id, TOOL_UNSPECIFIED);
            }
        }
    }

    #[test]
    fn foreign_name_with_category_picks_canonical_tool_per_category() {
        use toolpath_convo::ToolCategory;
        let cases = [
            (ToolCategory::Shell, 15u32, "run_terminal_command_v2"),
            (ToolCategory::FileWrite, 38, "edit_file_v2"),
            (ToolCategory::FileRead, 40, "read_file_v2"),
            (ToolCategory::FileSearch, 42, "glob_file_search"),
            (ToolCategory::Network, 18, "web_search"),
            (ToolCategory::Delegation, 48, "task_v2"),
        ];
        for (cat, want_id, want_name) in cases {
            let tu = ToolInvocation {
                id: "test".into(),
                name: "Foreign_Name_Not_In_Cursor".into(),
                input: json!({}),
                result: None,
                category: Some(cat),
            };
            let (id, name) = super::tool_id_and_name(&tu);
            assert_eq!(id, want_id, "{cat:?}");
            assert_eq!(name, want_name);
        }
    }

    #[test]
    fn foreign_name_without_category_falls_back_to_unspecified() {
        let tu = ToolInvocation {
            id: "test".into(),
            name: "weird_thing_v9".into(),
            input: json!({}),
            result: None,
            category: None,
        };
        let (id, name) = super::tool_id_and_name(&tu);
        assert_eq!(id, crate::types::TOOL_UNSPECIFIED);
        assert_eq!(name, "weird_thing_v9");
    }

    #[test]
    fn projects_via_trait_object() {
        use toolpath_convo::project::AnyProjector;
        let any = AnyProjector::new(CursorProjector::new());
        let session: CursorSession = any
            .project_as(&view_with(vec![user_turn("u1", "hello")]))
            .unwrap();
        assert_eq!(session.bubbles.len(), 1);
    }

    #[test]
    fn parse_hunk_header_basic() {
        assert_eq!(super::parse_hunk_header("@@ -1,3 +1,4 @@"), Some((1, 1)));
        assert_eq!(
            super::parse_hunk_header("@@ -10,5 +20,7 @@ fn foo()"),
            Some((10, 20))
        );
        assert_eq!(super::parse_hunk_header("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(super::parse_hunk_header("@@ -0,0 +1,3 @@"), Some((0, 1)));
        assert_eq!(super::parse_hunk_header("not a hunk"), None);
    }

    #[test]
    fn reconstruct_hunks_from_diff_simple_edit() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n }\n";
        let (before, after) = super::reconstruct_hunks_from_diff(diff);
        assert_eq!(before, "fn main() {\n    println!(\"hi\");\n}\n");
        assert_eq!(after, "fn main() {\n    println!(\"hello\");\n}\n");
    }

    #[test]
    fn reconstruct_hunks_from_diff_new_file() {
        let diff = "--- /dev/null\n+++ b/x.rs\n@@ -0,0 +1,2 @@\n+fn a() {}\n+fn b() {}\n";
        let (before, after) = super::reconstruct_hunks_from_diff(diff);
        assert_eq!(before, "");
        assert_eq!(after, "fn a() {}\nfn b() {}\n");
    }

    #[test]
    fn reconstruct_hunks_from_diff_handles_blank_context_lines() {
        let diff = "@@ -1,3 +1,3 @@\n line1\n\n+line3-new\n";
        let (before, after) = super::reconstruct_hunks_from_diff(diff);
        assert_eq!(before, "line1\n\n");
        assert_eq!(after, "line1\n\nline3-new\n");
    }

    #[test]
    fn edit_tool_with_raw_diff_emits_content_blobs() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "edit_file_v2".into(),
            input: json!({"relativeWorkspacePath": "/proj/x.rs"}),
            result: Some(ToolResult {
                content: "ok".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileWrite),
        }];
        t.file_mutations = vec![FileMutation {
            path: "/proj/x.rs".into(),
            tool_id: Some("tc1".into()),
            operation: Some("edit".into()),
            raw_diff: Some("@@ -1,1 +1,1 @@\n-old\n+new\n".into()),
            before: None,
            after: None,
            rename_to: None,
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let tf = s.bubbles[0].tool_former_data.as_ref().unwrap();
        let result = tf.parse_result().unwrap().unwrap();
        let before_id = result["beforeContentId"].as_str().unwrap();
        let after_id = result["afterContentId"].as_str().unwrap();
        assert!(before_id.starts_with("composer.content."));
        assert!(after_id.starts_with("composer.content."));
        assert_ne!(before_id, after_id);
        let before_hash = before_id.trim_start_matches("composer.content.");
        let after_hash = after_id.trim_start_matches("composer.content.");
        assert_eq!(
            s.content_blobs.get(before_hash).map(String::as_str),
            Some("old\n")
        );
        assert_eq!(
            s.content_blobs.get(after_hash).map(String::as_str),
            Some("new\n")
        );
    }

    #[test]
    fn edit_tool_with_full_content_prefers_blob_over_reconstruction() {
        let mut t = assistant_turn("a1", "");
        t.tool_uses = vec![ToolInvocation {
            id: "tc1".into(),
            name: "edit_file_v2".into(),
            input: json!({"relativeWorkspacePath": "/proj/x.rs"}),
            result: Some(ToolResult {
                content: "ok".into(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileWrite),
        }];
        t.file_mutations = vec![FileMutation {
            path: "/proj/x.rs".into(),
            tool_id: Some("tc1".into()),
            operation: Some("edit".into()),
            raw_diff: Some("@@ -1,1 +1,1 @@\n-fake\n+also fake\n".into()),
            before: Some("real before".into()),
            after: Some("real after".into()),
            rename_to: None,
        }];
        let s = CursorProjector::new().project(&view_with(vec![t])).unwrap();
        let tf = s.bubbles[0].tool_former_data.as_ref().unwrap();
        let result = tf.parse_result().unwrap().unwrap();
        let before_hash = result["beforeContentId"]
            .as_str()
            .unwrap()
            .trim_start_matches("composer.content.");
        let after_hash = result["afterContentId"]
            .as_str()
            .unwrap()
            .trim_start_matches("composer.content.");
        assert_eq!(
            s.content_blobs.get(before_hash).map(String::as_str),
            Some("real before")
        );
        assert_eq!(
            s.content_blobs.get(after_hash).map(String::as_str),
            Some("real after")
        );
    }

    /// Cursor 3.24 sends the model the messages `conversationState` lists:
    /// field 1 of a protobuf, one 32-byte SHA-256 per `agentKv:blob:` row,
    /// in order. Shape observed in a native 3.24.12 chat.
    #[test]
    fn conversation_state_lists_the_history_blobs_in_order() {
        let mut a = user_turn("a1", "");
        a.role = Role::Assistant;
        a.text = "running it".into();
        a.tool_uses = vec![ToolInvocation {
            id: "call-1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
            result: Some(toolpath_convo::ToolResult {
                content: "notes.txt".into(),
                is_error: false,
            }),
            category: None,
        }];
        let s = CursorProjector::new()
            .project(&view_with(vec![user_turn("u1", "list the files"), a]))
            .unwrap();
        let cs = s.data.extra["conversationState"].as_str().unwrap();
        assert!(cs.starts_with('~'));
        let raw = decode_b64(&cs[1..]);
        assert_eq!(raw.len(), 3 * 34, "user, assistant, tool");
        let mut roles = Vec::new();
        for chunk in raw.chunks(34) {
            assert_eq!(&chunk[..2], &[0x0a, 32]);
            let hash = hex_lower(&chunk[2..]);
            let body = s
                .agent_blobs
                .get(&hash)
                .expect("every listed hash has its blob");
            let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
            assert_eq!(hex_lower(&digest), hash, "the key is the blob's sha256");
            let msg: serde_json::Value = serde_json::from_str(body).unwrap();
            roles.push(msg["role"].as_str().unwrap().to_string());
        }
        assert_eq!(roles, ["user", "assistant", "tool"]);
        let assistant: serde_json::Value = s
            .agent_blobs
            .values()
            .map(|b| serde_json::from_str::<serde_json::Value>(b).unwrap())
            .find(|m| m["role"] == "assistant")
            .unwrap();
        assert_eq!(assistant["content"][1]["type"], "tool-call");
        assert_eq!(assistant["content"][1]["toolName"], "Bash");
    }

    fn decode_b64(s: &str) -> Vec<u8> {
        crate::types::base64_std_decode(s).expect("standard base64")
    }

    #[test]
    fn system_text_reads_back_from_the_model_history_in_place() {
        let mut sys = user_turn("s0", "be terse");
        sys.role = Role::System;
        let mut sys2 = user_turn("s1", "the second instructions");
        sys2.role = Role::System;
        let view = view_with(vec![
            sys,
            user_turn("u1", "hello"),
            assistant_turn("a1", "hi"),
            sys2,
            user_turn("u2", "again"),
            assistant_turn("a2", "ok"),
        ]);
        let s = CursorProjector::new().project(&view).unwrap();
        let back = crate::session_to_view(&s);
        let shape: Vec<(Role, &str)> = back
            .turns
            .iter()
            .map(|t| (t.role.clone(), t.text.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                (Role::System, "be terse"),
                (Role::User, "hello"),
                (Role::Assistant, "hi"),
                (Role::System, "the second instructions"),
                (Role::User, "again"),
                (Role::Assistant, "ok"),
            ]
        );
        for k in 1..back.turns.len() {
            assert_eq!(
                back.turns[k].parent_id.as_deref(),
                Some(back.turns[k - 1].id.as_str())
            );
        }
        assert_eq!(back.turns[0].parent_id, None);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(decode_b64(&base64_std(b"foobar")), b"foobar");
    }
}
