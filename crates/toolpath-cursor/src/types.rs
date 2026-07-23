//! On-disk schema for Cursor's bubble store. See
//! `docs/agents/formats/cursor.md` for the full field census.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

// ── Composer index (`ItemTable.composer.composerHeaders`) ──────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposerHeaders {
    #[serde(rename = "allComposers", default)]
    pub all_composers: Vec<ComposerHead>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposerHead {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(rename = "composerId")]
    pub composer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(
        rename = "lastUpdatedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_updated_at: Option<i64>,
    #[serde(
        rename = "conversationCheckpointLastUpdatedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation_checkpoint_last_updated_at: Option<i64>,
    #[serde(
        rename = "unifiedMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unified_mode: Option<String>,
    #[serde(rename = "forceMode", default, skip_serializing_if = "Option::is_none")]
    pub force_mode: Option<String>,
    #[serde(rename = "isArchived", default)]
    pub is_archived: bool,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
    #[serde(rename = "hasUnreadMessages", default)]
    pub has_unread_messages: bool,
    #[serde(
        rename = "totalLinesAdded",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub total_lines_added: Option<i64>,
    #[serde(
        rename = "totalLinesRemoved",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub total_lines_removed: Option<i64>,
    #[serde(
        rename = "filesChangedCount",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub files_changed_count: Option<i64>,
    #[serde(
        rename = "contextUsagePercent",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub context_usage_percent: Option<f64>,
    #[serde(rename = "numSubComposers", default)]
    pub num_sub_composers: u32,
    #[serde(
        rename = "workspaceIdentifier",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_identifier: Option<WorkspaceIdentifier>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

impl ComposerHead {
    pub fn created_at_utc(&self) -> Option<DateTime<Utc>> {
        self.created_at
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
    }

    pub fn last_updated_at_utc(&self) -> Option<DateTime<Utc>> {
        self.last_updated_at
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
    }

    pub fn workspace_path(&self) -> Option<PathBuf> {
        self.workspace_identifier
            .as_ref()?
            .uri
            .as_ref()?
            .fs_path
            .as_deref()
            .map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceIdentifier {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<WorkspaceUri>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceUri {
    #[serde(rename = "$mid", default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<i64>,
    #[serde(rename = "fsPath", default, skip_serializing_if = "Option::is_none")]
    pub fs_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
}

// ── Composer data (`cursorDiskKV.composerData:<uuid>`) ─────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComposerData {
    #[serde(rename = "_v", default)]
    pub v: u32,
    #[serde(rename = "composerId")]
    pub composer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(
        rename = "lastUpdatedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_updated_at: Option<i64>,
    #[serde(rename = "isAgentic", default)]
    pub is_agentic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(
        rename = "unifiedMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unified_mode: Option<String>,
    #[serde(rename = "forceMode", default, skip_serializing_if = "Option::is_none")]
    pub force_mode: Option<String>,
    #[serde(
        rename = "agentBackend",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub agent_backend: Option<String>,
    #[serde(
        rename = "modelConfig",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_config: Option<ModelConfig>,
    /// May contain more entries than there are `bubbleId:` rows on disk;
    /// don't use for iteration order.
    #[serde(rename = "fullConversationHeadersOnly", default)]
    pub full_conversation_headers_only: Vec<BubbleHeader>,
    #[serde(
        rename = "subComposerIds",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub sub_composer_ids: Vec<String>,
    #[serde(
        rename = "subagentComposerIds",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub subagent_composer_ids: Vec<String>,
    #[serde(
        rename = "latestChatGenerationUUID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_chat_generation_uuid: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

impl ComposerData {
    pub fn created_at_utc(&self) -> Option<DateTime<Utc>> {
        self.created_at
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
    }

    pub fn last_updated_at_utc(&self) -> Option<DateTime<Utc>> {
        self.last_updated_at
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
    }

    pub fn default_model(&self) -> Option<&str> {
        self.model_config
            .as_ref()
            .and_then(|mc| mc.model_name.as_deref())
            .filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(rename = "modelName", default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(rename = "maxMode", default)]
    pub max_mode: bool,
    #[serde(
        rename = "selectedModels",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub selected_models: Vec<SelectedModel>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelectedModel {
    #[serde(rename = "modelId")]
    pub model_id: String,
    // Must serialize even when empty — Cursor's loader calls
    // `parameters.map(...)` unconditionally; absent throws and hangs
    // the chat panel.
    #[serde(default)]
    pub parameters: Vec<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BubbleHeader {
    #[serde(rename = "bubbleId")]
    pub bubble_id: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<BubbleGrouping>,
    #[serde(
        rename = "contentHeightHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub content_height_hint: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BubbleGrouping {
    #[serde(rename = "isRenderable", default)]
    pub is_renderable: bool,
    #[serde(rename = "hasText", default, skip_serializing_if = "Option::is_none")]
    pub has_text: Option<bool>,
    #[serde(
        rename = "hasThinking",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub has_thinking: Option<bool>,
    #[serde(
        rename = "thinkingDurationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_duration_ms: Option<u64>,
    #[serde(
        rename = "capabilityType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub capability_type: Option<u32>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

// ── Bubble (`cursorDiskKV.bubbleId:<composer>:<bubble>`) ────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bubble {
    #[serde(rename = "_v", default)]
    pub v: u32,
    #[serde(rename = "bubbleId")]
    pub bubble_id: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default)]
    pub text: String,
    /// Escaped JSON string holding a Lexical document. Opaque.
    #[serde(rename = "richText", default, skip_serializing_if = "Option::is_none")]
    pub rich_text: Option<String>,
    /// `15` = tool, `30` = thinking, `null` = text.
    #[serde(
        rename = "capabilityType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub capability_type: Option<u32>,
    #[serde(
        rename = "conversationState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation_state: Option<String>,
    #[serde(
        rename = "unifiedMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unified_mode: Option<u32>,
    #[serde(rename = "isAgentic", default)]
    pub is_agentic: bool,
    #[serde(rename = "requestId", default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(
        rename = "checkpointId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub checkpoint_id: Option<String>,
    #[serde(
        rename = "tokenCount",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_count: Option<TokenCount>,
    #[serde(rename = "modelInfo", default, skip_serializing_if = "Option::is_none")]
    pub model_info: Option<ModelInfo>,
    #[serde(
        rename = "toolFormerData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_former_data: Option<ToolFormerData>,
    /// Must serialize as `[]` when empty — Cursor's renderer calls
    /// `Object.entries(undefined)` on the thinking-blocks indexer.
    #[serde(rename = "allThinkingBlocks", default)]
    pub all_thinking_blocks: Vec<ThinkingBlock>,
    /// Must serialize as `[]` when empty — same renderer trap as
    /// `allThinkingBlocks`.
    #[serde(rename = "toolResults", default)]
    pub tool_results: Vec<Value>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

impl Bubble {
    pub fn created_at_utc(&self) -> Option<DateTime<Utc>> {
        self.created_at.as_ref()?.parse::<DateTime<Utc>>().ok()
    }

    pub fn is_user(&self) -> bool {
        self.kind == BUBBLE_TYPE_USER
    }

    pub fn is_assistant(&self) -> bool {
        self.kind == BUBBLE_TYPE_ASSISTANT
    }

    pub fn is_thinking(&self) -> bool {
        self.capability_type == Some(CAPABILITY_THINKING)
    }

    pub fn is_tool(&self) -> bool {
        self.tool_former_data.is_some() || self.capability_type == Some(CAPABILITY_TOOL)
    }

    pub fn is_summarization(&self) -> bool {
        self.capability_type == Some(CAPABILITY_SUMMARIZATION)
    }
}

pub const BUBBLE_TYPE_USER: u8 = 1;
pub const BUBBLE_TYPE_ASSISTANT: u8 = 2;

pub const CAPABILITY_TOOL: u32 = 15;
pub const CAPABILITY_THINKING: u32 = 30;
/// The `/summarize` boundary marker. The bubble has no recoverable summary or
/// kept set (those are server-side), so the provider skips it — see
/// `docs/agents/formats/cursor.md`.
pub const CAPABILITY_SUMMARIZATION: u32 = 22;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCount {
    #[serde(
        rename = "inputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<u64>,
    #[serde(
        rename = "outputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(rename = "modelName", default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThinkingBlock {
    #[serde(default)]
    pub text: String,
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, Value>,
}

// ── Tool call ─────────────────────────────────────────────────────────

/// `params` and `result` are JSON strings (double-encoded on the wire);
/// parse on use via [`ToolFormerData::parse_params`] / [`ToolFormerData::parse_result`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolFormerData {
    pub tool: u32,
    #[serde(rename = "toolIndex", default, skip_serializing_if = "Option::is_none")]
    pub tool_index: Option<u32>,
    #[serde(
        rename = "modelCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_call_id: Option<String>,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    /// `"completed"` | `"error"` | `"running"`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub params: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(
        rename = "additionalData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_data: Option<Value>,
}

impl ToolFormerData {
    pub fn parse_params(&self) -> std::result::Result<Value, serde_json::Error> {
        if self.params.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&self.params)
    }

    pub fn parse_result(&self) -> std::result::Result<Option<Value>, serde_json::Error> {
        match &self.result {
            None => Ok(None),
            Some(s) if s.is_empty() => Ok(None),
            Some(s) => Ok(Some(serde_json::from_str(s)?)),
        }
    }

    pub fn is_error(&self) -> bool {
        self.status == "error"
    }

    pub fn is_completed(&self) -> bool {
        self.status == "completed"
    }
}

/// Cursor's numeric tool-dispatch enum, extracted from
/// `Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js`.
/// The numeric id is what Cursor's UI dispatches on; a projector that
/// wants its output to load back into Cursor.app must emit the
/// canonical id, not just preserve the name.
pub const TOOL_TABLE: &[(u32, &str)] = &[
    (0, "unspecified"),
    (1, "read_semsearch_files"),
    (3, "ripgrep_search"),
    (5, "read_file"),
    (6, "list_dir"),
    (7, "edit_file"),
    (8, "file_search"),
    (9, "semantic_search_full"),
    (11, "delete_file"),
    (12, "reapply"),
    (15, "run_terminal_command_v2"),
    (16, "fetch_rules"),
    (18, "web_search"),
    (19, "mcp"),
    (23, "search_symbols"),
    (24, "background_composer_followup"),
    (25, "knowledge_base"),
    (26, "fetch_pull_request"),
    (27, "deep_search"),
    (28, "create_diagram"),
    (29, "fix_lints"),
    (30, "read_lints"),
    (31, "go_to_definition"),
    (32, "task"),
    (33, "await_task"),
    (34, "todo_read"),
    (35, "todo_write"),
    (38, "edit_file_v2"),
    (39, "list_dir_v2"),
    (40, "read_file_v2"),
    (41, "ripgrep_raw_search"),
    (42, "glob_file_search"),
    (43, "create_plan"),
    (44, "list_mcp_resources"),
    (45, "read_mcp_resource"),
    (46, "read_project"),
    (47, "update_project"),
    (48, "task_v2"),
    (49, "call_mcp_tool"),
    (50, "apply_agent_diff"),
    (51, "ask_question"),
    (52, "switch_mode"),
    (53, "generate_image"),
    (54, "computer_use"),
    (55, "write_shell_stdin"),
    (56, "record_screen"),
    (57, "web_fetch"),
    (58, "report_bugfix_results"),
    (59, "ai_attribution"),
    (60, "mcp_auth"),
    (61, "reflect"),
    (62, "await"),
    (63, "get_mcp_tools"),
];

pub const TOOL_UNSPECIFIED: u32 = 0;

pub fn tool_id_for_name(name: &str) -> Option<u32> {
    TOOL_TABLE
        .iter()
        .find_map(|(id, n)| if *n == name { Some(*id) } else { None })
}

pub fn tool_name_for_id(id: u32) -> Option<&'static str> {
    TOOL_TABLE
        .iter()
        .find_map(|(i, name)| if *i == id { Some(*name) } else { None })
}

pub const TOOL_RUN_TERMINAL_COMMAND_V2: u32 = 15;
pub const TOOL_EDIT_FILE_V2: u32 = 38;
pub const TOOL_READ_FILE_V2: u32 = 40;
pub const TOOL_GLOB_FILE_SEARCH: u32 = 42;
pub const TOOL_RIPGREP_RAW_SEARCH: u32 = 41;
pub const TOOL_TASK_V2: u32 = 48;
pub const TOOL_WEB_SEARCH: u32 = 18;

// ── Session — what the reader emits ────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CursorSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<ComposerHead>,
    pub data: ComposerData,
    #[serde(default)]
    pub bubbles: Vec<Bubble>,
    /// Keyed by hash (the part after `composer.content.`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub content_blobs: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
}

impl CursorSession {
    pub fn id(&self) -> &str {
        &self.data.composer_id
    }

    pub fn title(&self) -> Option<String> {
        self.data
            .name
            .clone()
            .or_else(|| self.head.as_ref().and_then(|h| h.name.clone()))
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.data
            .created_at_utc()
            .or_else(|| self.head.as_ref().and_then(|h| h.created_at_utc()))
    }

    pub fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.data
            .last_updated_at_utc()
            .or_else(|| self.head.as_ref().and_then(|h| h.last_updated_at_utc()))
    }

    pub fn workspace_path(&self) -> Option<PathBuf> {
        self.head.as_ref().and_then(|h| h.workspace_path())
    }

    pub fn first_user_text(&self) -> Option<String> {
        self.bubbles
            .iter()
            .find(|b| b.is_user() && !b.text.is_empty())
            .map(|b| b.text.clone())
    }

    pub fn content_blob(&self, hash: &str) -> Option<&str> {
        self.content_blobs.get(hash).map(|s| s.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorSessionMetadata {
    pub id: String,
    pub name: Option<String>,
    pub subtitle: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub unified_mode: Option<String>,
    pub workspace_path: Option<PathBuf>,
    pub message_count: usize,
    pub first_user_message: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_table_round_trips_name_and_id() {
        for (id, name) in TOOL_TABLE {
            assert_eq!(tool_id_for_name(name), Some(*id));
            assert_eq!(tool_name_for_id(*id), Some(*name));
        }
    }

    #[test]
    fn tool_table_has_all_observed_ids() {
        for &id in &[15u32, 38, 40, 41, 42, 48] {
            assert!(
                tool_name_for_id(id).is_some(),
                "expected tool id {id} in TOOL_TABLE",
            );
        }
    }

    #[test]
    fn tool_table_unknown_lookups_return_none() {
        assert_eq!(tool_id_for_name("future_tool_v9"), None);
        assert_eq!(tool_name_for_id(9999), None);
    }

    #[test]
    fn parse_minimal_composer_data() {
        let raw = r#"{
            "_v": 16,
            "composerId": "abcd-1",
            "name": "Test session",
            "createdAt": 1780325474978,
            "lastUpdatedAt": 1780325508923,
            "isAgentic": true,
            "unifiedMode": "agent",
            "agentBackend": "cursor-agent",
            "modelConfig": {
                "modelName": "default",
                "maxMode": false,
                "selectedModels": [{"modelId": "default", "parameters": []}]
            },
            "fullConversationHeadersOnly": [
                {"bubbleId": "b1", "type": 1, "grouping": {"isRenderable": true, "hasText": true}}
            ],
            "futureField": "kept"
        }"#;
        let cd: ComposerData = serde_json::from_str(raw).unwrap();
        assert_eq!(cd.v, 16);
        assert_eq!(cd.composer_id, "abcd-1");
        assert_eq!(cd.name.as_deref(), Some("Test session"));
        assert!(cd.is_agentic);
        assert_eq!(cd.unified_mode.as_deref(), Some("agent"));
        assert_eq!(cd.default_model(), Some("default"));
        assert_eq!(cd.full_conversation_headers_only.len(), 1);
        assert_eq!(
            cd.extra.get("futureField"),
            Some(&Value::String("kept".into()))
        );
    }

    #[test]
    fn parse_user_bubble() {
        let raw = r#"{
            "_v": 3,
            "type": 1,
            "bubbleId": "bub-user-1",
            "createdAt": "2026-06-01T14:51:48.926Z",
            "text": "hello",
            "conversationState": "~"
        }"#;
        let b: Bubble = serde_json::from_str(raw).unwrap();
        assert!(b.is_user());
        assert!(!b.is_assistant());
        assert_eq!(b.text, "hello");
        assert_eq!(b.created_at.as_deref(), Some("2026-06-01T14:51:48.926Z"));
        assert!(b.created_at_utc().is_some());
    }

    #[test]
    fn parse_assistant_tool_bubble() {
        let raw = r#"{
            "_v": 3,
            "type": 2,
            "bubbleId": "bub-tool-1",
            "createdAt": "2026-06-01T14:52:08.647Z",
            "text": "",
            "capabilityType": 15,
            "tokenCount": {"inputTokens": 0, "outputTokens": 0},
            "toolFormerData": {
                "tool": 38,
                "toolIndex": 0,
                "modelCallId": "",
                "toolCallId": "tool_x",
                "status": "completed",
                "name": "edit_file_v2",
                "params": "{\"relativeWorkspacePath\":\"/p/x.rs\",\"noCodeblock\":true}",
                "result": "{\"beforeContentId\":\"composer.content.e3b0c4\",\"afterContentId\":\"composer.content.abcd\"}"
            }
        }"#;
        let b: Bubble = serde_json::from_str(raw).unwrap();
        assert!(b.is_assistant());
        assert!(b.is_tool());
        let tf = b.tool_former_data.as_ref().unwrap();
        assert_eq!(tf.tool, TOOL_EDIT_FILE_V2);
        assert!(tf.is_completed());
        let params = tf.parse_params().unwrap();
        assert_eq!(params["relativeWorkspacePath"], "/p/x.rs");
        let result = tf.parse_result().unwrap().unwrap();
        assert_eq!(result["afterContentId"], "composer.content.abcd");
    }

    #[test]
    fn parse_assistant_thinking_bubble() {
        let raw = r#"{
            "_v": 3,
            "type": 2,
            "bubbleId": "bub-think-1",
            "createdAt": "2026-06-01T14:52:01.382Z",
            "text": "",
            "capabilityType": 30
        }"#;
        let b: Bubble = serde_json::from_str(raw).unwrap();
        assert!(b.is_thinking());
        assert!(!b.is_tool());
    }

    #[test]
    fn tool_former_error_has_no_result() {
        let raw = r#"{
            "tool": 38,
            "toolCallId": "tool_err",
            "status": "error",
            "name": "edit_file_v2",
            "params": "{}",
            "result": null
        }"#;
        let tf: ToolFormerData = serde_json::from_str(raw).unwrap();
        assert!(tf.is_error());
        assert!(tf.parse_result().unwrap().is_none());
    }

    #[test]
    fn composer_headers_roundtrip() {
        let raw = r#"{
            "allComposers": [
                {
                    "type": "head",
                    "composerId": "abcd-1",
                    "name": "X",
                    "createdAt": 1000,
                    "lastUpdatedAt": 2000,
                    "unifiedMode": "agent",
                    "isArchived": false,
                    "isDraft": false,
                    "hasUnreadMessages": false,
                    "workspaceIdentifier": {
                        "id": "ws1",
                        "uri": {"$mid": 1, "fsPath": "/p", "path": "/p", "scheme": "file"}
                    }
                }
            ]
        }"#;
        let h: ComposerHeaders = serde_json::from_str(raw).unwrap();
        assert_eq!(h.all_composers.len(), 1);
        let head = &h.all_composers[0];
        assert_eq!(head.composer_id, "abcd-1");
        assert_eq!(head.workspace_path().unwrap(), PathBuf::from("/p"));
        assert!(head.created_at_utc().is_some());
    }

    #[test]
    fn composer_headers_tolerate_empty_uri_for_remote_workspace() {
        let raw = r#"{
            "allComposers": [
                {
                    "type": "head",
                    "composerId": "abcd-2",
                    "workspaceIdentifier": {"id": "1780325463952", "uri": {}}
                }
            ]
        }"#;
        let h: ComposerHeaders = serde_json::from_str(raw).unwrap();
        assert_eq!(h.all_composers.len(), 1);
        let head = &h.all_composers[0];
        assert!(head.workspace_path().is_none());
        assert_eq!(
            head.workspace_identifier.as_ref().unwrap().id,
            "1780325463952"
        );
    }

    #[test]
    fn composer_headers_tolerate_missing_workspace_identifier() {
        let raw = r#"{
            "allComposers": [
                {"type": "head", "composerId": "abcd-3"}
            ]
        }"#;
        let h: ComposerHeaders = serde_json::from_str(raw).unwrap();
        assert!(h.all_composers[0].workspace_identifier.is_none());
        assert!(h.all_composers[0].workspace_path().is_none());
    }

    #[test]
    fn unknown_capability_type_round_trips() {
        let raw = r#"{
            "_v": 3,
            "type": 2,
            "bubbleId": "b",
            "text": "x",
            "capabilityType": 999
        }"#;
        let b: Bubble = serde_json::from_str(raw).unwrap();
        assert_eq!(b.capability_type, Some(999));
        assert!(!b.is_thinking());
        assert!(!b.is_tool());
    }

    #[test]
    fn unknown_tool_id_parses() {
        let raw = r#"{
            "tool": 12345,
            "toolCallId": "tool_z",
            "status": "completed",
            "name": "future_tool_v9",
            "params": "{}",
            "result": "{}"
        }"#;
        let tf: ToolFormerData = serde_json::from_str(raw).unwrap();
        assert_eq!(tf.tool, 12345);
        assert_eq!(tf.name, "future_tool_v9");
    }
}
