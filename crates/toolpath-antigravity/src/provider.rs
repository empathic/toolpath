//! `toolpath_convo::ConversationProvider` for Antigravity CLI (`agy`).
//!
//! A turn is a `user_input`-carrying step followed by every step up to
//! the next `user_input` (or the end): any tool calls in between attach
//! to that one assistant [`Turn`], and every planner-response/model-usage
//! step in the run contributes its (non-empty) text and its usage —
//! `agy` can emit more than one generation step per turn (one deciding
//! to call a tool, a later one with the final answer), confirmed in the
//! `tool` fixture.

use crate::AntigravityConvo;
use crate::types::{Conversation, Step};
use serde_json::Value;
use toolpath_convo::{
    ConversationMeta, ConversationProvider, ConversationView, ConvoError, Item, ProducerInfo, Role,
    TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Classify an Antigravity native tool name into toolpath's ontology.
/// Names observed in real 1.4.8 samples (the agent's own tool-permission
/// listing, decoded from a `user_input` step's trailing config blob).
pub fn tool_category(name: &str) -> Option<ToolCategory> {
    match name {
        "read_file" => Some(ToolCategory::FileRead),
        "write_file" | "edit_file" => Some(ToolCategory::FileWrite),
        "run_command" => Some(ToolCategory::Shell),
        "search_web" => Some(ToolCategory::Network),
        "invoke_subagent" | "define_subagent" | "manage_subagents" => {
            Some(ToolCategory::Delegation)
        }
        _ => None,
    }
}

fn step_to_turn_pieces(step: &Step) -> (Option<ToolInvocation>, Option<&str>, Option<TokenUsage>) {
    let tool_use = step.tool_call.as_ref().map(|tc| {
        let input: Value = serde_json::from_str(&tc.arguments_json)
            .unwrap_or_else(|_| Value::String(tc.arguments_json.clone()));
        ToolInvocation {
            id: tc.id.clone(),
            name: tc.name.clone(),
            input,
            result: step.tool_result_text.clone().map(|content| ToolResult {
                content,
                is_error: false,
            }),
            category: tool_category(&tc.name),
        }
    });

    let text = step.assistant_text.as_deref().filter(|s| !s.is_empty());

    let usage = step.model_usage.as_ref().map(|u| {
        let mut usage = TokenUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
            ..Default::default()
        };
        // output_tokens already includes thinking (confirmed: thinking +
        // response == output in every sample) — record the reasoning
        // slice informationally, same convention as toolpath-gemini.
        if let Some(thinking) = u.thinking_output_tokens {
            usage
                .breakdowns
                .entry("output".to_string())
                .or_default()
                .insert("reasoning".to_string(), thinking);
        }
        usage
    });

    (tool_use, text, usage)
}

fn blank_turn(id: String, parent_id: Option<String>, role: Role) -> Turn {
    Turn {
        id,
        parent_id,
        group_id: None,
        role,
        timestamp: String::new(),
        text: String::new(),
        thinking: None,
        // Not populated: `ThinkingSignature` (#375) models Anthropic's
        // signed-thinking scheme (a provider tag plus a verifiable
        // sha256-of-text check). Agy's own signatures
        // (`CortexStepPlannerResponse.thinking_signature`,
        // `ChatToolCall.thought_signature`) are Gemini function-calling
        // signatures over the *tool call*, not over `Turn.thinking` text —
        // a different shape this field doesn't fit. They're carried
        // instead on `ToolInvocation` extras where decoded; see
        // `step_to_turn_pieces`.
        thinking_signature: None,
        tool_uses: Vec::new(),
        model: None,
        // Antigravity always serves a Google model (the closed
        // `exa.codeium_common_pb.Model` enum); "google" is the provider
        // family, distinct from the harness id ("antigravity").
        provider: None,
        stop_reason: None,
        token_usage: None,
        attributed_token_usage: None,
        environment: None,
        delegations: Vec::new(),
        file_mutations: Vec::new(),
    }
}

fn add_usage(total: &mut TokenUsage, u: &TokenUsage) {
    total.input_tokens = Some(total.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
    total.output_tokens = Some(total.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
    total.cache_read_tokens = match (total.cache_read_tokens, u.cache_read_tokens) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    for (class, inner) in &u.breakdowns {
        let target = total.breakdowns.entry(class.clone()).or_default();
        for (sub, n) in inner {
            *target.entry(sub.clone()).or_insert(0) += n;
        }
    }
}

/// Convert a decoded [`Conversation`] into a [`ConversationView`].
pub fn to_view(convo: &Conversation) -> ConversationView {
    let mut items: Vec<Item> = Vec::new();
    let mut current: Option<Turn> = None;

    let flush = |current: &mut Option<Turn>, items: &mut Vec<Item>| {
        if let Some(t) = current.take()
            && !t.is_content_empty()
        {
            items.push(Item::Turn(t));
        }
    };

    for step in &convo.steps {
        if let Some(text) = step.user_text.as_deref().filter(|s| !s.is_empty()) {
            flush(&mut current, &mut items);
            let mut user_turn = blank_turn(format!("{}-{}", convo.id, step.idx), None, Role::User);
            user_turn.timestamp = step.created_at.map(|t| t.to_rfc3339()).unwrap_or_default();
            user_turn.text = text.to_string();
            let user_turn_id = user_turn.id.clone();
            items.push(Item::Turn(user_turn));
            // The assistant turn that answers this user turn starts lazily
            // on the next step that carries a tool call or response text.
            current = Some(blank_turn(
                format!("{}-{}-r", convo.id, step.idx),
                Some(user_turn_id),
                Role::Assistant,
            ));
            continue;
        }

        let (tool_use, text, usage) = step_to_turn_pieces(step);
        if tool_use.is_none() && text.is_none() && usage.is_none() {
            continue; // nothing this adapter decodes from this step type
        }
        let turn = current.get_or_insert_with(|| {
            blank_turn(
                format!("{}-{}-r", convo.id, step.idx),
                None,
                Role::Assistant,
            )
        });
        if turn.timestamp.is_empty() {
            turn.timestamp = step.created_at.map(|t| t.to_rfc3339()).unwrap_or_default();
        }
        if let Some(tu) = tool_use {
            turn.tool_uses.push(tu);
        }
        if let Some(text) = text {
            if turn.text.is_empty() {
                turn.text = text.to_string();
            } else {
                turn.text.push('\n');
                turn.text.push_str(text);
            }
            turn.stop_reason = step.stop_reason.map(|r| r.to_string());
        }
        if let Some(u) = usage {
            turn.token_usage = Some(match turn.token_usage.take() {
                Some(mut total) => {
                    add_usage(&mut total, &u);
                    total
                }
                None => u,
            });
            // Every Antigravity generation is served by a Google model
            // (the closed ModelUsageStats.model enum); we can set the
            // provider family without being able to resolve the enum to
            // a model name string.
            turn.provider = Some("google".to_string());
        }
    }
    flush(&mut current, &mut items);

    let total_usage = sum_usage(&items);
    let files_changed = extract_files_changed(&items);

    ConversationView {
        id: convo.id.clone(),
        started_at: convo.steps.first().and_then(|s| s.created_at),
        last_activity: convo
            .steps
            .last()
            .and_then(|s| s.completed_at.or(s.created_at)),
        items,
        total_usage,
        provider_id: Some("antigravity".into()),
        files_changed,
        session_ids: vec![],
        base: None,
        producer: Some(ProducerInfo {
            name: "antigravity-cli".into(),
            version: None,
        }),
    }
}

fn sum_usage(items: &[Item]) -> Option<TokenUsage> {
    let mut total = TokenUsage::default();
    let mut any = false;
    for u in items
        .iter()
        .filter_map(Item::as_turn)
        .filter_map(|t| t.token_usage.as_ref())
    {
        any = true;
        add_usage(&mut total, u);
    }
    if any { Some(total) } else { None }
}

fn extract_files_changed(items: &[Item]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for turn in items.iter().filter_map(Item::as_turn) {
        for tu in &turn.tool_uses {
            if tu.category != Some(ToolCategory::FileWrite) {
                continue;
            }
            let Some(path) = tu
                .input
                .get("Path")
                .or_else(|| tu.input.get("path"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            if seen.insert(path.to_string()) {
                files.push(path.to_string());
            }
        }
    }
    files
}

impl ConversationProvider for AntigravityConvo {
    fn list_conversations(&self, _project: &str) -> toolpath_convo::Result<Vec<String>> {
        // Antigravity has no per-project slot (flat `conversations/`,
        // see `docs/agents/formats/antigravity.md`): `workspace_uris`'
        // encoding is unconfirmed, so this returns every conversation
        // regardless of `project` rather than guess at a filter.
        self.io
            .list_conversations()
            .map_err(|e| ConvoError::Provider(e.to_string()))
    }

    fn load_conversation(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationView> {
        let convo = self
            .io
            .read_conversation(conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(to_view(&convo))
    }

    fn load_metadata(
        &self,
        _project: &str,
        conversation_id: &str,
    ) -> toolpath_convo::Result<ConversationMeta> {
        let convo = self
            .io
            .read_conversation(conversation_id)
            .map_err(|e| ConvoError::Provider(e.to_string()))?;
        Ok(ConversationMeta {
            id: convo.id.clone(),
            started_at: convo.steps.first().and_then(|s| s.created_at),
            last_activity: convo
                .steps
                .last()
                .and_then(|s| s.completed_at.or(s.created_at)),
            message_count: convo.steps.len(),
            file_path: self.io.resolver().conversation_path(conversation_id).ok(),
            predecessor: None,
            successor: None,
        })
    }

    fn list_metadata(&self, project: &str) -> toolpath_convo::Result<Vec<ConversationMeta>> {
        let ids = ConversationProvider::list_conversations(self, project)?;
        ids.iter()
            .map(|id| self.load_metadata(project, id))
            .collect()
    }
}
