//! Decoded shapes for an Antigravity (`agy`) conversation.
//!
//! These mirror the real, observed SQLite schema and the subset of the
//! `gemini_coder`/`exa.cortex_pb` protobuf fields toolpath needs — see
//! `crate::proto` and `test-fixtures/antigravity/SOURCE.md` for how each
//! field number was confirmed.

use chrono::{DateTime, Utc};

/// `exa.codeium_common_pb.ChatToolCall` — the model's call that produced
/// a tool step. Fields `{id,name,arguments_json} = {1,2,3}` confirmed
/// against real samples.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallRaw {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

/// `exa.codeium_common_pb.ModelUsageStats`, the subset toolpath reads.
/// `output_tokens` already includes `thinking_output_tokens` (confirmed:
/// `thinking_output_tokens + response_output_tokens == output_tokens` in
/// every sample checked) — unlike `toolpath-gemini`'s older-format
/// quirk, no folding is needed here.
#[derive(Debug, Clone, Default)]
pub struct ModelUsage {
    /// Closed `exa.codeium_common_pb.Model` enum value. Kept raw: we have
    /// no table mapping these ids to model name strings.
    pub model_enum: Option<u64>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub thinking_output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
    pub message_id: Option<String>,
}

/// One decoded row from the `steps` table (one `Step` message per row,
/// `step_payload` column; see `crate::reader`).
#[derive(Debug, Clone, Default)]
pub struct Step {
    /// `steps.idx` — the step's position within the trajectory.
    pub idx: i64,
    /// `Step.type` (field 1): a closed `CortexStepType` enum value. The
    /// *values* observed so far (14 = carries `user_input`, 15 = carries
    /// `planner_response`, 101/132 = seen but undecoded) are not the
    /// doc's field-number table for `Step`'s body oneof — that's a
    /// separate numbering space. We don't have the enum's name table,
    /// so this stays a raw integer.
    pub step_type: i64,
    /// `Step.status` (field 4): `CortexStepStatus`. `3 = DONE` confirmed.
    pub status: i64,
    pub created_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Populated when this step carries `Step.user_input` (field 19).
    /// Read as `field2` (`user_response`) falling back to the first
    /// `items[]` text (field 3) — see `SOURCE.md`: the doc's `query`
    /// (field 1) is empty in every 1.4.8 sample we have.
    pub user_text: Option<String>,
    /// Populated when this step carries `Step.planner_response` (field
    /// 20): `CortexStepPlannerResponse.response` (field 1).
    pub assistant_text: Option<String>,
    /// `CortexStepPlannerResponse.stop_reason` (field 12), raw enum.
    pub stop_reason: Option<i64>,
    /// `CortexStepMetadata.tool_call` (field 4), when present.
    pub tool_call: Option<ToolCallRaw>,
    /// `CortexStepMetadata.model_usage` (field 9), when present.
    pub model_usage: Option<ModelUsage>,
    /// Best-effort tool result text. Only decoded for tool bodies we've
    /// confirmed a field layout for (`read_file`, `Step` field 140 in
    /// 1.4.8 — see `SOURCE.md`); `None` for every other tool, even
    /// though the call itself (`tool_call`) is still populated.
    pub tool_result_text: Option<String>,
}

impl Step {
    pub fn has_tool_call(&self) -> bool {
        self.tool_call.is_some()
    }
}

/// A full conversation: every row of one `conversations/<uuid>.db`,
/// decoded and ordered by `idx`.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub id: String,
    pub steps: Vec<Step>,
}

/// One row of `conversation_summaries.db` — a cache, not authoritative
/// (see `docs/agents/formats/antigravity.md`): a missing row doesn't
/// mean a missing conversation.
#[derive(Debug, Clone, Default)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub title: String,
    pub preview: String,
    pub step_count: i64,
    pub last_modified_time: Option<DateTime<Utc>>,
    /// Raw string from the `status` column (e.g.
    /// `"CASCADE_RUN_STATUS_IDLE"`) — observed values only cover the
    /// idle/finished case so far; kept as the raw string rather than an
    /// enum until more values are seen.
    pub status: String,
}
