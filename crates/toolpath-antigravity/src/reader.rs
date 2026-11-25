//! Read-only SQLite access to an `agy` conversation store.
//!
//! Opened `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`, same posture
//! as `toolpath-opencode`'s `DbReader`, so a toolpath read never
//! interferes with a live `agy` process (WAL mode allows concurrent
//! readers alongside the one writer).

use crate::error::{ConvoError, Result};
use crate::proto;
use crate::types::{Conversation, ConversationSummary, ModelUsage, Step, ToolCallRaw};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Field numbers confirmed against real `agy` 1.4.8 samples — see
/// `test-fixtures/antigravity/SOURCE.md`.
mod field {
    // `step_type`/`status` are read directly from their own `steps`
    // columns (see `read_conversation_at`), not re-decoded from field 1
    // / field 4 of `step_payload`, even though both are also mirrored
    // there (confirmed in `SOURCE.md`).
    pub const STEP_METADATA: u32 = 5;
    pub const STEP_USER_INPUT: u32 = 19;
    pub const STEP_PLANNER_RESPONSE: u32 = 20;
    /// Observed only on a `read_file` tool step in our samples; other
    /// tool bodies may use a different field and are left undecoded.
    pub const STEP_READ_FILE_RESULT: u32 = 140;

    pub const METADATA_CREATED_AT: u32 = 1;
    pub const METADATA_TOOL_CALL: u32 = 4;
    pub const METADATA_MODEL_USAGE: u32 = 9;
    pub const METADATA_COMPLETED_AT: u32 = 8;

    pub const TOOL_CALL_ID: u32 = 1;
    pub const TOOL_CALL_NAME: u32 = 2;
    pub const TOOL_CALL_ARGS_JSON: u32 = 3;

    pub const USAGE_MODEL: u32 = 1;
    pub const USAGE_INPUT_TOKENS: u32 = 2;
    pub const USAGE_OUTPUT_TOKENS: u32 = 3;
    pub const USAGE_CACHE_WRITE_TOKENS: u32 = 4;
    pub const USAGE_CACHE_READ_TOKENS: u32 = 5;
    pub const USAGE_MESSAGE_ID: u32 = 7;
    pub const USAGE_THINKING_OUTPUT_TOKENS: u32 = 9;

    pub const USER_INPUT_RESPONSE: u32 = 2;
    pub const USER_INPUT_ITEMS: u32 = 3;
    pub const TEXT_OR_SCOPE_ITEM_TEXT: u32 = 1;

    pub const PLANNER_RESPONSE_TEXT: u32 = 1;
    pub const PLANNER_RESPONSE_STOP_REASON: u32 = 12;

    pub const READ_FILE_RESULT_BODY: u32 = 2;
    pub const READ_FILE_RESULT_TEXT: u32 = 1;
}

#[derive(Debug, Clone)]
pub struct ConvoIO {
    resolver: crate::paths::PathResolver,
}

impl Default for ConvoIO {
    fn default() -> Self {
        Self::new()
    }
}

impl ConvoIO {
    pub fn new() -> Self {
        Self {
            resolver: crate::paths::PathResolver::new(),
        }
    }

    pub fn with_resolver(resolver: crate::paths::PathResolver) -> Self {
        Self { resolver }
    }

    pub fn resolver(&self) -> &crate::paths::PathResolver {
        &self.resolver
    }

    pub fn exists(&self) -> bool {
        self.resolver.exists()
    }

    fn open_ro(path: &Path) -> Result<Connection> {
        if !path.exists() {
            return Err(ConvoError::DatabaseNotFound(path.to_path_buf()));
        }
        Ok(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?)
    }

    /// List conversation ids from the `conversations/` directory
    /// (`<uuid>.db`/`<uuid>.pb` file stems), not the summaries cache —
    /// the summaries db is explicitly a cache that may be missing rows
    /// (`docs/agents/formats/antigravity.md`).
    pub fn list_conversations(&self) -> Result<Vec<String>> {
        let dir = self.resolver.conversations_dir()?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("db") | Some("pb")
                )
            {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Read every row of `conversation_summaries.db`, as a cache (a
    /// missing row there does not mean a missing conversation).
    pub fn list_summaries(&self) -> Result<Vec<ConversationSummary>> {
        let path = self.resolver.summaries_db_path()?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let conn = Self::open_ro(&path)?;
        let mut stmt = conn.prepare(
            "SELECT conversation_id, title, preview, step_count, last_modified_time, status
             FROM conversation_summaries
             ORDER BY last_modified_time DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let last_modified: Option<String> = row.get(4)?;
            Ok(ConversationSummary {
                conversation_id: row.get(0)?,
                title: row.get(1)?,
                preview: row.get(2)?,
                step_count: row.get(3)?,
                last_modified_time: last_modified
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc)),
                status: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Read a full conversation, steps ordered by `idx`.
    pub fn read_conversation(&self, conversation_id: &str) -> Result<Conversation> {
        let path = self.resolver.conversation_path(conversation_id)?;
        self.read_conversation_at(conversation_id, &path)
    }

    /// Read a conversation from an explicit `.db` path (used by tests
    /// against fixtures that don't sit under a resolved `~/.gemini`).
    pub fn read_conversation_at(&self, conversation_id: &str, path: &Path) -> Result<Conversation> {
        let conn = Self::open_ro(path)?;
        let mut stmt =
            conn.prepare("SELECT idx, step_type, status, step_payload FROM steps ORDER BY idx")?;
        let rows = stmt.query_map([], |row| {
            let idx: i64 = row.get(0)?;
            let step_type: i64 = row.get(1)?;
            let status: i64 = row.get(2)?;
            let payload: Vec<u8> = row.get(3)?;
            Ok((idx, step_type, status, payload))
        })?;

        let mut steps = Vec::new();
        for r in rows {
            let (idx, step_type, status, payload) = r?;
            steps.push(decode_step(idx, step_type, status, &payload));
        }

        Ok(Conversation {
            id: conversation_id.to_string(),
            steps,
        })
    }

    pub fn last_conversation_for_cwd(&self, cwd: &str) -> Result<Option<String>> {
        let path = self.resolver.last_conversations_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let map: std::collections::HashMap<String, String> = serde_json::from_str(&content)?;
        Ok(map.get(cwd).cloned())
    }
}

/// Decode one `steps` row into a [`Step`]. Never panics on an
/// unrecognized or malformed payload — unknown fields are simply
/// absent from the result, matching the closed-binary, no-schema
/// reality of this store (see `crate::proto`).
fn decode_step(idx: i64, step_type: i64, status: i64, payload: &[u8]) -> Step {
    let mut step = Step {
        idx,
        step_type,
        status,
        ..Default::default()
    };

    let Some(metadata) = proto::first_message(payload, field::STEP_METADATA) else {
        apply_user_input(&mut step, payload);
        apply_planner_response(&mut step, payload);
        apply_tool_result(&mut step, payload);
        return step;
    };

    step.created_at =
        proto::first_message(metadata, field::METADATA_CREATED_AT).and_then(proto::timestamp);
    step.completed_at =
        proto::first_message(metadata, field::METADATA_COMPLETED_AT).and_then(proto::timestamp);

    if let Some(tc) = proto::first_message(metadata, field::METADATA_TOOL_CALL) {
        step.tool_call = Some(ToolCallRaw {
            id: proto::first_str(tc, field::TOOL_CALL_ID)
                .unwrap_or_default()
                .to_string(),
            name: proto::first_str(tc, field::TOOL_CALL_NAME)
                .unwrap_or_default()
                .to_string(),
            arguments_json: proto::first_str(tc, field::TOOL_CALL_ARGS_JSON)
                .unwrap_or_default()
                .to_string(),
        });
    }

    if let Some(usage) = proto::first_message(metadata, field::METADATA_MODEL_USAGE) {
        step.model_usage = Some(ModelUsage {
            model_enum: proto::first_u64(usage, field::USAGE_MODEL),
            input_tokens: proto::first_u64(usage, field::USAGE_INPUT_TOKENS).map(|v| v as u32),
            output_tokens: proto::first_u64(usage, field::USAGE_OUTPUT_TOKENS).map(|v| v as u32),
            thinking_output_tokens: proto::first_u64(usage, field::USAGE_THINKING_OUTPUT_TOKENS)
                .map(|v| v as u32),
            cache_read_tokens: proto::first_u64(usage, field::USAGE_CACHE_READ_TOKENS)
                .map(|v| v as u32),
            cache_write_tokens: proto::first_u64(usage, field::USAGE_CACHE_WRITE_TOKENS)
                .map(|v| v as u32),
            message_id: proto::first_str(usage, field::USAGE_MESSAGE_ID).map(|s| s.to_string()),
        });
    }

    apply_user_input(&mut step, payload);
    apply_planner_response(&mut step, payload);
    apply_tool_result(&mut step, payload);
    step
}

fn apply_user_input(step: &mut Step, payload: &[u8]) {
    let Some(ui) = proto::first_message(payload, field::STEP_USER_INPUT) else {
        return;
    };
    if let Some(s) = proto::first_str(ui, field::USER_INPUT_RESPONSE)
        && !s.is_empty()
    {
        step.user_text = Some(s.to_string());
        return;
    }
    // Fall back to the first `items[]` (`TextOrScopeItem`) text.
    for item in proto::all_messages(ui, field::USER_INPUT_ITEMS) {
        if let Some(s) = proto::first_str(item, field::TEXT_OR_SCOPE_ITEM_TEXT)
            && !s.is_empty()
        {
            step.user_text = Some(s.to_string());
            return;
        }
    }
}

fn apply_planner_response(step: &mut Step, payload: &[u8]) {
    let Some(pr) = proto::first_message(payload, field::STEP_PLANNER_RESPONSE) else {
        return;
    };
    if let Some(s) = proto::first_str(pr, field::PLANNER_RESPONSE_TEXT) {
        step.assistant_text = Some(s.to_string());
    }
    step.stop_reason = proto::first_u64(pr, field::PLANNER_RESPONSE_STOP_REASON).map(|v| v as i64);
}

fn apply_tool_result(step: &mut Step, payload: &[u8]) {
    let Some(body) = proto::first_message(payload, field::STEP_READ_FILE_RESULT) else {
        return;
    };
    let Some(result) = proto::first_message(body, field::READ_FILE_RESULT_BODY) else {
        return;
    };
    if let Some(s) = proto::first_str(result, field::READ_FILE_RESULT_TEXT) {
        step.tool_result_text = Some(s.to_string());
    }
}

/// Convenience: a conversation's `.db`/`.pb` path given an explicit
/// directory (used by tests against fixtures copied flat, without the
/// rest of the `~/.gemini` tree around them).
pub fn conversation_path_in(dir: &Path, conversation_id: &str) -> PathBuf {
    let db = dir.join(format!("{conversation_id}.db"));
    if db.exists() {
        return db;
    }
    dir.join(format!("{conversation_id}.pb"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_conversations_empty_when_dir_missing() {
        let resolver = crate::paths::PathResolver::new().with_gemini_dir("/never/exists/.gemini");
        let io = ConvoIO::with_resolver(resolver);
        assert_eq!(io.list_conversations().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn last_conversation_for_cwd_none_when_cache_missing() {
        let resolver = crate::paths::PathResolver::new().with_gemini_dir("/never/exists/.gemini");
        let io = ConvoIO::with_resolver(resolver);
        assert_eq!(io.last_conversation_for_cwd("/some/cwd").unwrap(), None);
    }
}
