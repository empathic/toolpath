//! Project a [`ConversationView`] into an Antigravity (`agy`) conversation
//! store (SQLite with protobuf blobs) — the write half of Bryan Russett's
//! "move a thread into Antigravity" request.
//!
//! Follows `docs/agents/adding-a-projector.md` (the Gemini reference):
//! [`native_name`] reverse-maps [`ToolCategory`] to agy's tool vocabulary;
//! [`AntigravityProjector`] implements `ConversationProjector`, producing
//! an in-memory [`AntigravityStore`]; [`AntigravityStore::write_to`]
//! persists the three stores agy itself keeps in sync (the conversation
//! `.db`, the `conversation_summaries.db` row, and
//! `cache/last_conversations.json`) — see the "Multi-store consistency"
//! pitfall below.
//!
//! ## The three MUSTs (per the guide)
//!
//! 1. **Drop foreign-namespace extras.** `toolpath_convo::Turn` carries no
//!    generic `extra` map as of this writing (checked against #375 and
//!    Gemini's own `project.rs`, which synthesizes from `args`/`result`
//!    rather than reading one) — there is nothing foreign to drop. Agy's
//!    own extras (model enum, message ids, trajectory/cascade ids,
//!    per-call `thought_signature`) never reach `Turn` in the first
//!    place (see `provider.rs`'s doc comment on `thinking_signature`), so
//!    an agy → agy round trip through this crate is necessarily lossy on
//!    those — a declared loss, not a leak.
//! 2. **Remap tool names.** [`resolve_tool_name`]: pass through verbatim
//!    when `tool_category(name)` already recognizes it as one of ours
//!    (same-harness round trips don't churn names); otherwise
//!    `native_name(category, args)`; otherwise keep the foreign name
//!    verbatim (no analog — a declared loss, not a silent drop).
//! 3. **Synthesize required fields.** Every synthesized value is marked
//!    `SYNTHESIZED_*` below with what it stands in for and why it's
//!    unverified pending a live check (step 7 of the guide).
//!
//! ## Pitfalls specific to this harness (not in the general guide)
//!
//! - **Three stores must agree.** `conversations/<uuid>.db`'s filename,
//!   `conversation_summaries.conversation_id`, and
//!   `cache/last_conversations.json`'s value for the target `cwd` must
//!   all name the same id, or `agy --conversation <uuid>` and `agy -c`
//!   disagree about what's resumable. [`AntigravityStore::write_to`]
//!   writes all three from one id.
//! - **The model enum is closed and changes per release.**
//!   `ModelUsageStats.model` is `exa.codeium_common_pb.Model`, a closed
//!   enum we have no name table for (see `types.rs`). This projector
//!   **synthesizes** a value (`SYNTHESIZED_MODEL_ENUM`) observed on a
//!   real 1.4.8 `gemini-3.6-flash-low` run — carried through verbatim
//!   when the source `Turn` is itself an agy turn (round trip), always
//!   synthesized for a foreign source. Whether a stale/wrong enum value
//!   is silently accepted or rejected on load is a live-check item.
//! - **Provider-issued signatures can't be forged.** Agy's
//!   `thought_signature` (on a tool call) and
//!   `CortexStepPlannerResponse.thinking_signature` are opaque,
//!   provider-issued bytes Google's API validates. This projector never
//!   synthesizes them — it **omits** them. A foreign tool call has no
//!   such signature to carry through either way. Whether the model
//!   tolerates a missing signature on injected history, versus rejecting
//!   the call, is a live-check item (see
//!   `docs/agents/formats/antigravity.md`'s Pitfalls section).

use crate::proto;
use crate::provider::tool_category;
use crate::reader::ConvoIO;
use chrono::{DateTime, Utc};
use serde_json::Value;
use toolpath_convo::{
    ConversationProjector, ConversationView, ConvoError, Item, Result, Role, ToolCategory, Turn,
};

// Step-level field numbers, as `reader.rs` reads them (see
// `test-fixtures/antigravity/SOURCE.md`).
mod field {
    pub const STEP_METADATA: u32 = 5;
    pub const STEP_USER_INPUT: u32 = 19;
    pub const STEP_PLANNER_RESPONSE: u32 = 20;
    pub const STEP_TOOL_RESULT: u32 = 140; // confirmed real only for read_file

    pub const METADATA_CREATED_AT: u32 = 1;
    pub const METADATA_TOOL_CALL: u32 = 4;
    pub const METADATA_MODEL_USAGE: u32 = 9;

    pub const TOOL_CALL_ID: u32 = 1;
    pub const TOOL_CALL_NAME: u32 = 2;
    pub const TOOL_CALL_ARGS_JSON: u32 = 3;

    pub const USAGE_MODEL: u32 = 1;
    pub const USAGE_INPUT_TOKENS: u32 = 2;
    pub const USAGE_OUTPUT_TOKENS: u32 = 3;
    pub const USAGE_MESSAGE_ID: u32 = 7;

    pub const USER_INPUT_RESPONSE: u32 = 2;

    pub const PLANNER_RESPONSE_TEXT: u32 = 1;
    pub const PLANNER_RESPONSE_MESSAGE_ID: u32 = 6;
    pub const PLANNER_RESPONSE_STOP_REASON: u32 = 12;

    pub const TOOL_RESULT_ARGS: u32 = 1;
    pub const TOOL_RESULT_ARGS_PATH: u32 = 2;
    pub const TOOL_RESULT_BODY: u32 = 2;
    pub const TOOL_RESULT_TEXT: u32 = 1;
}

/// `CortexStepType` values this crate writes (see `types.rs`'s doc on
/// `Step::step_type` for why these aren't the doc's Step-body field
/// numbers).
mod step_type {
    pub const USER_TURN: i64 = 14;
    pub const GENERATION: i64 = 15;
    pub const TOOL_CALL: i64 = 132;
}

const STATUS_DONE: i64 = 3;

/// Observed on a real `agy` 1.4.8 `gemini-3.6-flash-low` run (see
/// `test-fixtures/antigravity/SOURCE.md`). Synthesized for any `Turn`
/// without its own agy-sourced value — see the module doc's "model enum"
/// pitfall.
pub const SYNTHESIZED_MODEL_ENUM: u64 = 1073;

/// Reverse of [`crate::provider::tool_category`]: agy's native tool name
/// for a toolpath [`ToolCategory`], picked by inspecting `args` the way
/// Gemini's `native_name` does (a category is coarser than a tool name:
/// `FileWrite` covers both `write_file` and `edit_file`).
///
/// `FileSearch` has no observed agy-native analog yet (no sample ever
/// called a search tool) — `None` there is a real gap, not an oversight.
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::FileRead => Some("read_file"),
        ToolCategory::FileWrite => Some(if args.get("old_string").is_some() {
            "edit_file"
        } else {
            "write_file"
        }),
        ToolCategory::FileSearch => None,
        ToolCategory::Shell => Some("run_command"),
        ToolCategory::Network => Some("search_web"),
        ToolCategory::Delegation => Some("invoke_subagent"),
    }
}

/// MUST #2: pass through verbatim when `name` is already one of agy's
/// own tool names; otherwise remap through `category` + `native_name`;
/// otherwise keep the foreign name verbatim (no analog — a declared
/// loss the caller can detect via the returned `bool`, `true` meaning
/// "this call has no agy-native name and was kept as-is").
pub fn resolve_tool_name(
    name: &str,
    category: Option<ToolCategory>,
    args: &Value,
) -> (String, bool) {
    if tool_category(name).is_some() {
        return (name.to_string(), false);
    }
    match category.and_then(|c| native_name(c, args)) {
        Some(n) => (n.to_string(), false),
        None => (name.to_string(), true),
    }
}

/// One encoded `steps` row (SQLite columns `step_type`, `status`,
/// `step_payload`; `idx` is the row's position in
/// [`AntigravityStore::steps`]).
pub struct EncodedStep {
    pub step_type: i64,
    pub status: i64,
    pub payload: Vec<u8>,
}

/// The in-memory result of projecting a [`ConversationView`]. Write it to
/// disk with [`AntigravityStore::write_to`].
pub struct AntigravityStore {
    pub conversation_id: String,
    pub steps: Vec<EncodedStep>,
    /// For the `conversation_summaries` row's `title`/`preview` columns.
    pub title: String,
    pub preview: String,
    /// Foreign-sourced tool calls that had no agy-native analog
    /// (`resolve_tool_name`'s loss flag) — surfaced so a caller can
    /// report them, per the guide's "flag it" instruction for pitfall #5.
    pub unmapped_tool_names: Vec<String>,
}

impl AntigravityStore {
    /// Write all three stores `agy` keeps in sync, rooted at `gemini_dir`
    /// (what `~/.gemini` resolves to — pass a temp dir in tests, never
    /// the real one): the conversation `.db`, a `conversation_summaries`
    /// row, and `cache/last_conversations.json` keyed by `cwd`'s absolute
    /// path. Never touches a path outside `gemini_dir`.
    pub fn write_to(&self, gemini_dir: &std::path::Path, cwd: &str) -> crate::error::Result<()> {
        use rusqlite::Connection;

        let antigravity_dir = gemini_dir.join("antigravity-cli");
        let conversations_dir = antigravity_dir.join("conversations");
        std::fs::create_dir_all(&conversations_dir)?;
        std::fs::create_dir_all(antigravity_dir.join("cache"))?;

        // 1. The conversation store — same schema as a real one (see
        //    SOURCE.md's `.schema` dump).
        let db_path = conversations_dir.join(format!("{}.db", self.conversation_id));
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE `steps` (`idx` integer,`step_type` integer NOT NULL DEFAULT 0,\
             `status` integer NOT NULL DEFAULT 0,`has_subtrajectory` numeric NOT NULL DEFAULT false,\
             `metadata` blob,`error_details` blob,`permissions` blob,`task_details` blob,\
             `render_info` blob,`step_payload` blob,`step_format` integer NOT NULL DEFAULT 0,\
             PRIMARY KEY (`idx`));\
             CREATE TABLE `trajectory_metadata_blob` (`id` text DEFAULT \"main\",`data` blob,\
             PRIMARY KEY (`id`));\
             PRAGMA user_version = 1;",
        )?;
        {
            let mut stmt = conn.prepare(
                "INSERT INTO steps (idx, step_type, status, step_payload) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (idx, step) in self.steps.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    idx as i64,
                    step.step_type,
                    step.status,
                    step.payload
                ])?;
            }
        }
        drop(conn);

        // 2. The summaries cache row.
        let summaries_path = antigravity_dir.join("conversation_summaries.db");
        let sconn = Connection::open(&summaries_path)?;
        sconn.execute_batch(
            "CREATE TABLE IF NOT EXISTS `conversation_summaries` (\
             `conversation_id` text,`title` text NOT NULL DEFAULT \"\",\
             `preview` text NOT NULL DEFAULT \"\",`step_count` integer NOT NULL DEFAULT 0,\
             `last_modified_time` datetime NOT NULL,`workspace_uris` text NOT NULL DEFAULT \"\",\
             `status` text NOT NULL DEFAULT \"\",PRIMARY KEY (`conversation_id`));",
        )?;
        sconn.execute(
            "INSERT OR REPLACE INTO conversation_summaries \
             (conversation_id, title, preview, step_count, last_modified_time, workspace_uris, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'CASCADE_RUN_STATUS_IDLE')",
            rusqlite::params![
                self.conversation_id,
                self.title,
                self.preview,
                self.steps.len() as i64,
                Utc::now().to_rfc3339(),
                cwd,
            ],
        )?;
        drop(sconn);

        // 3. `cache/last_conversations.json`, keyed by the absolute cwd
        //    (SOURCE.md / antigravity.md: `{<abs cwd>: <uuid>}`) — merge
        //    with whatever's already there rather than clobbering other
        //    projects' entries.
        let cache_path = antigravity_dir.join("cache/last_conversations.json");
        let mut map: std::collections::HashMap<String, String> = if cache_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&cache_path)?).unwrap_or_default()
        } else {
            Default::default()
        };
        map.insert(cwd.to_string(), self.conversation_id.clone());
        std::fs::write(&cache_path, serde_json::to_string_pretty(&map)?)?;

        Ok(())
    }

    /// Read this store back through [`ConvoIO`] (the library's own
    /// reader), rooted at `gemini_dir` — the round-trip half of the
    /// guide's step-6 contract test, and library/CLI parity's library
    /// side (step 3): whatever `write_to` wrote, `ConvoIO` must resolve
    /// by the same id, with no separate lookup path.
    pub fn read_back(
        &self,
        gemini_dir: &std::path::Path,
    ) -> crate::error::Result<crate::types::Conversation> {
        let resolver = crate::paths::PathResolver::new().with_gemini_dir(gemini_dir);
        ConvoIO::with_resolver(resolver).read_conversation(&self.conversation_id)
    }
}

/// Config for [`AntigravityProjector::project`]. `conversation_id`
/// defaults to the view's own id (agy ids are UUIDs; a foreign source's
/// id may not be UUID-shaped — pass an explicit one when it isn't).
#[derive(Debug, Clone, Default)]
pub struct AntigravityProjector {
    pub conversation_id: Option<String>,
}

impl AntigravityProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_conversation_id(mut self, id: impl Into<String>) -> Self {
        self.conversation_id = Some(id.into());
        self
    }
}

impl ConversationProjector for AntigravityProjector {
    type Output = AntigravityStore;

    fn project(&self, view: &ConversationView) -> Result<AntigravityStore> {
        self.build(view)
            .map_err(|e| ConvoError::Provider(e.to_string()))
    }
}

impl AntigravityProjector {
    fn build(&self, view: &ConversationView) -> anyhow::Result<AntigravityStore> {
        let conversation_id = self
            .conversation_id
            .clone()
            .unwrap_or_else(|| view.id.clone());
        let mut steps = Vec::new();
        let mut unmapped_tool_names = Vec::new();
        let mut call_seq: u32 = 0;

        for item in &view.items {
            let Item::Turn(turn) = item else { continue };
            match turn.role {
                Role::User => steps.push(user_step(turn)),
                Role::Assistant => {
                    for tu in &turn.tool_uses {
                        call_seq += 1;
                        let (resolved_name, unmapped) =
                            resolve_tool_name(&tu.name, tu.category, &tu.input);
                        if unmapped {
                            unmapped_tool_names.push(tu.name.clone());
                        }
                        let call_id = if tu.id.is_empty() {
                            format!("call_{call_seq}")
                        } else {
                            tu.id.clone()
                        };
                        steps.push(tool_call_step(turn, &call_id, &resolved_name, tu));
                    }
                    if !turn.text.is_empty() || turn.token_usage.is_some() {
                        steps.push(generation_step(turn, &conversation_id));
                    }
                }
                _ => {} // System/Other turns have no agy step shape; dropped (declared loss).
            }
        }

        let title = view
            .turns()
            .find(|t| t.role == Role::User)
            .map(|t| truncate(&t.text, 80))
            .unwrap_or_default();
        let preview = view
            .turns()
            .filter(|t| t.role == Role::Assistant && !t.text.is_empty())
            .last()
            .map(|t| truncate(&t.text, 80))
            .unwrap_or_else(|| title.clone());

        Ok(AntigravityStore {
            conversation_id,
            steps,
            title,
            preview,
            unmapped_tool_names,
        })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn parse_ts(t: &Turn) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&t.timestamp)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn user_step(turn: &Turn) -> EncodedStep {
    let payload = proto::message(|buf| {
        proto::put_bytes_field(
            buf,
            field::STEP_METADATA,
            &proto::message(|m| {
                if let Some(ts) = parse_ts(turn) {
                    proto::put_bytes_field(
                        m,
                        field::METADATA_CREATED_AT,
                        &proto::put_timestamp(ts),
                    );
                }
            }),
        );
        proto::put_bytes_field(
            buf,
            field::STEP_USER_INPUT,
            &proto::message(|m| proto::put_str_field(m, field::USER_INPUT_RESPONSE, &turn.text)),
        );
    });
    EncodedStep {
        step_type: step_type::USER_TURN,
        status: STATUS_DONE,
        payload,
    }
}

fn tool_call_step(
    turn: &Turn,
    call_id: &str,
    resolved_name: &str,
    tu: &toolpath_convo::ToolInvocation,
) -> EncodedStep {
    // MUST #3 (args): preserved verbatim, per the guide's pitfall #5 —
    // only the *name* is remapped, never the argument shape.
    let args_json = serde_json::to_string(&tu.input).unwrap_or_else(|_| "{}".to_string());
    let metadata = proto::message(|m| {
        if let Some(ts) = parse_ts(turn) {
            proto::put_bytes_field(m, field::METADATA_CREATED_AT, &proto::put_timestamp(ts));
        }
        proto::put_bytes_field(
            m,
            field::METADATA_TOOL_CALL,
            &proto::message(|tc| {
                proto::put_str_field(tc, field::TOOL_CALL_ID, call_id);
                proto::put_str_field(tc, field::TOOL_CALL_NAME, resolved_name);
                proto::put_str_field(tc, field::TOOL_CALL_ARGS_JSON, &args_json);
                // thought_signature (field 7 on the real ChatToolCall) is
                // never synthesized — omitted, per the module doc's
                // "signatures" pitfall.
            }),
        );
    });
    let payload = proto::message(|buf| {
        proto::put_bytes_field(buf, field::STEP_METADATA, &metadata);
        // Only read_file's result shape is confirmed real (SOURCE.md);
        // every tool's result is written at the same field anyway so a
        // round trip through *this crate's own reader* is self-
        // consistent — see the module doc.
        if let Some(result) = &tu.result {
            let args_for_result = tool_result_args(resolved_name, &tu.input);
            proto::put_bytes_field(
                buf,
                field::STEP_TOOL_RESULT,
                &proto::message(|tr| {
                    proto::put_bytes_field(tr, field::TOOL_RESULT_ARGS, &args_for_result);
                    proto::put_bytes_field(
                        tr,
                        field::TOOL_RESULT_BODY,
                        &proto::message(|body| {
                            proto::put_str_field(body, field::TOOL_RESULT_TEXT, &result.content)
                        }),
                    );
                }),
            );
        }
    });
    EncodedStep {
        step_type: step_type::TOOL_CALL,
        status: STATUS_DONE,
        payload,
    }
}

fn tool_result_args(resolved_name: &str, input: &Value) -> Vec<u8> {
    if resolved_name == "read_file" {
        let path = input
            .get("Path")
            .or_else(|| input.get("path"))
            .or_else(|| input.get("file_path"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        proto::message(|m| proto::put_str_field(m, field::TOOL_RESULT_ARGS_PATH, path))
    } else {
        Vec::new()
    }
}

fn generation_step(turn: &Turn, conversation_id: &str) -> EncodedStep {
    let message_id = format!("toolpath-synth-{}", turn.id);
    let metadata = proto::message(|m| {
        if let Some(ts) = parse_ts(turn) {
            proto::put_bytes_field(m, field::METADATA_CREATED_AT, &proto::put_timestamp(ts));
        }
        if let Some(usage) = &turn.token_usage {
            proto::put_bytes_field(
                m,
                field::METADATA_MODEL_USAGE,
                &proto::message(|u| {
                    proto::put_varint_field(u, field::USAGE_MODEL, SYNTHESIZED_MODEL_ENUM);
                    if let Some(input_tokens) = usage.input_tokens {
                        proto::put_varint_field(u, field::USAGE_INPUT_TOKENS, input_tokens as u64);
                    }
                    if let Some(output_tokens) = usage.output_tokens {
                        proto::put_varint_field(
                            u,
                            field::USAGE_OUTPUT_TOKENS,
                            output_tokens as u64,
                        );
                    }
                    proto::put_str_field(u, field::USAGE_MESSAGE_ID, &message_id);
                }),
            );
        }
    });
    let planner_response = proto::message(|m| {
        proto::put_str_field(m, field::PLANNER_RESPONSE_TEXT, &turn.text);
        proto::put_str_field(m, field::PLANNER_RESPONSE_MESSAGE_ID, &message_id);
        if let Some(stop) = turn
            .stop_reason
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
        {
            proto::put_varint_field(m, field::PLANNER_RESPONSE_STOP_REASON, stop);
        }
        // thinking_signature (field 14) is never synthesized — see the
        // module doc's "signatures" pitfall.
    });
    let _ = conversation_id; // reserved: real samples carry it at metadata field 20; not yet wired (see report).
    let payload = proto::message(|buf| {
        proto::put_bytes_field(buf, field::STEP_METADATA, &metadata);
        proto::put_bytes_field(buf, field::STEP_PLANNER_RESPONSE, &planner_response);
    });
    EncodedStep {
        step_type: step_type::GENERATION,
        status: STATUS_DONE,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use toolpath_convo::{ProducerInfo, ToolInvocation, ToolResult};

    fn user_turn(id: &str, text: &str) -> Item {
        Item::Turn(Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role: Role::User,
            timestamp: "2026-11-25T00:00:00Z".to_string(),
            text: text.to_string(),
            thinking: None,
            thinking_signature: None,
            tool_uses: Vec::new(),
            model: None,
            provider: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        })
    }

    fn assistant_turn(id: &str, text: &str, tool_uses: Vec<ToolInvocation>) -> Item {
        Item::Turn(Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role: Role::Assistant,
            timestamp: "2026-11-25T00:00:05Z".to_string(),
            text: text.to_string(),
            thinking: None,
            thinking_signature: None,
            tool_uses,
            model: None,
            provider: Some("google".to_string()),
            stop_reason: Some("2".to_string()),
            token_usage: Some(toolpath_convo::TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(10),
                ..Default::default()
            }),
            attributed_token_usage: None,
            environment: None,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        })
    }

    fn view(id: &str, items: Vec<Item>) -> ConversationView {
        ConversationView {
            id: id.to_string(),
            started_at: None,
            last_activity: None,
            items,
            total_usage: None,
            provider_id: Some("test".into()),
            files_changed: vec![],
            session_ids: vec![],
            base: None,
            producer: Some(ProducerInfo {
                name: "test".into(),
                version: None,
            }),
        }
    }

    #[test]
    fn native_name_covers_every_category_but_file_search() {
        let a = json!({});
        assert_eq!(native_name(ToolCategory::FileRead, &a), Some("read_file"));
        assert_eq!(native_name(ToolCategory::Shell, &a), Some("run_command"));
        assert_eq!(native_name(ToolCategory::Network, &a), Some("search_web"));
        assert_eq!(
            native_name(ToolCategory::Delegation, &a),
            Some("invoke_subagent")
        );
        assert_eq!(native_name(ToolCategory::FileSearch, &a), None);
    }

    #[test]
    fn native_name_disambiguates_file_write_by_args() {
        let edit = json!({"old_string": "a", "new_string": "b"});
        let write = json!({"content": "x"});
        assert_eq!(
            native_name(ToolCategory::FileWrite, &edit),
            Some("edit_file")
        );
        assert_eq!(
            native_name(ToolCategory::FileWrite, &write),
            Some("write_file")
        );
    }

    #[test]
    fn resolve_tool_name_passes_through_agys_own_names_verbatim() {
        let (name, unmapped) =
            resolve_tool_name("read_file", Some(ToolCategory::FileRead), &json!({}));
        assert_eq!(name, "read_file");
        assert!(!unmapped);
    }

    #[test]
    fn resolve_tool_name_remaps_a_foreign_name_through_category() {
        // Claude's "Bash" -> agy's "run_command".
        let (name, unmapped) = resolve_tool_name("Bash", Some(ToolCategory::Shell), &json!({}));
        assert_eq!(name, "run_command");
        assert!(!unmapped);
    }

    #[test]
    fn resolve_tool_name_keeps_an_unanalogous_foreign_name_and_flags_it() {
        let (name, unmapped) = resolve_tool_name("SomeMcpTool", None, &json!({}));
        assert_eq!(name, "SomeMcpTool");
        assert!(unmapped);
    }

    #[test]
    fn project_round_trips_text_and_tool_calls_through_the_own_reader() {
        let tool = ToolInvocation {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: json!({"Path": "/tmp/notes.txt"}),
            result: Some(ToolResult {
                content: "the magic number is 42".to_string(),
                is_error: false,
            }),
            category: Some(ToolCategory::FileRead),
        };
        let v = view(
            "test-convo",
            vec![
                user_turn("u1", "Read notes.txt"),
                assistant_turn("a1", "42", vec![tool]),
            ],
        );

        let store = AntigravityProjector::new()
            .with_conversation_id("00000000-0000-0000-0000-000000000001")
            .project(&v)
            .unwrap();
        assert!(store.unmapped_tool_names.is_empty());
        assert_eq!(store.steps.len(), 3); // user, tool_call, generation

        let dir = TempDir::new().unwrap();
        store.write_to(dir.path(), "/tmp/project").unwrap();
        let read_back = store.read_back(dir.path()).unwrap();
        let rebuilt_view = crate::provider::to_view(&read_back);

        let turns: Vec<_> = rebuilt_view
            .items
            .iter()
            .filter_map(Item::as_turn)
            .collect();
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].text, "Read notes.txt");
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[1].text, "42");
        assert_eq!(turns[1].tool_uses.len(), 1);
        assert_eq!(turns[1].tool_uses[0].name, "read_file");
        assert_eq!(
            turns[1].tool_uses[0].result.as_ref().unwrap().content,
            "the magic number is 42"
        );
    }

    #[test]
    fn project_drops_system_turns_as_a_declared_loss() {
        let v = view(
            "test-convo-2",
            vec![Item::Turn(Turn {
                id: "s1".to_string(),
                parent_id: None,
                group_id: None,
                role: Role::System,
                timestamp: String::new(),
                text: "system prompt".to_string(),
                thinking: None,
                thinking_signature: None,
                tool_uses: vec![],
                model: None,
                provider: None,
                stop_reason: None,
                token_usage: None,
                attributed_token_usage: None,
                environment: None,
                delegations: vec![],
                file_mutations: vec![],
            })],
        );
        let store = AntigravityProjector::new().project(&v).unwrap();
        assert!(store.steps.is_empty());
    }

    #[test]
    fn write_to_keeps_three_stores_consistent() {
        let v = view(
            "00000000-0000-0000-0000-0000000000ab",
            vec![user_turn("u1", "hi")],
        );
        let store = AntigravityProjector::new().project(&v).unwrap();
        let dir = TempDir::new().unwrap();
        store.write_to(dir.path(), "/tmp/myproject").unwrap();

        let resolver = crate::paths::PathResolver::new().with_gemini_dir(dir.path());
        let io = crate::reader::ConvoIO::with_resolver(resolver);
        let summaries = io.list_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].conversation_id, store.conversation_id);

        let last = io.last_conversation_for_cwd("/tmp/myproject").unwrap();
        assert_eq!(last, Some(store.conversation_id.clone()));
    }
}
