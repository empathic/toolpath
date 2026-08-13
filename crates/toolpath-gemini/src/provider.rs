//! Implementation of `toolpath-convo` traits for Gemini CLI conversations.
//!
//! Key differences from the Claude implementation:
//!
//! - Gemini stores tool results inline on the same message as the call, so
//!   there's no cross-message "tool-result-only" assembly pass.
//! - Sub-agent work lives in sibling chat files inside the same session
//!   UUID directory. When a `task` tool invocation fires in the main
//!   chat, the matching sub-agent file's turns are populated onto a
//!   [`DelegatedWork`].

use crate::GeminiConvo;
use crate::types::{ChatFile, Conversation, GeminiMessage, GeminiRole, Thought, Tokens, ToolCall};
use serde_json::Value;
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError, DelegatedWork,
    EnvironmentSnapshot, Role, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

// ── Role/tool mapping ────────────────────────────────────────────────

fn gemini_role_to_role(role: &GeminiRole) -> Role {
    match role {
        GeminiRole::User => Role::User,
        GeminiRole::Gemini => Role::Assistant,
        GeminiRole::Info => Role::System,
        GeminiRole::Other(s) => Role::Other(s.clone()),
    }
}

/// Classify a Gemini CLI tool name into toolpath's category ontology.
///
/// Returns `None` for unrecognized tools. Keep this table in sync with
/// <https://geminicli.com/docs/reference/tools>.
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "read_file" | "read_many_files" | "list_directory" | "get_internal_docs"
        | "read_mcp_resource" => Some(ToolCategory::FileRead),
        "glob" | "grep_search" | "search_file_content" => Some(ToolCategory::FileSearch),
        "write_file" | "replace" | "edit" => Some(ToolCategory::FileWrite),
        "run_shell_command" => Some(ToolCategory::Shell),
        "web_fetch" | "google_web_search" => Some(ToolCategory::Network),
        "task" | "activate_skill" => Some(ToolCategory::Delegation),
        _ => None,
    }
}

/// Reverse of [`tool_category`]: pick Gemini's preferred native tool name
/// for a generic [`ToolCategory`], using the call's `args` to disambiguate
/// when multiple Gemini tools share the same category.
///
/// Used by [`crate::project::GeminiProjector`] when projecting tool calls
/// from foreign harnesses (Claude, Codex, etc.) — we know the category
/// from the source harness's classifier and need to pick a Gemini name
/// whose arg shape best matches the call's actual args. Returns `None`
/// for categories with no obvious Gemini analog.
pub fn native_name(category: ToolCategory, args: &Value) -> Option<&'static str> {
    match category {
        ToolCategory::Shell => Some("run_shell_command"),
        ToolCategory::FileRead => Some(if args.get("file_paths").is_some() {
            "read_many_files"
        } else if args.get("path").is_some() && args.get("file_path").is_none() {
            // `path` (no `file_path`) matches list_directory's arg shape
            "list_directory"
        } else {
            "read_file"
        }),
        ToolCategory::FileSearch => Some(if args.get("pattern").is_some() {
            "grep_search"
        } else {
            "glob"
        }),
        ToolCategory::FileWrite => Some(
            // Edit-family calls carry old_string + new_string; whole-file
            // writes carry content. Multi-edit shapes (Claude's MultiEdit
            // edits[]) collapse to `replace` — Gemini has no direct
            // multi-edit equivalent, but `replace` accepts the args
            // schema closely enough that the call is still intelligible.
            if args.get("old_string").is_some() || args.get("edits").is_some() {
                "replace"
            } else {
                "write_file"
            },
        ),
        ToolCategory::Network => Some(if args.get("url").is_some() {
            "web_fetch"
        } else {
            "google_web_search"
        }),
        ToolCategory::Delegation => Some("task"),
    }
}

// ── Message → Turn ────────────────────────────────────────────────────

fn message_to_turn(msg: &GeminiMessage, working_dir: Option<&str>) -> Turn {
    let text = msg.content.text();
    let thinking = flatten_thoughts(msg.thoughts());
    let tool_uses: Vec<ToolInvocation> = msg
        .tool_calls()
        .iter()
        .map(tool_call_to_invocation)
        .collect();
    let file_mutations = compute_file_mutations(msg.tool_calls());

    let token_usage = msg.tokens.as_ref().map(tokens_to_usage);

    let environment = working_dir.map(|wd| EnvironmentSnapshot {
        working_dir: Some(wd.to_string()),
        vcs_branch: None,
        vcs_revision: None,
    });

    Turn {
        id: msg.id.clone(),
        parent_id: None,
        group_id: None,
        role: gemini_role_to_role(&msg.role),
        timestamp: msg.timestamp.clone(),
        text,
        thinking,
        tool_uses,
        model: msg.model.clone(),
        stop_reason: None,
        token_usage,
        attributed_token_usage: None,
        environment,
        delegations: vec![],
        file_mutations,
    }
}

/// Map Gemini's on-disk [`Tokens`] struct onto the provider-agnostic
/// [`TokenUsage`].
///
/// Gemini records `thoughts` (reasoning) as a **separate additive**
/// counter, sibling to `output` — across real sessions
/// `total == input + output + thoughts` exactly, and the format doc
/// describes `output` as "generated tokens *excluding reasoning*."
/// Google bills reasoning as output, so we fold `thoughts` into
/// `output_tokens` (same convention as opencode) — that way the IR's
/// `output` means "all generated tokens" and the session total isn't
/// under-counted.
///
/// The folded reasoning slice is *also* recorded under
/// `breakdowns["output"]["reasoning"]`. It's INFORMATIONAL: breakdowns
/// are never summed into the total (output already counts it), and the
/// invariant `Σ(inner) = reasoning ≤ output` holds because we fold the
/// same number in. It's also what lets the projector un-fold reasoning
/// back out of `output_tokens` (see `tokens_from_common`), so the
/// `output`/`thoughts` split round-trips losslessly. The entry is
/// recorded whenever `thoughts` is present (including a genuine `Some(0)`),
/// preserving the `Some(0)` vs `None` distinction; when `thoughts` is
/// absent the map stays empty and is omitted from serialization.
///
/// `tool` is prompt-side (tool-result tokens billed separately) and
/// `total` is a Gemini-side sum; neither is folded here — both remain
/// available raw via `Turn.extra["gemini"]["tokens"]`.
fn tokens_to_usage(t: &Tokens) -> TokenUsage {
    let output = t.output.unwrap_or(0);
    let thoughts = t.thoughts.unwrap_or(0);
    let generated = output.saturating_add(thoughts);

    let mut usage = TokenUsage {
        input_tokens: t.input,
        // Fold reasoning into output (additive in Gemini — billed as
        // output). None only when both output and thoughts are
        // absent/zero, mirroring the per-field Option semantics.
        output_tokens: if generated == 0 {
            None
        } else {
            Some(generated)
        },
        cache_read_tokens: t.cached,
        cache_write_tokens: None,
        ..Default::default()
    };

    // Memoize the reasoning slice folded into output so the projector can
    // un-fold it back out losslessly. Recorded whenever `thoughts` is
    // present — including a genuine `Some(0)` — so the projector
    // reconstructs `Some(0)` rather than `None`; absent only when the
    // source had no reasoning counter at all.
    if let Some(thoughts) = t.thoughts {
        usage
            .breakdowns
            .entry("output".to_string())
            .or_default()
            .insert("reasoning".to_string(), thoughts);
    }

    usage
}

/// For each file-write tool call in this message, build a
/// `FileMutation` with a pre-resolved unified diff. Preference order:
///   1. Gemini's own `resultDisplay.fileDiff` when present (real diff
///      computed by the harness).
///   2. Hand-rolled fallback from `args` (`old_string`/`new_string` for
///      `replace`, `content` for `write_file`).
///
/// `tool_id` links back to the [`ToolCall`].
fn compute_file_mutations(calls: &[ToolCall]) -> Vec<toolpath_convo::FileMutation> {
    let mut out = Vec::new();
    for call in calls {
        if tool_category(&call.name) != Some(ToolCategory::FileWrite) {
            continue;
        }
        let Some(path) = file_path_from_args(&call.args) else {
            continue;
        };
        let raw_diff = call.file_diff().or_else(|| fallback_raw_diff(call));
        let operation = match call.name.as_str() {
            "write_file" => Some("add".to_string()),
            "replace" | "edit" => Some("update".to_string()),
            _ => Some(call.name.clone()),
        };
        let after = match call.name.as_str() {
            "write_file" => call
                .args
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        out.push(toolpath_convo::FileMutation {
            path,
            tool_id: Some(call.id.clone()),
            operation,
            raw_diff,
            before: None,
            after,
            rename_to: None,
        });
    }
    out
}

/// Synthesize a unified-diff hunk when Gemini's `resultDisplay.fileDiff`
/// is absent. Not pixel-perfect but enough to give readers a change
/// perspective.
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

fn flatten_thoughts(thoughts: &[Thought]) -> Option<String> {
    if thoughts.is_empty() {
        return None;
    }
    let joined: Vec<String> = thoughts
        .iter()
        .filter_map(|t| match (&t.subject, &t.description) {
            (Some(s), Some(d)) => Some(format!("**{}**\n{}", s, d)),
            (Some(s), None) => Some(s.clone()),
            (None, Some(d)) => Some(d.clone()),
            (None, None) => None,
        })
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join("\n\n"))
    }
}

fn tool_call_to_invocation(call: &ToolCall) -> ToolInvocation {
    let text = call.result_text();
    let is_error = call.is_error();
    let result = if call.result.is_empty() && !is_error {
        None
    } else {
        Some(ToolResult {
            content: text,
            is_error,
        })
    };
    ToolInvocation {
        id: call.id.clone(),
        name: call.name.clone(),
        input: call.args.clone(),
        result,
        category: tool_category(&call.name),
    }
}

// ── Delegation wiring ────────────────────────────────────────────────

/// Build a `DelegatedWork` from a sub-agent chat file, optionally using
/// a parent `ToolInvocation`'s fields as a fallback.
fn sub_agent_to_delegation(
    sub: &ChatFile,
    working_dir: Option<&str>,
    fallback_prompt: &str,
    fallback_result: Option<&ToolResult>,
) -> DelegatedWork {
    let turns: Vec<Turn> = sub
        .messages
        .iter()
        .map(|m| message_to_turn(m, working_dir))
        .collect();

    let prompt = first_user_text(sub).unwrap_or_else(|| fallback_prompt.to_string());

    let result = sub
        .summary
        .clone()
        .or_else(|| fallback_result.map(|r| r.content.clone()));

    let agent_id = if sub.session_id.is_empty() {
        format!("subagent-{}", turns.len())
    } else {
        sub.session_id.clone()
    };

    DelegatedWork {
        agent_id,
        prompt,
        turns,
        result,
    }
}

fn first_user_text(chat: &ChatFile) -> Option<String> {
    chat.messages
        .iter()
        .find(|m| m.role == GeminiRole::User)
        .map(|m| m.content.text())
        .filter(|t| !t.is_empty())
}

/// Build a fallback `DelegatedWork` from a `ToolInvocation` when no
/// sub-agent file was found — mirrors the Claude provider's behaviour.
fn tool_invocation_to_delegation(tu: &ToolInvocation) -> DelegatedWork {
    DelegatedWork {
        agent_id: tu.id.clone(),
        prompt: tu
            .input
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        turns: vec![],
        result: tu.result.as_ref().map(|r| r.content.clone()),
    }
}

// ── Conversation → View ──────────────────────────────────────────────

fn conversation_to_view(convo: &Conversation) -> ConversationView {
    let working_dir: Option<String> = convo.project_path.clone().or_else(|| {
        convo
            .main
            .directories()
            .first()
            .map(|p| p.to_string_lossy().to_string())
    });
    let wd_ref = working_dir.as_deref();

    // Sort sub-agents by start_time (deterministic attachment).
    let mut sub_order: Vec<&ChatFile> = convo.sub_agents.iter().collect();
    sub_order.sort_by_key(|s| s.start_time);
    let mut sub_iter = sub_order.into_iter();

    let mut turns: Vec<Turn> = Vec::with_capacity(convo.main.messages.len());

    for msg in &convo.main.messages {
        let mut turn = message_to_turn(msg, wd_ref);

        // For each delegation-category tool invocation, try to pull the
        // next sub-agent off the queue.
        for tu in &turn.tool_uses {
            if tu.category != Some(ToolCategory::Delegation) {
                continue;
            }
            let delegation = match sub_iter.next() {
                Some(sub) => {
                    let prompt_fallback = tu
                        .input
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    sub_agent_to_delegation(sub, wd_ref, prompt_fallback, tu.result.as_ref())
                }
                None => tool_invocation_to_delegation(tu),
            };
            turn.delegations.push(delegation);
        }

        turns.push(turn);
    }

    // Leftover sub-agents (no matching task invocation) attach to the
    // last assistant turn, or get dropped if there is none.
    let leftover: Vec<&ChatFile> = sub_iter.collect();
    if !leftover.is_empty()
        && let Some(last_assistant) = turns
            .iter_mut()
            .rev()
            .find(|t| matches!(t.role, Role::Assistant))
    {
        for sub in leftover {
            last_assistant
                .delegations
                .push(sub_agent_to_delegation(sub, wd_ref, "", None));
        }
    }

    // Gemini's wire format doesn't carry parent_id on messages, so link
    // turns sequentially. (Matches the old `derive_path_from_view`,
    // which used `last_step_id` as the parent for each new step.)
    let mut prev: Option<String> = None;
    for t in turns.iter_mut() {
        if t.parent_id.is_none() {
            t.parent_id = prev.clone();
        }
        prev = Some(t.id.clone());
    }

    let total_usage = sum_usage(&turns);
    let files_changed = extract_files_changed(&turns);

    let view_base = working_dir.as_ref().map(|wd| toolpath_convo::SessionBase {
        working_dir: Some(wd.clone()),
        vcs_revision: None,
        vcs_branch: None,
        vcs_remote: None,
    });

    ConversationView {
        id: convo.session_uuid.clone(),
        started_at: convo.started_at,
        last_activity: convo.last_activity,
        turns,
        total_usage,
        provider_id: Some("gemini-cli".into()),
        files_changed,
        session_ids: vec![],
        events: vec![],
        base: view_base,
        producer: Some(toolpath_convo::ProducerInfo {
            name: "gemini-cli".into(),
            version: None,
        }),
    }
}

fn sum_usage(turns: &[Turn]) -> Option<TokenUsage> {
    let mut total = TokenUsage::default();
    let mut any = false;
    for turn in turns {
        if let Some(u) = &turn.token_usage {
            any = true;
            total.input_tokens =
                Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
            total.output_tokens =
                Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
            total.cache_read_tokens = match (total.cache_read_tokens, u.cache_read_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
        }
        // Also walk sub-agent turns inside delegations.
        for d in &turn.delegations {
            for t in &d.turns {
                if let Some(u) = &t.token_usage {
                    any = true;
                    total.input_tokens =
                        Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
                    total.output_tokens =
                        Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
                    total.cache_read_tokens = match (total.cache_read_tokens, u.cache_read_tokens) {
                        (Some(a), Some(b)) => Some(a + b),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                }
            }
        }
    }
    if any { Some(total) } else { None }
}

fn extract_files_changed(turns: &[Turn]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    let push = |tool_use: &ToolInvocation,
                seen: &mut std::collections::HashSet<String>,
                files: &mut Vec<String>| {
        if tool_use.category == Some(ToolCategory::FileWrite)
            && let Some(path) = file_path_from_args(&tool_use.input)
            && seen.insert(path.clone())
        {
            files.push(path);
        }
    };
    for turn in turns {
        for tu in &turn.tool_uses {
            push(tu, &mut seen, &mut files);
        }
        for d in &turn.delegations {
            for t in &d.turns {
                for tu in &t.tool_uses {
                    push(tu, &mut seen, &mut files);
                }
            }
        }
    }
    files
}

/// Pull the file path out of a tool's `args`. Gemini uses different key
/// names depending on the tool (`file_path`, `path`, or `absolute_path`).
pub(crate) fn file_path_from_args(args: &Value) -> Option<String> {
    for key in ["file_path", "absolute_path", "path"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

// ── Public conversion helpers ────────────────────────────────────────

/// Convert a Gemini [`Conversation`] into a [`ConversationView`].
pub fn to_view(convo: &Conversation) -> ConversationView {
    conversation_to_view(convo)
}

/// Convert a single [`GeminiMessage`] into a [`Turn`]. Does not perform
/// any cross-message sub-agent assembly; call [`to_view`] for that.
pub fn to_turn(msg: &GeminiMessage) -> Turn {
    message_to_turn(msg, None)
}

// ── Trait impls ──────────────────────────────────────────────────────

impl ConversationProvider for GeminiConvo {
    fn list_conversations(&self, project: &str) -> toolpath_convo::Result<Vec<String>> {
        GeminiConvo::list_conversations(self, project)
            .map_err(|e| ConvoError::Provider(e.to_string()))
    }

    fn load_conversation(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let convo = self
            .read_conversation(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        let view = conversation_to_view(&convo);
        Ok(view)
    }

    fn load_metadata(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let meta = self
            .read_conversation_metadata(project, conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: meta.session_uuid,
            started_at: meta.started_at,
            last_activity: meta.last_activity,
            message_count: meta.message_count,
            file_path: Some(meta.file_path),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let metas = self
            .list_conversation_metadata(project)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(metas
            .into_iter()
            .map(|m| ConversationMeta {
                id: m.session_uuid,
                started_at: m.started_at,
                last_activity: m.last_activity,
                message_count: m.message_count,
                file_path: Some(m.file_path),
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
    use crate::PathResolver;
    use std::fs;
    use tempfile::TempDir;

    fn setup_provider() -> (TempDir, GeminiConvo) {
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let session_dir = gemini.join("tmp/myrepo/chats/session-uuid");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            gemini.join("projects.json"),
            r#"{"projects":{"/abs/myrepo":"myrepo"}}"#,
        )
        .unwrap();

        let main = r#"{
  "sessionId":"main-s",
  "projectHash":"h",
  "startTime":"2026-04-17T15:00:00Z",
  "lastUpdated":"2026-04-17T15:10:00Z",
  "directories":["/abs/myrepo"],
  "messages":[
    {"id":"m1","timestamp":"2026-04-17T15:00:00Z","type":"user","content":[{"text":"Find the bug"}]},
    {"id":"m2","timestamp":"2026-04-17T15:00:01Z","type":"gemini","content":"I'll delegate.","model":"gemini-3-flash-preview","tokens":{"input":100,"output":50,"cached":0,"thoughts":10,"tool":0,"total":160},"toolCalls":[
      {"id":"task-1","name":"task","args":{"prompt":"Find auth bug"},"status":"success","timestamp":"2026-04-17T15:00:01Z","result":[{"functionResponse":{"id":"task-1","name":"task","response":{"output":"Found it"}}}]}
    ]},
    {"id":"m3","timestamp":"2026-04-17T15:05:00Z","type":"gemini","content":"Writing fix.","model":"gemini-3-flash-preview","tokens":{"input":200,"output":80,"cached":50,"thoughts":0,"tool":0,"total":330},"toolCalls":[
      {"id":"write-1","name":"write_file","args":{"file_path":"src/auth.rs","content":"fn ok(){}"},"status":"success","timestamp":"2026-04-17T15:05:00Z","result":[{"functionResponse":{"id":"write-1","name":"write_file","response":{"output":"wrote"}}}]}
    ]},
    {"id":"m4","timestamp":"2026-04-17T15:05:05Z","type":"gemini","content":"Oops, fix again.","model":"gemini-3-flash-preview","toolCalls":[
      {"id":"replace-1","name":"replace","args":{"file_path":"src/auth.rs","oldString":"a","newString":"b"},"status":"success","timestamp":"2026-04-17T15:05:05Z","result":[{"functionResponse":{"id":"replace-1","name":"replace","response":{"output":"ok"}}}]},
      {"id":"write-2","name":"write_file","args":{"file_path":"src/lib.rs","content":"pub mod auth;"},"status":"success","timestamp":"2026-04-17T15:05:05Z","result":[{"functionResponse":{"id":"write-2","name":"write_file","response":{"output":"wrote"}}}]}
    ]}
  ]
}"#;
        fs::write(session_dir.join("main.json"), main).unwrap();

        let sub = r#"{
  "sessionId":"qclszz",
  "projectHash":"h",
  "startTime":"2026-04-17T15:01:00Z",
  "lastUpdated":"2026-04-17T15:04:00Z",
  "kind":"subagent",
  "summary":"Found auth bug at line 42",
  "messages":[
    {"id":"s1","timestamp":"2026-04-17T15:01:00Z","type":"user","content":[{"text":"Search for auth bug"}]},
    {"id":"s2","timestamp":"2026-04-17T15:02:00Z","type":"gemini","content":"","thoughts":[{"subject":"Searching","description":"looking in /auth","timestamp":"2026-04-17T15:02:00Z"}],"model":"gemini-3-flash-preview","tokens":{"input":20,"output":5,"cached":0},"toolCalls":[
      {"id":"qclszz#0-0","name":"grep_search","args":{"pattern":"auth"},"status":"success","timestamp":"2026-04-17T15:02:00Z","result":[{"functionResponse":{"id":"qclszz#0-0","name":"grep_search","response":{"output":"auth.rs:42"}}}]}
    ]}
  ]
}"#;
        fs::write(session_dir.join("qclszz.json"), sub).unwrap();

        let resolver = PathResolver::new(temp.path()).with_gemini_dir(&gemini);
        (temp, GeminiConvo::with_resolver(resolver))
    }

    #[test]
    fn test_tool_category_mapping() {
        assert_eq!(tool_category("read_file"), Some(ToolCategory::FileRead));
        assert_eq!(tool_category("glob"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("grep_search"), Some(ToolCategory::FileSearch));
        assert_eq!(tool_category("write_file"), Some(ToolCategory::FileWrite));
        assert_eq!(tool_category("replace"), Some(ToolCategory::FileWrite));
        assert_eq!(
            tool_category("run_shell_command"),
            Some(ToolCategory::Shell)
        );
        assert_eq!(tool_category("web_fetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("task"), Some(ToolCategory::Delegation));
        assert_eq!(
            tool_category("activate_skill"),
            Some(ToolCategory::Delegation)
        );
        assert_eq!(tool_category("unknown"), None);
    }

    #[test]
    fn test_load_conversation_basic() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(view.id, "session-uuid");
        assert_eq!(view.provider_id.as_deref(), Some("gemini-cli"));
        assert_eq!(view.turns.len(), 4);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Find the bug");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll delegate.");
        assert_eq!(
            view.turns[1].model.as_deref(),
            Some("gemini-3-flash-preview")
        );
    }

    #[test]
    fn test_delegation_populated_from_sub_agent() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        let delegations = &view.turns[1].delegations;
        assert_eq!(delegations.len(), 1);
        let d = &delegations[0];
        assert_eq!(d.agent_id, "qclszz");
        assert_eq!(d.prompt, "Search for auth bug");
        assert_eq!(d.result.as_deref(), Some("Found auth bug at line 42"));
        // Sub-agent turns are populated, unlike Claude
        assert_eq!(d.turns.len(), 2);
        assert_eq!(d.turns[0].text, "Search for auth bug");
    }

    #[test]
    fn test_tool_result_assembled_inline() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        let result = view.turns[1].tool_uses[0].result.as_ref().unwrap();
        assert_eq!(result.content, "Found it");
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_category_on_invocations() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(
            view.turns[1].tool_uses[0].category,
            Some(ToolCategory::Delegation)
        );
        assert_eq!(
            view.turns[2].tool_uses[0].category,
            Some(ToolCategory::FileWrite)
        );
    }

    #[test]
    fn test_token_usage_aggregated() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        let total = view.total_usage.as_ref().unwrap();
        // Main turns: input/(output+thoughts) = (100, 50+10), (200, 80+0).
        // Sub-agent turn: (20, 5+0). thoughts is additive reasoning, folded
        // into output (billed as output by Google).
        assert_eq!(total.input_tokens, Some(320));
        assert_eq!(total.output_tokens, Some(145));
        assert_eq!(total.cache_read_tokens, Some(50));
    }

    #[test]
    fn test_thoughts_folded_into_output_with_breakdown() {
        // Real-fixture-shaped numbers: total == input + output + thoughts
        // (8665 + 94 + 243 == 9002). output EXCLUDES reasoning, so folding
        // gives output_tokens = 94 + 243 = 337, and the reasoning slice is
        // recorded under breakdowns["output"]["reasoning"] = 243 (≤ output).
        let t = Tokens {
            input: Some(8665),
            output: Some(94),
            cached: Some(0),
            thoughts: Some(243),
            tool: Some(0),
            total: Some(9002),
        };
        let u = tokens_to_usage(&t);
        assert_eq!(u.input_tokens, Some(8665));
        assert_eq!(u.output_tokens, Some(337));
        let reasoning = u
            .breakdowns
            .get("output")
            .and_then(|m| m.get("reasoning"))
            .copied();
        assert_eq!(reasoning, Some(243));
        // reasoning ≤ output invariant holds.
        assert!(reasoning.unwrap() <= u.output_tokens.unwrap());
    }

    #[test]
    fn test_present_zero_thoughts_records_zero_breakdown() {
        // thoughts == Some(0) → breakdown records reasoning: 0 so the
        // projector reconstructs Some(0) (not None) on the reverse path.
        // output_tokens is unchanged (folding 0 is a no-op).
        let t = Tokens {
            input: Some(200),
            output: Some(80),
            cached: Some(50),
            thoughts: Some(0),
            tool: Some(0),
            total: Some(330),
        };
        let u = tokens_to_usage(&t);
        assert_eq!(u.output_tokens, Some(80));
        assert_eq!(
            u.breakdowns.get("output").and_then(|m| m.get("reasoning")),
            Some(&0)
        );
    }

    #[test]
    fn test_absent_thoughts_yields_no_breakdown() {
        // thoughts absent (Gemini 2.5) → treated as 0: no breakdown,
        // output_tokens == output.
        let t = Tokens {
            input: Some(20),
            output: Some(5),
            cached: Some(0),
            thoughts: None,
            tool: None,
            total: None,
        };
        let u = tokens_to_usage(&t);
        assert_eq!(u.output_tokens, Some(5));
        assert!(u.breakdowns.is_empty());
    }

    #[test]
    fn test_zero_output_and_thoughts_yields_none_output() {
        // Both output and thoughts zero → output_tokens None (mirrors the
        // per-field Option semantics; no fabricated zero). thoughts is
        // present (Some(0)), so the breakdown still records reasoning: 0
        // for lossless round-trip of the Some(0) distinction.
        let t = Tokens {
            input: Some(100),
            output: Some(0),
            cached: Some(0),
            thoughts: Some(0),
            tool: Some(0),
            total: Some(100),
        };
        let u = tokens_to_usage(&t);
        assert_eq!(u.output_tokens, None);
        assert_eq!(
            u.breakdowns.get("output").and_then(|m| m.get("reasoning")),
            Some(&0)
        );

        // And the fully-absent case: thoughts None → no breakdown.
        let empty = Tokens::default();
        let u2 = tokens_to_usage(&empty);
        assert_eq!(u2.output_tokens, None);
        assert!(u2.breakdowns.is_empty());
    }

    #[test]
    fn test_files_changed() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(
            view.files_changed,
            vec!["src/auth.rs".to_string(), "src/lib.rs".to_string()]
        );
    }

    #[test]
    fn test_environment_working_dir() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        for turn in &view.turns {
            let wd = turn
                .environment
                .as_ref()
                .and_then(|e| e.working_dir.as_deref());
            assert_eq!(wd, Some("/abs/myrepo"));
        }
    }

    #[test]
    fn test_thinking_from_sub_agent_thoughts() {
        let (_t, p) = setup_provider();
        let view =
            ConversationProvider::load_conversation(&p, "/abs/myrepo", "session-uuid").unwrap();
        let sub_turn = &view.turns[1].delegations[0].turns[1];
        let thinking = sub_turn.thinking.as_ref().unwrap();
        assert!(thinking.contains("Searching"));
        assert!(thinking.contains("looking in /auth"));
    }

    #[test]
    fn test_list_metadata() {
        let (_t, p) = setup_provider();
        let metas = ConversationProvider::list_metadata(&p, "/abs/myrepo").unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "session-uuid");
        // Chains are not a thing in Gemini — link fields stay None.
        assert!(metas[0].predecessor.is_none());
        assert!(metas[0].successor.is_none());
    }

    #[test]
    fn test_load_metadata() {
        let (_t, p) = setup_provider();
        let meta = ConversationProvider::load_metadata(&p, "/abs/myrepo", "session-uuid").unwrap();
        assert_eq!(meta.id, "session-uuid");
        // 4 main messages + 2 sub-agent messages
        assert_eq!(meta.message_count, 6);
    }

    #[test]
    fn test_list_conversations_via_trait() {
        let (_t, p) = setup_provider();
        let ids = ConversationProvider::list_conversations(&p, "/abs/myrepo").unwrap();
        assert_eq!(ids, vec!["session-uuid".to_string()]);
    }

    #[test]
    fn test_to_view_directly() {
        let (_t, p) = setup_provider();
        let convo = p.read_conversation("/abs/myrepo", "session-uuid").unwrap();
        let view = to_view(&convo);
        assert_eq!(view.turns.len(), 4);
    }

    #[test]
    fn test_to_turn_single_message() {
        let json = r#"{"id":"m","timestamp":"ts","type":"user","content":[{"text":"hi"}]}"#;
        let msg: GeminiMessage = serde_json::from_str(json).unwrap();
        let turn = to_turn(&msg);
        assert_eq!(turn.id, "m");
        assert_eq!(turn.text, "hi");
        assert_eq!(turn.role, Role::User);
    }

    #[test]
    fn test_file_path_from_args_all_keys() {
        let v1 = serde_json::json!({"file_path": "/a"});
        let v2 = serde_json::json!({"absolute_path": "/b"});
        let v3 = serde_json::json!({"path": "/c"});
        let v4 = serde_json::json!({"something_else": "/d"});
        assert_eq!(file_path_from_args(&v1).as_deref(), Some("/a"));
        assert_eq!(file_path_from_args(&v2).as_deref(), Some("/b"));
        assert_eq!(file_path_from_args(&v3).as_deref(), Some("/c"));
        assert_eq!(file_path_from_args(&v4), None);
    }

    #[test]
    fn test_flatten_thoughts() {
        let thoughts = vec![
            Thought {
                subject: Some("s1".into()),
                description: Some("d1".into()),
                timestamp: None,
            },
            Thought {
                subject: None,
                description: Some("d2".into()),
                timestamp: None,
            },
            Thought {
                subject: Some("s3".into()),
                description: None,
                timestamp: None,
            },
            Thought {
                subject: None,
                description: None,
                timestamp: None,
            },
        ];
        let out = flatten_thoughts(&thoughts).unwrap();
        assert!(out.contains("s1"));
        assert!(out.contains("d1"));
        assert!(out.contains("d2"));
        assert!(out.contains("s3"));
    }

    #[test]
    fn test_flatten_thoughts_empty() {
        assert!(flatten_thoughts(&[]).is_none());
    }

    #[test]
    fn test_unused_delegation_fallback() {
        // If sub-agent file is missing, delegation still emits from the
        // tool_use fields.
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let session_dir = gemini.join("tmp/p/chats/s");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();

        fs::write(
            session_dir.join("main.json"),
            r#"{
  "sessionId":"main",
  "projectHash":"",
  "messages":[
    {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"x"}]},
    {"id":"m2","timestamp":"ts","type":"gemini","content":"","toolCalls":[
      {"id":"t1","name":"task","args":{"prompt":"go"},"status":"success","timestamp":"ts","result":[{"functionResponse":{"id":"t1","name":"task","response":{"output":"done"}}}]}
    ]}
  ]
}"#,
        )
        .unwrap();

        let mgr =
            GeminiConvo::with_resolver(PathResolver::new(temp.path()).with_gemini_dir(&gemini));
        let view = ConversationProvider::load_conversation(&mgr, "/p", "s").unwrap();

        let d = &view.turns[1].delegations[0];
        assert_eq!(d.agent_id, "t1");
        assert_eq!(d.prompt, "go");
        assert_eq!(d.result.as_deref(), Some("done"));
        assert!(d.turns.is_empty());
    }

    #[test]
    fn test_leftover_subagent_attached_to_last_assistant() {
        // Two sub-agents but only one `task` call — the second sub-agent
        // attaches to the last assistant turn.
        let temp = TempDir::new().unwrap();
        let gemini = temp.path().join(".gemini");
        let session_dir = gemini.join("tmp/p/chats/s");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(gemini.join("projects.json"), r#"{"projects":{"/p":"p"}}"#).unwrap();
        fs::write(
            session_dir.join("main.json"),
            r#"{"sessionId":"main","projectHash":"","messages":[
  {"id":"m1","timestamp":"ts","type":"user","content":[{"text":"x"}]},
  {"id":"m2","timestamp":"ts","type":"gemini","content":"","toolCalls":[
    {"id":"t1","name":"task","args":{},"status":"success","timestamp":"ts"}
  ]}
]}"#,
        )
        .unwrap();
        fs::write(
            session_dir.join("a.json"),
            r#"{"sessionId":"a","projectHash":"","startTime":"2026-04-17T10:00:00Z","kind":"subagent","summary":"A","messages":[]}"#,
        )
        .unwrap();
        fs::write(
            session_dir.join("b.json"),
            r#"{"sessionId":"b","projectHash":"","startTime":"2026-04-17T11:00:00Z","kind":"subagent","summary":"B","messages":[]}"#,
        )
        .unwrap();

        let mgr =
            GeminiConvo::with_resolver(PathResolver::new(temp.path()).with_gemini_dir(&gemini));
        let view = ConversationProvider::load_conversation(&mgr, "/p", "s").unwrap();
        let delegations = &view.turns[1].delegations;
        assert_eq!(delegations.len(), 2);
        // a.json attaches to the task (first delegation), b.json is leftover
        assert_eq!(delegations[0].agent_id, "a");
        assert_eq!(delegations[1].agent_id, "b");
    }
}
