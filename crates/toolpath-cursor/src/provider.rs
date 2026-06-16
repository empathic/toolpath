//! Implementation of `toolpath-convo` traits for Cursor sessions.
//!
//! Cursor's bubble store is already linear (one bubble per UI message,
//! ordered by `fullConversationHeadersOnly`), so the mapping per turn
//! is direct:
//!
//! - User bubble (`type: 1`) → [`Turn`] with `role: User`, `text`
//!   from `bubble.text`.
//! - Assistant text/thinking/tool bubble (`type: 2`) → [`Turn`] with
//!   `role: Assistant`. The bubble's `text`, `allThinkingBlocks`,
//!   and `toolFormerData` populate `text`, `thinking`, and a single
//!   `tool_uses[0]` respectively (Cursor stores at most one tool
//!   call per bubble in observed data; multi-tool turns fan out to
//!   multiple bubbles with the same `requestId`).
//!
//! File mutations are resolved at view-build time: when a tool bubble
//! carries `edit_file_v2` with `beforeContentId`/`afterContentId`, the
//! provider looks the content blobs up from
//! [`CursorSession::content_blobs`] and emits a [`FileMutation`] with
//! `before`, `after`, and a unified diff computed via
//! [`toolpath_convo::unified_diff`].
//!
//! Provider-specific UI metadata (Cursor's per-bubble `checkpointId`,
//! `requestId`, `richText`, capability flags, the
//! `anysphere.cursor-commits` checkpoint store, …) is **not** smuggled
//! through the IR. The IR has no `Turn.extra`; round-tripping a Cursor
//! session through `ConversationView` drops those fields by design.
//! The bubble store on disk is the source of truth for anything that
//! needs them.

use serde_json::Value;

use crate::error::Result;
use crate::io::CursorIO;
use crate::paths::PathResolver;
use crate::reader::CONTENT_PREFIX;
use crate::types::{
    Bubble, CursorSession, CursorSessionMetadata, ToolFormerData, BUBBLE_TYPE_ASSISTANT,
    BUBBLE_TYPE_USER, TOOL_EDIT_FILE_V2, TOOL_RUN_TERMINAL_COMMAND_V2, tool_name_for_id,
};
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError as ConvoTraitError,
    EnvironmentSnapshot, FileMutation, ProducerInfo, Role, SessionBase, TokenUsage, ToolCategory,
    ToolInvocation, ToolResult, Turn, unified_diff,
};

/// The dispatch family used in `path.meta.source` and
/// `ConversationView.provider_id`.
pub const PROVIDER_ID: &str = "cursor";

/// Provider for Cursor sessions.
#[derive(Default)]
pub struct CursorConvo {
    io: CursorIO,
}

impl CursorConvo {
    pub fn new() -> Self {
        Self { io: CursorIO::new() }
    }

    pub fn with_resolver(resolver: PathResolver) -> Self {
        Self {
            io: CursorIO::with_resolver(resolver),
        }
    }

    pub fn io(&self) -> &CursorIO {
        &self.io
    }

    pub fn resolver(&self) -> &PathResolver {
        self.io.resolver()
    }

    pub fn read_session(&self, composer_id: &str) -> Result<CursorSession> {
        self.io.read_session(composer_id)
    }

    pub fn list_sessions(&self) -> Result<Vec<CursorSessionMetadata>> {
        self.io.list_session_metadata()
    }

    pub fn most_recent_session(&self) -> Result<Option<CursorSession>> {
        let metas = self.list_sessions()?;
        // list_session_metadata follows the header order, which Cursor
        // maintains roughly newest-first. Fall back to last_activity
        // when present so this stays right even if the header order
        // ever drifts.
        let pick = metas
            .iter()
            .max_by_key(|m| m.last_activity.unwrap_or_else(chrono::DateTime::<chrono::Utc>::default));
        match pick {
            Some(m) => Ok(Some(self.read_session(&m.id)?)),
            None => Ok(None),
        }
    }

    /// Read every session that has bubbles on disk. Expensive on
    /// large histories.
    pub fn read_all_sessions(&self) -> Result<Vec<CursorSession>> {
        let metas = self.list_sessions()?;
        let mut out = Vec::with_capacity(metas.len());
        for m in metas {
            match self.read_session(&m.id) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("Warning: could not read composer {}: {}", m.id, e),
            }
        }
        Ok(out)
    }
}

// ── Tool classification ────────────────────────────────────────────────

/// Map a Cursor tool to toolpath's category ontology. `tool` is the
/// numeric id from `toolFormerData.tool`; `name` is the internal tool
/// name. Both are needed because Anysphere has historically renamed
/// tools while keeping ids stable and vice versa.
/// Classify a Cursor tool into [`ToolCategory`].
///
/// The mapping covers every tool name observed in:
/// - the 71 `aiserver.v1.<Name>Params` protobuf messages embedded in
///   Cursor.app's workbench bundle
///   (`Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js`),
/// - the 47 `*ToolCall` discriminators in the same bundle's switch
///   statements (UI-side aliases and control-flow markers), and
/// - the agent-side JSONL transcript vocabulary (`Shell`, `Read`,
///   `Write`, `StrReplace`, `Glob`, `Grep`, `Task`).
///
/// Numeric ids only fire for ones we've **observed in the wild**
/// (`TOOL_*` consts in [`crate::types`]). Anysphere assigns these
/// at the cloud-side; the workbench bundle doesn't expose the full
/// enum literally and we'd rather miss a numeric id than guess one
/// wrong. Tools whose id we haven't seen still classify by `name`.
///
/// Tools that have no clean fit in toolpath's category ontology
/// (planning/`todo_*`, MCP machinery, UI control flow like `await` /
/// `truncated` / `end`, reporting tools, VCS read-only queries,
/// screenshot/diagram/image generation) return `None`. Consumers
/// still see the `name` + `input` on the `ToolInvocation`.
pub fn tool_category(tool: u32, name: &str) -> Option<ToolCategory> {
    // Resolve the tool by id when possible — the canonical `tool`
    // value beats whatever name the bubble carries (names can drift
    // between releases; ids are the cloud-server enum and survive
    // renames). But when the id is `0` (UNSPECIFIED) or otherwise
    // canonical-but-uncategorized, the literal `name` still might
    // classify (foreign provider names, JSONL-friendly aliases),
    // so we try it as a fallback rather than short-circuiting to
    // `None`.
    if let Some(canonical) = tool_name_for_id(tool)
        && let Some(cat) = category_by_name(canonical)
    {
        return Some(cat);
    }
    category_by_name(name)
}

fn category_by_name(name: &str) -> Option<ToolCategory> {
    match name {
        // ── Shell ────────────────────────────────────────────────
        // `RunTerminalCommandV2` (observed id 15), `RunTerminalCommands`,
        // `RunTest`, plus the JSONL-side friendly `Shell`/`shell` and
        // the legacy `run_terminal_cmd`. `WriteShellStdin` writes to
        // an already-running shell session so it's shell-category too.
        "run_terminal_command_v2"
        | "run_terminal_commands"
        | "run_terminal_cmd"
        | "run_test"
        | "write_shell_stdin"
        | "Shell"
        | "shell" => Some(ToolCategory::Shell),

        // ── FileWrite ────────────────────────────────────────────
        // Includes legacy v1 (`edit_file`, `new_file`, `new_edit`),
        // current v2 (`edit_file_v2`), test-mgmt (`add_test`/`delete_test`),
        // lint-fix (`fix_lints`/`fix_lints_subagent`), bulk file ops
        // (`create_rm_files`, `save_file`, `undo_edit`, `apply_agent_diff`,
        // `reapply`), and the agent-side friendly aliases
        // `Write`/`StrReplace`/`edit`/`delete`/`Edit`.
        "edit_file_v2"
        | "edit_file"
        | "edit"
        | "Edit"
        | "Write"
        | "StrReplace"
        | "delete_file"
        | "delete"
        | "new_edit"
        | "new_file"
        | "save_file"
        | "reapply"
        | "undo_edit"
        | "apply_agent_diff"
        | "create_rm_files"
        | "add_test"
        | "delete_test"
        | "fix_lints"
        | "fix_lints_subagent" => Some(ToolCategory::FileWrite),

        // ── FileRead ─────────────────────────────────────────────
        // Includes legacy v1 (`read_file`, `list_dir`), current v2
        // (`read_file_v2`, `list_dir_v2`), partial reads (`read_chunk`),
        // project-level (`read_project`, `get_project_structure`),
        // code-intel (`get_symbols`, `gotodef`, `summarize_code`),
        // test discovery (`get_tests`), lint reading
        // (`read_lints`, `read_with_linter`), semsearch hydration
        // (`read_semsearch_files`), the friendly aliases
        // `Read`/`read`/`ls`, and the read-only PR query
        // `blame_by_file_path`.
        "read_file_v2"
        | "read_file"
        | "read"
        | "Read"
        | "read_chunk"
        | "list_dir"
        | "list_dir_v2"
        | "ls"
        | "read_project"
        | "get_project_structure"
        | "get_symbols"
        | "get_tests"
        | "gotodef"
        | "summarize_code"
        | "read_lints"
        | "read_with_linter"
        | "read_semsearch_files"
        | "blame_by_file_path" => Some(ToolCategory::FileRead),

        // ── FileSearch ───────────────────────────────────────────
        // Glob/grep family, semantic search variants, and the
        // discriminator-level alias `sem_search`.
        "glob_file_search"
        | "Glob"
        | "glob"
        | "ripgrep_raw_search"
        | "ripgrep_search"
        | "grep_search"
        | "grep"
        | "Grep"
        | "search"
        | "search_symbols"
        | "semantic_search"
        | "semantic_search_full"
        | "sem_search"
        | "deep_search"
        | "deep_search_subagent"
        | "tool_call_file_search" => Some(ToolCategory::FileSearch),

        // ── Network ──────────────────────────────────────────────
        // Network covers everything that talks to a remote service
        // outside the local fs / shell: web fetch + search, GitHub
        // PR retrieval, and MCP tool dispatch (`call_mcp_tool`
        // proxies a model-driven call to a remote MCP server).
        "web_search"
        | "web_fetch"
        | "fetch_pull_request"
        | "fetch"
        | "call_mcp_tool" => Some(ToolCategory::Network),

        // ── Delegation ───────────────────────────────────────────
        // `Task`/`task_v2` is the dispatch primitive. `TaskSubagent`,
        // `SpecSubagent`, `BackgroundComposerFollowup`,
        // `StartGrindExecution`/`StartGrindPlanning` (background-
        // agent "grind" mode) all spin up sub-work.
        "task_v2"
        | "task"
        | "Task"
        | "task_subagent"
        | "spec_subagent"
        | "background_composer_followup"
        | "start_grind_execution"
        | "start_grind_planning" => Some(ToolCategory::Delegation),

        // ── Uncategorized — no IR slot ──────────────────────────
        // Planning / todos: create_plan, todo_read, todo_write,
        //   read_todos, update_todos.
        // MCP control plane: get_mcp_tools, list_mcp_resources,
        //   read_mcp_resource, mcp, mcp_auth.
        // UI / control flow: ask_question, communicate_update,
        //   send_final_summary, switch_mode, set_run, await,
        //   await_task, end, partial, truncated, reflect, add_ui_step.
        // Reporting: report_bug, report_bugfix_results,
        //   record_ci_investigation_findings, ai_attribution.
        // VCS write: set_active_branch, edit_pr_labels,
        //   update_pr_code_tour, pr_management.
        // Misc: knowledge_base, fetch_rules, update_project,
        //   replace_env, setup_vm_environment, generate_image,
        //   computer_use, record_screen, create_diagram.
        _ => None,
    }
}

// ── Session → ConversationView ─────────────────────────────────────────

/// Convert a Cursor [`CursorSession`] into the provider-agnostic
/// [`ConversationView`].
pub fn session_to_view(session: &CursorSession) -> ConversationView {
    Builder::new(session).build()
}

struct Builder<'a> {
    session: &'a CursorSession,
    turns: Vec<Turn>,
    files_changed_order: Vec<String>,
    files_changed_seen: std::collections::HashSet<String>,
    total_usage: TokenUsage,
    total_usage_set: bool,
}

impl<'a> Builder<'a> {
    fn new(session: &'a CursorSession) -> Self {
        Self {
            session,
            turns: Vec::new(),
            files_changed_order: Vec::new(),
            files_changed_seen: std::collections::HashSet::new(),
            total_usage: TokenUsage::default(),
            total_usage_set: false,
        }
    }

    fn build(mut self) -> ConversationView {
        let mut prev_turn_id: Option<String> = None;
        for bubble in &self.session.bubbles {
            let turn = match bubble.kind {
                BUBBLE_TYPE_USER => self.user_turn(bubble, prev_turn_id.as_deref()),
                BUBBLE_TYPE_ASSISTANT => self.assistant_turn(bubble, prev_turn_id.as_deref()),
                // Unknown bubble kind — skip silently. A new Anysphere
                // bubble kind would land here; we'd rather emit a
                // shorter conversation than break the parse.
                _ => continue,
            };
            prev_turn_id = Some(turn.id.clone());
            self.turns.push(turn);
        }

        let started_at = self
            .session
            .started_at()
            .or_else(|| {
                self.session
                    .bubbles
                    .first()
                    .and_then(|b| b.created_at_utc())
            });
        let last_activity = self
            .session
            .last_activity()
            .or_else(|| {
                self.session
                    .bubbles
                    .last()
                    .and_then(|b| b.created_at_utc())
            });

        ConversationView {
            id: self.session.id().to_string(),
            started_at,
            last_activity,
            turns: self.turns,
            total_usage: if self.total_usage_set {
                Some(self.total_usage)
            } else {
                None
            },
            provider_id: Some(PROVIDER_ID.to_string()),
            files_changed: self.files_changed_order,
            session_ids: vec![self.session.id().to_string()],
            events: Vec::new(),
            base: Some(SessionBase {
                working_dir: self
                    .session
                    .workspace_path()
                    .map(|p| p.to_string_lossy().into_owned()),
                vcs_revision: None,
                vcs_branch: None,
                vcs_remote: None,
            }),
            producer: Some(ProducerInfo {
                name: PROVIDER_ID.into(),
                version: self.session.data.agent_backend.clone(),
            }),
        }
    }

    fn user_turn(&self, bubble: &Bubble, parent: Option<&str>) -> Turn {
        let environment = self.environment();
        Turn {
            id: bubble.bubble_id.clone(),
            parent_id: parent.map(str::to_string),
            group_id: None,
            role: Role::User,
            timestamp: bubble.created_at.clone().unwrap_or_default(),
            text: bubble.text.clone(),
            thinking: None,
            tool_uses: Vec::new(),
            model: None,
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment,
            delegations: Vec::new(),
            file_mutations: Vec::new(),
        }
    }

    fn assistant_turn(&mut self, bubble: &Bubble, parent: Option<&str>) -> Turn {
        let environment = self.environment();

        // Thinking: join `allThinkingBlocks[*].text`. Empty when Cursor
        // recorded a thinking-duration on the header but no body
        // (common in observed sessions).
        let thinking_chunks: Vec<String> = bubble
            .all_thinking_blocks
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| t.text.clone())
            .collect();
        let thinking = if thinking_chunks.is_empty() {
            None
        } else {
            Some(thinking_chunks.join("\n\n"))
        };

        // Tool call (at most one per bubble in observed data).
        let mut tool_uses = Vec::new();
        let mut file_mutations: Vec<FileMutation> = Vec::new();
        if let Some(tf) = &bubble.tool_former_data {
            let invocation = self.to_invocation(tf);
            if invocation.category == Some(ToolCategory::FileWrite)
                && let Some(fm) = self.file_mutation_for_edit(tf, &invocation.id)
            {
                if self.files_changed_seen.insert(fm.path.clone()) {
                    self.files_changed_order.push(fm.path.clone());
                }
                file_mutations.push(fm);
            }
            tool_uses.push(invocation);
        }

        // Token usage (per-bubble snapshot — only populated when the
        // server returned non-zero counts).
        let token_usage = bubble.token_count.as_ref().and_then(|t| {
            let u = TokenUsage {
                input_tokens: t.input_tokens.map(|n| n as u32).filter(|n| *n > 0),
                output_tokens: t.output_tokens.map(|n| n as u32).filter(|n| *n > 0),
                cache_read_tokens: None,
                cache_write_tokens: None,
            };
            if u.input_tokens.is_none() && u.output_tokens.is_none() {
                None
            } else {
                Some(u)
            }
        });
        if let Some(u) = &token_usage {
            accumulate(&mut self.total_usage, u);
            self.total_usage_set = true;
        }

        // Model: per-bubble modelInfo wins, then composer default.
        let model = bubble
            .model_info
            .as_ref()
            .and_then(|m| m.model_name.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| self.session.data.default_model().map(str::to_string));

        Turn {
            id: bubble.bubble_id.clone(),
            parent_id: parent.map(str::to_string),
            group_id: None,
            role: Role::Assistant,
            timestamp: bubble.created_at.clone().unwrap_or_default(),
            text: bubble.text.clone(),
            thinking,
            tool_uses,
            model,
            stop_reason: None,
            token_usage,
            attributed_token_usage: None,
            environment,
            delegations: Vec::new(),
            file_mutations,
        }
    }

    fn to_invocation(&self, tf: &ToolFormerData) -> ToolInvocation {
        let mut input = tf.parse_params().unwrap_or(Value::Null);
        if tf.tool == TOOL_EDIT_FILE_V2
            && let Value::Object(obj) = &mut input
        {
            if let Some(path) = obj
                .remove("relativeWorkspacePath")
                .and_then(|v| v.as_str().map(str::to_string))
            {
                obj.insert("file_path".into(), Value::String(path));
            }
            if let Ok(Some(result)) = tf.parse_result() {
                let before = blob_hash(&result, "beforeContentId")
                    .as_deref()
                    .and_then(|h| self.session.content_blob(h));
                let after = blob_hash(&result, "afterContentId")
                    .as_deref()
                    .and_then(|h| self.session.content_blob(h));
                match (before, after) {
                    (Some(b), Some(a)) if !b.is_empty() && b != a => {
                        obj.insert("old_string".into(), Value::String(b.to_string()));
                        obj.insert("new_string".into(), Value::String(a.to_string()));
                    }
                    (_, Some(a)) => {
                        obj.insert("content".into(), Value::String(a.to_string()));
                    }
                    _ => {}
                }
            }
        }
        let result = match (tf.is_completed(), tf.parse_result().ok().flatten()) {
            (true, Some(v)) => Some(ToolResult {
                content: result_to_text(tf, &v),
                is_error: false,
            }),
            // Server may emit a result on `running` too; treat any
            // present non-null result as the tool's output.
            (false, Some(v)) if !tf.is_error() => Some(ToolResult {
                content: result_to_text(tf, &v),
                is_error: false,
            }),
            (false, _) if tf.is_error() => Some(ToolResult {
                content: format!("tool {} errored", tf.name),
                is_error: true,
            }),
            _ => None,
        };
        ToolInvocation {
            id: tf.tool_call_id.clone(),
            name: tf.name.clone(),
            input,
            result,
            category: tool_category(tf.tool, &tf.name),
        }
    }

    fn file_mutation_for_edit(
        &self,
        tf: &ToolFormerData,
        tool_id: &str,
    ) -> Option<FileMutation> {
        let params = tf.parse_params().ok()?;
        let path = params
            .get("relativeWorkspacePath")
            .or_else(|| params.get("path"))
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())?;

        let result = tf.parse_result().ok().flatten()?;
        let before_hash = blob_hash(&result, "beforeContentId");
        let after_hash = blob_hash(&result, "afterContentId");

        let before = before_hash
            .as_deref()
            .and_then(|h| self.session.content_blob(h))
            .map(str::to_string);
        let after = after_hash
            .as_deref()
            .and_then(|h| self.session.content_blob(h))
            .map(str::to_string);

        let operation = match (before.as_deref(), after.as_deref()) {
            (None, _) => "update",
            (Some(b), Some(a)) if b.is_empty() && !a.is_empty() => "add",
            (Some(_), Some(_)) => "update",
            (Some(_), None) => "delete",
        };

        let raw_diff = match (&before, &after) {
            (Some(b), Some(a)) if b != a => Some(unified_diff(path, b, a)),
            _ => None,
        };

        Some(FileMutation {
            path: path.to_string(),
            tool_id: Some(tool_id.to_string()),
            operation: Some(operation.to_string()),
            raw_diff,
            before,
            after,
            rename_to: None,
        })
    }

    fn environment(&self) -> Option<EnvironmentSnapshot> {
        let wd = self
            .session
            .workspace_path()?
            .to_string_lossy()
            .into_owned();
        Some(EnvironmentSnapshot {
            working_dir: Some(wd),
            vcs_branch: None,
            vcs_revision: None,
        })
    }
}

/// Render a tool result `Value` to the text the model would have
/// seen. For shell commands that's the stdout/stderr stream; for
/// edits it's the descriptive summary; everything else falls back
/// to the JSON serialization, with a special case for the
/// `{"output": "..."}` envelope our own projector wraps non-native
/// results in (keeps `IR → cursor → IR` symmetric for unknown tools).
fn result_to_text(tf: &ToolFormerData, v: &Value) -> String {
    match tf.tool {
        TOOL_RUN_TERMINAL_COMMAND_V2 => v
            .get("output")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(v).unwrap_or_default()),
        TOOL_EDIT_FILE_V2 => {
            // The literal result is `{beforeContentId, afterContentId}` —
            // the model sees a synthesized "OK, edited X" line, not these
            // ids. We surface a deterministic summary so consumers
            // (audit, diff inspection) get something readable while the
            // structured file mutation carries the real payload.
            let path = tf
                .parse_params()
                .ok()
                .and_then(|p| {
                    p.get("relativeWorkspacePath")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            match path {
                Some(p) => format!("edited {p}"),
                None => "edited file".into(),
            }
        }
        _ => {
            // If the result is the single-field envelope
            // `{"output": "..."}` (the shape our projector emits for
            // tools without a native Cursor wire shape), unwrap it so
            // the round-trip is byte-symmetric. Otherwise hand back
            // the raw JSON — that's what the model would have seen
            // for native Cursor wire shapes like
            // `{"directories":[…]}` from `glob_file_search`.
            if let Value::Object(map) = v
                && map.len() == 1
                && let Some(Value::String(s)) = map.get("output")
            {
                return s.clone();
            }
            serde_json::to_string(v).unwrap_or_default()
        }
    }
}

fn blob_hash(v: &Value, field: &str) -> Option<String> {
    let raw = v.get(field)?.as_str()?;
    raw.strip_prefix(CONTENT_PREFIX).map(str::to_string)
}

fn accumulate(total: &mut TokenUsage, delta: &TokenUsage) {
    if let Some(n) = delta.input_tokens {
        *total.input_tokens.get_or_insert(0) += n;
    }
    if let Some(n) = delta.output_tokens {
        *total.output_tokens.get_or_insert(0) += n;
    }
    if let Some(n) = delta.cache_read_tokens {
        *total.cache_read_tokens.get_or_insert(0) += n;
    }
    if let Some(n) = delta.cache_write_tokens {
        *total.cache_write_tokens.get_or_insert(0) += n;
    }
}

// ── ConversationProvider impl ──────────────────────────────────────────

impl ConversationProvider for CursorConvo {
    fn list_conversations(&self, _project: &str) -> toolpath_convo::Result<Vec<String>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas.into_iter().map(|m| m.id).collect())
    }

    fn load_conversation(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let s = self
            .read_session(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(session_to_view(&s))
    }

    fn load_metadata(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let m = self
            .io
            .read_session_metadata(conversation_id)
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: m.id,
            started_at: m.started_at,
            last_activity: m.last_activity,
            message_count: m.message_count,
            file_path: m.workspace_path,
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, _project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_sessions()
            .map_err(|e| ConvoTraitError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.id,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: m.workspace_path,
                predecessor: None,
                successor: None,
            })
            .collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::tests::{BASIC_FIXTURE, fixture_db};
    use chrono::{TimeZone, Utc};
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, CursorConvo) {
        let temp = TempDir::new().unwrap();
        let user_data = temp.path().join("UserData");
        let global = user_data.join("User").join("globalStorage");
        fs::create_dir_all(&global).unwrap();
        let src = fixture_db(BASIC_FIXTURE);
        fs::copy(src.path(), global.join("state.vscdb")).unwrap();
        let resolver = PathResolver::new()
            .with_home(temp.path())
            .with_anysphere_dir(temp.path().join(".cursor"))
            .with_user_data_dir(user_data);
        (temp, CursorConvo::with_resolver(resolver))
    }

    #[test]
    fn basic_view_shape() {
        let (_t, mgr) = setup();
        let session = mgr.read_session("c1").unwrap();
        let view = session_to_view(&session);

        assert_eq!(view.id, "c1");
        assert_eq!(view.provider_id.as_deref(), Some("cursor"));
        assert_eq!(view.turns.len(), 3);

        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "hello");

        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "hi back");
        assert_eq!(view.turns[1].model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(view.turns[1].token_usage.as_ref().unwrap().input_tokens, Some(10));

        assert_eq!(view.turns[2].role, Role::Assistant);
        assert_eq!(view.turns[2].tool_uses.len(), 1);
        assert_eq!(view.turns[2].tool_uses[0].name, "edit_file_v2");
        assert_eq!(
            view.turns[2].tool_uses[0].category,
            Some(ToolCategory::FileWrite)
        );
    }

    #[test]
    fn base_and_producer_populated() {
        let (_t, mgr) = setup();
        let view = session_to_view(&mgr.read_session("c1").unwrap());
        let base = view.base.as_ref().unwrap();
        assert_eq!(base.working_dir.as_deref(), Some("/p"));
        let producer = view.producer.as_ref().unwrap();
        assert_eq!(producer.name, "cursor");
        assert_eq!(producer.version.as_deref(), Some("cursor-agent"));
    }

    #[test]
    fn file_mutation_populated_with_diff() {
        let (_t, mgr) = setup();
        let view = session_to_view(&mgr.read_session("c1").unwrap());
        let edit_turn = &view.turns[2];
        assert_eq!(edit_turn.file_mutations.len(), 1);
        let fm = &edit_turn.file_mutations[0];
        assert_eq!(fm.path, "/p/x.rs");
        assert_eq!(fm.operation.as_deref(), Some("add"));
        assert_eq!(fm.before.as_deref(), Some(""));
        assert_eq!(fm.after.as_deref(), Some("fn main() {}"));
        assert!(fm.raw_diff.as_ref().unwrap().contains("+fn main() {}"));
        assert_eq!(fm.tool_id.as_deref(), Some("tool_a"));
    }

    #[test]
    fn files_changed_deduped_in_first_touch_order() {
        let (_t, mgr) = setup();
        let view = session_to_view(&mgr.read_session("c1").unwrap());
        assert_eq!(view.files_changed, vec!["/p/x.rs"]);
    }

    #[test]
    fn parent_id_chains_turns() {
        let (_t, mgr) = setup();
        let view = session_to_view(&mgr.read_session("c1").unwrap());
        assert!(view.turns[0].parent_id.is_none());
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("u1"));
        assert_eq!(view.turns[2].parent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn shell_tool_result_carries_output_and_exit_code() {
        let setup_sql = r#"
            INSERT INTO cursorDiskKV (key, value) VALUES
              ('composerData:cs', '{"_v":16,"composerId":"cs","fullConversationHeadersOnly":[{"bubbleId":"b1","type":2}]}'),
              ('bubbleId:cs:b1',
               '{"_v":3,"type":2,"bubbleId":"b1","createdAt":"2026-06-01T00:00:00Z","text":"","capabilityType":15,"toolFormerData":{"tool":15,"toolCallId":"tc","status":"completed","name":"run_terminal_command_v2","params":"{\"command\":\"ls\"}","result":"{\"output\":\"a\\nb\\n\",\"exitCode\":0,\"rejected\":false,\"notInterrupted\":true}"}}');
        "#;
        let f = fixture_db(setup_sql);
        let r = crate::reader::DbReader::open(f.path()).unwrap();
        let s = r.load_session("cs").unwrap();
        let view = session_to_view(&s);
        let tool = &view.turns[0].tool_uses[0];
        assert_eq!(tool.category, Some(ToolCategory::Shell));
        let result = tool.result.as_ref().unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "a\nb\n");
        assert_eq!(tool.input["command"], "ls");
    }

    #[test]
    fn errored_tool_marks_result_as_error() {
        let setup_sql = r#"
            INSERT INTO cursorDiskKV (key, value) VALUES
              ('composerData:ce', '{"_v":16,"composerId":"ce","fullConversationHeadersOnly":[{"bubbleId":"b1","type":2}]}'),
              ('bubbleId:ce:b1',
               '{"_v":3,"type":2,"bubbleId":"b1","createdAt":"2026-06-01T00:00:00Z","capabilityType":15,"toolFormerData":{"tool":38,"toolCallId":"tx","status":"error","name":"edit_file_v2","params":"{\"relativeWorkspacePath\":\"/p/x.rs\"}","result":null}}');
        "#;
        let f = fixture_db(setup_sql);
        let r = crate::reader::DbReader::open(f.path()).unwrap();
        let s = r.load_session("ce").unwrap();
        let view = session_to_view(&s);
        let tool = &view.turns[0].tool_uses[0];
        assert!(tool.result.as_ref().unwrap().is_error);
    }

    #[test]
    fn unknown_tool_id_still_emits_invocation_uncategorized() {
        let setup_sql = r#"
            INSERT INTO cursorDiskKV (key, value) VALUES
              ('composerData:cu', '{"_v":16,"composerId":"cu","fullConversationHeadersOnly":[{"bubbleId":"b1","type":2}]}'),
              ('bubbleId:cu:b1',
               '{"_v":3,"type":2,"bubbleId":"b1","createdAt":"2026-06-01T00:00:00Z","capabilityType":15,"toolFormerData":{"tool":12345,"toolCallId":"tz","status":"completed","name":"future_thing_v9","params":"{\"x\":1}","result":"{\"ok\":true}"}}');
        "#;
        let f = fixture_db(setup_sql);
        let r = crate::reader::DbReader::open(f.path()).unwrap();
        let s = r.load_session("cu").unwrap();
        let view = session_to_view(&s);
        let tool = &view.turns[0].tool_uses[0];
        assert_eq!(tool.name, "future_thing_v9");
        assert_eq!(tool.category, None);
        assert_eq!(tool.input["x"], 1);
    }

    #[test]
    fn started_at_uses_composer_when_present() {
        let (_t, mgr) = setup();
        let view = session_to_view(&mgr.read_session("c1").unwrap());
        assert_eq!(
            view.started_at.unwrap(),
            Utc.timestamp_millis_opt(1000).single().unwrap()
        );
    }

    #[test]
    fn provider_trait_lists_and_loads() {
        let (_t, mgr) = setup();
        let ids = ConversationProvider::list_conversations(&mgr, "").unwrap();
        assert_eq!(ids, vec!["c1".to_string()]);
        let v = ConversationProvider::load_conversation(&mgr, "", "c1").unwrap();
        assert_eq!(v.turns.len(), 3);
        let m = ConversationProvider::load_metadata(&mgr, "", "c1").unwrap();
        assert_eq!(m.message_count, 3);
    }

    #[test]
    fn tool_category_observed_numeric_ids() {
        // Observed in the wild — these come from `toolFormerData.tool`.
        assert_eq!(
            tool_category(15, "run_terminal_command_v2"),
            Some(ToolCategory::Shell)
        );
        assert_eq!(tool_category(38, "edit_file_v2"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category(40, "read_file_v2"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category(41, "ripgrep_raw_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category(42, "glob_file_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category(48, "task_v2"), Some(ToolCategory::Delegation));
    }

    #[test]
    fn tool_category_jsonl_friendly_names() {
        // Agent-side names from the JSONL transcript layer.
        assert_eq!(tool_category(9999, "Shell"), Some(ToolCategory::Shell));
        assert_eq!(tool_category(9999, "Write"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category(9999, "StrReplace"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category(9999, "Read"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category(9999, "Glob"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category(9999, "Grep"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category(9999, "Task"), Some(ToolCategory::Delegation));
        assert_eq!(tool_category(9999, "Edit"), Some(ToolCategory::FileWrite));
    }

    #[test]
    fn tool_category_aiserver_v1_protobuf_names() {
        // The 71 `aiserver.v1.*Params` types from Cursor's workbench
        // bundle, classified into IR ontology.
        let shell = [
            "run_terminal_command_v2",
            "run_terminal_commands",
            "run_test",
            "write_shell_stdin",
        ];
        for n in shell {
            assert_eq!(tool_category(0, n), Some(ToolCategory::Shell), "{n}");
        }

        let file_write = [
            "edit_file_v2",
            "edit_file",
            "delete_file",
            "new_edit",
            "new_file",
            "save_file",
            "reapply",
            "undo_edit",
            "apply_agent_diff",
            "create_rm_files",
            "add_test",
            "delete_test",
            "fix_lints",
            "fix_lints_subagent",
        ];
        for n in file_write {
            assert_eq!(tool_category(0, n), Some(ToolCategory::FileWrite), "{n}");
        }

        let file_read = [
            "read_file_v2",
            "read_file",
            "read_chunk",
            "list_dir",
            "list_dir_v2",
            "read_project",
            "get_project_structure",
            "get_symbols",
            "get_tests",
            "gotodef",
            "summarize_code",
            "read_lints",
            "read_with_linter",
            "read_semsearch_files",
        ];
        for n in file_read {
            assert_eq!(tool_category(0, n), Some(ToolCategory::FileRead), "{n}");
        }

        let file_search = [
            "glob_file_search",
            "ripgrep_raw_search",
            "ripgrep_search",
            "search",
            "search_symbols",
            "semantic_search",
            "semantic_search_full",
            "deep_search",
            "deep_search_subagent",
            "tool_call_file_search",
        ];
        for n in file_search {
            assert_eq!(tool_category(0, n), Some(ToolCategory::FileSearch), "{n}");
        }

        let network = [
            "web_search",
            "web_fetch",
            "fetch_pull_request",
            "call_mcp_tool",
        ];
        for n in network {
            assert_eq!(tool_category(0, n), Some(ToolCategory::Network), "{n}");
        }

        let delegation = [
            "task_v2",
            "task",
            "task_subagent",
            "spec_subagent",
            "background_composer_followup",
            "start_grind_execution",
            "start_grind_planning",
        ];
        for n in delegation {
            assert_eq!(tool_category(0, n), Some(ToolCategory::Delegation), "{n}");
        }
    }

    #[test]
    fn tool_category_uncategorized_drops_to_none() {
        // Tools that don't fit IR ontology stay `None` — callers
        // still see `name` + `input` on the ToolInvocation.
        let none_tools = [
            // Planning
            "create_plan",
            "todo_read",
            "todo_write",
            "read_todos",
            "update_todos",
            // MCP control plane
            "get_mcp_tools",
            "list_mcp_resources",
            "read_mcp_resource",
            "mcp",
            "mcp_auth",
            // UI / control
            "ask_question",
            "communicate_update",
            "send_final_summary",
            "switch_mode",
            "set_run",
            "await",
            "await_task",
            "end",
            "partial",
            "truncated",
            "reflect",
            "add_ui_step",
            // Reporting / VCS write / misc
            "report_bug",
            "report_bugfix_results",
            "record_ci_investigation_findings",
            "ai_attribution",
            "set_active_branch",
            "edit_pr_labels",
            "update_pr_code_tour",
            "pr_management",
            "knowledge_base",
            "fetch_rules",
            "update_project",
            "replace_env",
            "setup_vm_environment",
            "generate_image",
            "computer_use",
            "record_screen",
            "create_diagram",
            // Truly unknown
            "future_tool_does_not_exist",
        ];
        for n in none_tools {
            assert_eq!(tool_category(0, n), None, "{n} should be uncategorized");
        }
    }

    #[test]
    fn tool_category_unknown_id_falls_through_to_name() {
        // Future numeric ids we haven't seen still classify via name.
        assert_eq!(tool_category(7777, "edit_file_v2"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category(7777, "future_tool"), None);
    }
}
