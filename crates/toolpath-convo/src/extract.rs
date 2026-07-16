//! Reconstruct a [`ConversationView`] from a toolpath [`Path`] using the
//! conversation sub-protocol.
//!
//! The sub-protocol uses three structural change types:
//!
//! - **`conversation.init`** — sets session metadata (provider, session ID)
//! - **`conversation.append`** — adds a turn (user or assistant message)
//! - **`tool.invoke`** — attaches a tool invocation to a parent turn

use std::collections::{HashMap, HashSet};

use chrono::DateTime;
use toolpath::v1::{Path, Step};

use crate::{
    Compaction, CompactionTrigger, ConversationEvent, ConversationView, DelegatedWork,
    EnvironmentSnapshot, FileMutation, Item, ProducerInfo, Role, SessionBase, TokenUsage,
    ToolCategory, ToolInvocation, ToolResult, Turn,
};

/// Extract a [`ConversationView`] from a toolpath [`Path`] document.
///
/// Steps are walked in order (they are already topologically sorted in the
/// path). Structural changes with types `conversation.init`,
/// `conversation.append`, and `tool.invoke` are recognized; everything else
/// is silently skipped.
pub fn extract_conversation(path: &Path) -> ConversationView {
    let mut view = ConversationView::default();

    // Project `path.base` back to `view.base`.
    if let Some(base) = &path.path.base {
        let working_dir = base
            .uri
            .strip_prefix("file://")
            .map(|s| s.to_string())
            .or_else(|| {
                if base.uri.is_empty() {
                    None
                } else {
                    Some(base.uri.clone())
                }
            });
        let vcs_remote = path
            .meta
            .as_ref()
            .and_then(|m| m.extra.get("vcs_remote"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let sb = SessionBase {
            working_dir,
            vcs_revision: base.ref_str.clone(),
            vcs_branch: base.branch.clone(),
            vcs_remote,
        };
        if sb.working_dir.is_some()
            || sb.vcs_revision.is_some()
            || sb.vcs_branch.is_some()
            || sb.vcs_remote.is_some()
        {
            view.base = Some(sb);
        }
    }

    // Recover canonical session-level fields from `path.meta.extra`.
    // Unrecognized keys are dropped — the IR is the cross-harness contract.
    if let Some(meta) = &path.meta
        && let Some(p) = meta
            .extra
            .get("producer")
            .and_then(|v| serde_json::from_value::<ProducerInfo>(v.clone()).ok())
    {
        view.producer = Some(p);
    }

    // Map from step ID → index into view.items (of a turn item), for
    // parent lookups when attaching tool invocations.
    let mut step_to_turn: HashMap<&str, usize> = HashMap::new();
    // Map from event step ID → that step's first parent, for undoing
    // derive's `splice_onto_intervening` when rebuilding turn/compaction
    // parents (see `parent_past_events`).
    let mut event_parents: HashMap<String, (usize, Option<String>)> = HashMap::new();
    // Track files_changed for dedup in insertion order.
    let mut files_seen: HashSet<String> = HashSet::new();

    for (step_idx, step) in path.steps.iter().enumerate() {
        // Pre-collect file.write entries on this step. They attach to the
        // turn built from this step's `conversation.append` change (below);
        // the iteration order of `step.change` (HashMap) is non-deterministic
        // so a pre-pass keeps the attach step simple. Sorted by path for
        // determinism on the way back out.
        let mut step_mutations: Vec<FileMutation> = Vec::new();
        for (key, ch) in &step.change {
            let Some(s) = &ch.structural else { continue };
            if s.change_type != "file.write" {
                continue;
            }
            let fm = FileMutation {
                path: key.clone(),
                tool_id: s
                    .extra
                    .get("tool_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                operation: s
                    .extra
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                raw_diff: ch.raw.clone(),
                before: s
                    .extra
                    .get("before")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                after: s
                    .extra
                    .get("after")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                rename_to: s
                    .extra
                    .get("rename_to")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            };
            step_mutations.push(fm);
        }
        step_mutations.sort_by(|a, b| a.path.cmp(&b.path));

        for (artifact_key, artifact_change) in &step.change {
            let structural = match &artifact_change.structural {
                Some(s) => s,
                None => continue,
            };

            // The shared-derive path doesn't emit conversation.init; it
            // encodes provider + session in the artifact key of every
            // conversation step (e.g. `gemini-cli://<session>`). Pick them
            // up from the first one — append, event, or compact — so a
            // path with no turns still keeps its identity.
            if matches!(
                structural.change_type.as_str(),
                "conversation.append" | "conversation.event" | "conversation.compact"
            ) && view.id.is_empty()
                && let Some((provider, session)) = artifact_key.split_once("://")
                && !provider.is_empty()
                && !session.is_empty()
            {
                view.provider_id = Some(provider.to_string());
                view.id = session.to_string();
            }

            match structural.change_type.as_str() {
                "conversation.init" => {
                    handle_init(&mut view, artifact_key, &structural.extra);
                }
                "conversation.append" => {
                    let mut turn = build_turn(step, &structural.extra);
                    turn.parent_id = restore_source_parent(
                        turn.parent_id.take(),
                        &structural.extra,
                        step_idx,
                        &event_parents,
                    );
                    // Attach pre-collected file mutations to the turn.
                    // `tool_id` on each mutation links back to the
                    // specific `ToolInvocation` (when set by derive).
                    if !step_mutations.is_empty() {
                        turn.file_mutations = std::mem::take(&mut step_mutations);
                    }
                    let idx = view.items.len();
                    step_to_turn.insert(&step.step.id, idx);
                    view.items.push(Item::Turn(turn));
                }
                "conversation.event" => {
                    let event_type = structural
                        .extra
                        .get("entry_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    // Restore the provider's original event id (e.g. the
                    // source UUID for a Claude attachment). Falls back to
                    // the synthetic step id for events that didn't have one.
                    let id = structural
                        .extra
                        .get("event_source_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| step.step.id.clone());
                    // Strip the housekeeping keys we added in derive so the
                    // event's data round-trips clean. Restore the original
                    // `type` key from `event_data_type` if it was stashed.
                    let mut data = structural.extra.clone();
                    data.remove("entry_type");
                    data.remove("event_source_id");
                    if let Some(t) = data.remove("event_data_type") {
                        data.insert("type".to_string(), t);
                    }

                    let event = ConversationEvent {
                        id,
                        timestamp: step.step.timestamp.clone(),
                        parent_id: step.step.parents.first().cloned(),
                        event_type,
                        data,
                    };
                    event_parents.insert(
                        step.step.id.clone(),
                        (step_idx, step.step.parents.first().cloned()),
                    );
                    view.items.push(Item::Event(event));
                }
                "conversation.compact" => {
                    let trigger = structural
                        .extra
                        .get("trigger")
                        .and_then(|v| v.as_str())
                        .and_then(|s| match s {
                            "auto" => Some(CompactionTrigger::Auto),
                            "manual" => Some(CompactionTrigger::Manual),
                            _ => None,
                        });
                    let summary = structural
                        .extra
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let pre_tokens = structural.extra.get("pre_tokens").and_then(|v| v.as_u64());
                    // The wire carries the expanded kept run, oldest first;
                    // the anchor is its first element.
                    let kept_from = structural
                        .extra
                        .get("kept")
                        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                        .and_then(|kept| kept.into_iter().next());
                    let extra = structural
                        .extra
                        .get("extra")
                        .and_then(|v| {
                            serde_json::from_value::<HashMap<String, serde_json::Value>>(v.clone())
                                .ok()
                        })
                        .unwrap_or_default();
                    let compaction = Compaction {
                        id: step.step.id.clone(),
                        parent_id: restore_source_parent(
                            step.step.parents.first().cloned(),
                            &structural.extra,
                            step_idx,
                            &event_parents,
                        ),
                        timestamp: step.step.timestamp.clone(),
                        trigger,
                        summary,
                        pre_tokens,
                        kept_from,
                        extra,
                    };
                    view.items.push(Item::Compaction(compaction));
                }
                "tool.invoke" => {
                    let invocation = build_tool_invocation(&structural.extra);

                    // Track files_changed for file_write tools with non agent:// keys.
                    let category = parse_category(structural.extra.get("category"));
                    if category == Some(ToolCategory::FileWrite)
                        && !artifact_key.starts_with("agent://")
                        && files_seen.insert(artifact_key.clone())
                    {
                        view.files_changed.push(artifact_key.clone());
                    }

                    // Attach to parent turn.
                    if let Some(parent_id) = step.step.parents.first()
                        && let Some(&turn_idx) = step_to_turn.get(parent_id.as_str())
                        && let Some(Item::Turn(t)) = view.items.get_mut(turn_idx)
                    {
                        t.tool_uses.push(invocation);
                    }
                }
                _ => {
                    // Unknown structural change type — silently skip.
                }
            }
        }
    }

    // Compute total_usage by summing across turns.
    let mut has_any_usage = false;
    let mut total = TokenUsage::default();
    for turn in view.turns() {
        if let Some(usage) = &turn.token_usage {
            has_any_usage = true;
            total.input_tokens = add_opt(total.input_tokens, usage.input_tokens);
            total.output_tokens = add_opt(total.output_tokens, usage.output_tokens);
            total.cache_read_tokens = add_opt(total.cache_read_tokens, usage.cache_read_tokens);
            total.cache_write_tokens = add_opt(total.cache_write_tokens, usage.cache_write_tokens);
        }
    }
    if has_any_usage {
        view.total_usage = Some(total);
    }

    // Parse timestamps from first/last turns. Clone the strings out first
    // so the `turns()` borrow ends before we assign back into `view`.
    let first_ts = view.turns().next().map(|t| t.timestamp.clone());
    let last_ts = view.turns().last().map(|t| t.timestamp.clone());
    if let Some(ts) = first_ts {
        view.started_at = DateTime::parse_from_rfc3339(&ts)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }
    if let Some(ts) = last_ts {
        view.last_activity = DateTime::parse_from_rfc3339(&ts)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }

    view
}

/// Resolve a turn's or compaction's parent past any event-derived steps,
/// back to the nearest turn/compaction ancestor (or `None` at the root).
///
/// Providers never build a view in which a turn or compaction parents on an
/// event — the wire formats chain messages to messages (Claude even rewrites
/// tool-result parents onto the owning assistant turn at read time). So an
/// event step in a turn's `step.parents` can only have been put there by
/// derive's `splice_onto_intervening`, which re-parents through events to
/// keep them on the head's ancestry. Walking past event steps undoes that
/// splice, recovering the source-recorded parent — otherwise projectors
/// would write wire chains through ids that don't exist on the wire (e.g. a
/// Claude `parentUuid` naming a synthesized `claude-preamble-0` step).
///
/// Events themselves keep their spliced parents: event-to-event chains are
/// legitimate wire data (Claude chains consecutive tool-result entries), and
/// `derive_path` re-splices on the way back in, so the round-trip is stable.
/// Restore a turn's or compaction's source parent. Steps whose parents the
/// derive splice rewired carry the pre-splice parent in `source_parent`
/// (`null` = root) — read it back verbatim. Documents derived before that
/// key existed fall back to [`parent_past_events`]'s best-effort walk.
fn restore_source_parent(
    parent: Option<String>,
    extra: &HashMap<String, serde_json::Value>,
    step_idx: usize,
    event_parents: &HashMap<String, (usize, Option<String>)>,
) -> Option<String> {
    match extra.get("source_parent") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Null) => None,
        _ => parent_past_events(parent, step_idx, event_parents),
    }
}

fn parent_past_events(
    parent: Option<String>,
    step_idx: usize,
    event_parents: &HashMap<String, (usize, Option<String>)>,
) -> Option<String> {
    let mut current = parent;
    let mut at = step_idx;
    // The derive splice always rewires onto the immediately-preceding step,
    // so only adjacent event hops are splice artifacts. A parent naming an
    // event elsewhere in the path is native linkage and stays untouched.
    // `at` strictly decreases, so the walk terminates on any input.
    loop {
        match current.as_deref().and_then(|id| event_parents.get(id)) {
            Some((event_idx, next)) if event_idx + 1 == at => {
                at = *event_idx;
                current = next.clone();
            }
            _ => return current,
        }
    }
}

fn handle_init(
    view: &mut ConversationView,
    artifact_key: &str,
    extra: &HashMap<String, serde_json::Value>,
) {
    // Artifact key: agent://<provider>/<session-id>
    if let Some(rest) = artifact_key.strip_prefix("agent://") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            view.provider_id = Some(parts[0].to_string());
            view.id = parts[1].to_string();
        }
    }

    // Also check extra for explicit values.
    if let Some(serde_json::Value::String(v)) = extra.get("version") {
        // Store version in session_ids as a convention, or just note it.
        // For now, version is informational and not mapped to ConversationView fields.
        let _ = v;
    }
}

fn build_turn(step: &Step, extra: &HashMap<String, serde_json::Value>) -> Turn {
    let role = if let Some(serde_json::Value::String(r)) = extra.get("role") {
        parse_role(r)
    } else {
        role_from_actor(&step.step.actor)
    };

    let text = extra
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let thinking = extra
        .get("thinking")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Model is attributed via the step actor (`agent:{model}`).
    let model = model_from_actor(&step.step.actor);

    let stop_reason = extra
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let token_usage = build_token_usage(extra);

    let environment = build_environment(extra);

    let tool_uses = build_inline_tool_uses(extra);

    let delegations = build_delegations(extra);

    let parent_id = step.step.parents.first().cloned();

    let group_id = extra
        .get("group_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let attributed_token_usage = extra
        .get("attributed_token_usage")
        .and_then(|v| serde_json::from_value::<TokenUsage>(v.clone()).ok());

    Turn {
        id: step.step.id.clone(),
        parent_id,
        group_id,
        role,
        timestamp: step.step.timestamp.clone(),
        text,
        thinking,
        tool_uses,
        model,
        stop_reason,
        token_usage,
        attributed_token_usage,
        environment,
        delegations,
        file_mutations: Vec::new(),
    }
}

/// Build `Turn.environment` by preferring a nested `environment` object
/// (shared-derive schema) and falling back to top-level `cwd`/`git_branch`
/// (Claude's bespoke schema).
fn build_environment(extra: &HashMap<String, serde_json::Value>) -> Option<EnvironmentSnapshot> {
    if let Some(v) = extra.get("environment")
        && let Ok(env) = serde_json::from_value::<EnvironmentSnapshot>(v.clone())
    {
        return Some(env);
    }
    let cwd = extra
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let branch = extra
        .get("git_branch")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if cwd.is_some() || branch.is_some() {
        Some(EnvironmentSnapshot {
            working_dir: cwd,
            vcs_branch: branch,
            vcs_revision: None,
        })
    } else {
        None
    }
}

/// Rehydrate tool invocations stored inline on a `conversation.append` step
/// by the shared derive pipeline. Each entry carries `id`, `name`, `input`,
/// `category`, and optionally `result`.
fn build_inline_tool_uses(extra: &HashMap<String, serde_json::Value>) -> Vec<ToolInvocation> {
    let Some(arr) = extra.get("tool_uses").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let obj = entry.as_object()?;
            let id = obj.get("id")?.as_str()?.to_string();
            let name = obj.get("name")?.as_str()?.to_string();
            let input = obj.get("input").cloned().unwrap_or(serde_json::Value::Null);
            let category = parse_category(obj.get("category"));
            let result = obj
                .get("result")
                .and_then(|v| serde_json::from_value::<ToolResult>(v.clone()).ok());
            Some(ToolInvocation {
                id,
                name,
                input,
                result,
                category,
            })
        })
        .collect()
}

/// Rehydrate `Turn.delegations` stored on a `conversation.append` step.
fn build_delegations(extra: &HashMap<String, serde_json::Value>) -> Vec<DelegatedWork> {
    extra
        .get("delegations")
        .and_then(|v| serde_json::from_value::<Vec<DelegatedWork>>(v.clone()).ok())
        .unwrap_or_default()
}

fn build_token_usage(extra: &HashMap<String, serde_json::Value>) -> Option<TokenUsage> {
    // Shared-derive schema: nested `token_usage` object.
    if let Some(v) = extra.get("token_usage")
        && let Ok(usage) = serde_json::from_value::<TokenUsage>(v.clone())
    {
        return Some(usage);
    }

    // Claude bespoke schema: fields live at the top level of the extras.
    let input = extra
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let output = extra
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let cache_read = extra
        .get("cache_read_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let cache_write = extra
        .get("cache_write_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    if input.is_some() || output.is_some() || cache_read.is_some() || cache_write.is_some() {
        Some(TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            ..Default::default()
        })
    } else {
        None
    }
}

fn build_tool_invocation(extra: &HashMap<String, serde_json::Value>) -> ToolInvocation {
    let id = extra
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let name = extra
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let input = extra
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let is_error = extra
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let result_content = extra.get("result").and_then(|v| v.as_str());
    let result = result_content.map(|content| ToolResult {
        content: content.to_string(),
        is_error,
    });

    let category = parse_category(extra.get("category"));

    ToolInvocation {
        id,
        name,
        input,
        result,
        category,
    }
}

fn parse_category(value: Option<&serde_json::Value>) -> Option<ToolCategory> {
    value
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
}

fn parse_role(s: &str) -> Role {
    match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        other => Role::Other(other.to_string()),
    }
}

/// Pull the model name out of a step actor string like `agent:claude-opus-4-7`.
///
/// Conventions:
/// - `agent:{model}` → `Some("{model}")` (the standard attribution shape)
/// - `agent:{model}/tool:…` → model is the part before the `/` (Claude's
///   sub-actor style; only appears on non-turn tool steps, but handled for
///   robustness)
/// - `agent:unknown` → `None` — "unknown" is the sentinel the deriver writes
///   when the source has no model
/// - anything else (`human:…`, `system:…`, empty) → `None`
fn model_from_actor(actor: &str) -> Option<String> {
    let rest = actor.strip_prefix("agent:")?;
    let model = match rest.split_once('/') {
        Some((m, _)) => m,
        None => rest,
    };
    if model.is_empty() || model == "unknown" {
        None
    } else {
        Some(model.to_string())
    }
}

fn role_from_actor(actor: &str) -> Role {
    if actor.contains("/tool:") {
        // Tool step — shouldn't be a turn, but if it is, treat as Other.
        Role::Other("tool".to_string())
    } else if actor.starts_with("human:") {
        Role::User
    } else if actor.starts_with("agent:") {
        Role::Assistant
    } else if actor.starts_with("tool:") {
        Role::System
    } else {
        Role::Other(actor.to_string())
    }
}

fn add_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use toolpath::v1::{ArtifactChange, PathIdentity, StructuralChange};

    #[test]
    fn test_model_from_actor_variants() {
        assert_eq!(
            model_from_actor("agent:claude-opus-4-7"),
            Some("claude-opus-4-7".to_string())
        );
        assert_eq!(
            model_from_actor("agent:gemini-3-flash-preview"),
            Some("gemini-3-flash-preview".to_string())
        );
        // Sub-actor form (Claude tool steps): model is the part before "/".
        assert_eq!(
            model_from_actor("agent:claude-code/tool:Write"),
            Some("claude-code".to_string())
        );
        // `unknown` is the deriver's sentinel for "no model"; decode to None.
        assert_eq!(model_from_actor("agent:unknown"), None);
        // Non-agent actors carry no model.
        assert_eq!(model_from_actor("human:user"), None);
        assert_eq!(model_from_actor("system:gemini-cli"), None);
        assert_eq!(model_from_actor("tool:rustfmt"), None);
        // Malformed / empty.
        assert_eq!(model_from_actor(""), None);
        assert_eq!(model_from_actor("agent:"), None);
    }

    fn make_path(steps: Vec<Step>) -> Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        Path {
            path: PathIdentity {
                id: "test-path".into(),
                base: None,
                head,
                graph_ref: None,
            },
            steps,
            meta: None,
        }
    }

    fn make_step(
        id: &str,
        actor: &str,
        timestamp: &str,
        parents: Vec<&str>,
        changes: Vec<(&str, &str, HashMap<String, serde_json::Value>)>,
    ) -> Step {
        let mut change = HashMap::new();
        for (key, change_type, extra) in changes {
            change.insert(
                key.to_string(),
                ArtifactChange {
                    raw: None,
                    structural: Some(StructuralChange {
                        change_type: change_type.to_string(),
                        extra,
                    }),
                },
            );
        }
        Step {
            step: toolpath::v1::StepIdentity {
                id: id.to_string(),
                parents: parents.into_iter().map(String::from).collect(),
                actor: actor.to_string(),
                timestamp: timestamp.to_string(),
            },
            change,
            meta: None,
        }
    }

    fn extras(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn test_empty_path() {
        let path = make_path(vec![]);
        let view = extract_conversation(&path);
        assert!(view.id.is_empty());
        assert!(view.turns().next().is_none());
        assert!(view.total_usage.is_none());
        assert!(view.started_at.is_none());
        assert!(view.last_activity.is_none());
        assert!(view.files_changed.is_empty());
    }

    #[test]
    fn test_init_sets_metadata() {
        let path = make_path(vec![make_step(
            "step-001",
            "tool:claude-code",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-abc",
                "conversation.init",
                extras(&[("version", serde_json::json!("1.0"))]),
            )],
        )]);

        let view = extract_conversation(&path);
        assert_eq!(view.id, "sess-abc");
        assert_eq!(view.provider_id.as_deref(), Some("claude-code"));
    }

    #[test]
    fn test_simple_conversation() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "tool:claude-code",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.init",
                    HashMap::new(),
                )],
            ),
            make_step(
                "step-002",
                "human:alex",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("Fix the bug")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("I'll fix that.")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].text, "Fix the bug");
        assert_eq!(turns[0].id, "step-002");
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[1].text, "I'll fix that.");
        assert_eq!(turns[1].model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn test_group_id_round_trips_through_extraction() {
        let path = make_path(vec![make_step(
            "step-001",
            "agent:claude-opus-4-6",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "claude-code://sess-1",
                "conversation.append",
                extras(&[
                    ("role", serde_json::json!("assistant")),
                    ("text", serde_json::json!("")),
                    ("group_id", serde_json::json!("msg_01abc")),
                ]),
            )],
        )]);

        let view = extract_conversation(&path);
        assert_eq!(
            view.turns().next().unwrap().group_id.as_deref(),
            Some("msg_01abc")
        );
    }

    #[test]
    fn test_tool_invocations_attached_to_parent() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Let me read the file.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/main.rs"})),
                        ("result", serde_json::json!("fn main() {}")),
                        ("is_error", serde_json::json!(false)),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_uses.len(), 1);
        assert_eq!(turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(turns[0].tool_uses[0].name, "Read");
        assert_eq!(turns[0].tool_uses[0].category, Some(ToolCategory::FileRead));
        assert!(turns[0].tool_uses[0].result.is_some());
        assert!(!turns[0].tool_uses[0].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_token_usage_extracted_and_totaled() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("hi")),
                        ("input_tokens", serde_json::json!(100)),
                        ("output_tokens", serde_json::json!(50)),
                        ("cache_read_tokens", serde_json::json!(80)),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("more")),
                        ("input_tokens", serde_json::json!(200)),
                        ("output_tokens", serde_json::json!(100)),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let total = view.total_usage.as_ref().unwrap();
        assert_eq!(total.input_tokens, Some(300));
        assert_eq!(total.output_tokens, Some(150));
        assert_eq!(total.cache_read_tokens, Some(80));
        assert!(total.cache_write_tokens.is_none());
    }

    #[test]
    fn test_thinking_blocks_extracted() {
        let path = make_path(vec![make_step(
            "step-001",
            "agent:claude-opus-4-6",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-1",
                "conversation.append",
                extras(&[
                    ("role", serde_json::json!("assistant")),
                    ("text", serde_json::json!("The answer is 42.")),
                    (
                        "thinking",
                        serde_json::json!("Let me think about this carefully..."),
                    ),
                ]),
            )],
        )]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].thinking.as_deref(),
            Some("Let me think about this carefully...")
        );
    }

    #[test]
    fn test_parent_chain_preserved() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("first")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("second")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert!(turns[0].parent_id.is_none());
        assert_eq!(turns[1].parent_id.as_deref(), Some("step-001"));
    }

    #[test]
    fn test_unknown_structural_change_skipped() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "some.future.type",
                    extras(&[("data", serde_json::json!("whatever"))]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // Only the conversation.append step becomes a turn.
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "hello");
    }

    #[test]
    fn test_role_fallback_from_actor() {
        // No "role" extra — should infer from actor pattern.
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("hello"))]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("hi back"))]),
                )],
            ),
            make_step(
                "step-003",
                "tool:system-prompt",
                "2026-01-01T00:00:02Z",
                vec!["step-002"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[("text", serde_json::json!("system message"))]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[2].role, Role::System);
    }

    #[test]
    fn test_multiple_tool_invocations_same_turn() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Let me check two files.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/main.rs"})),
                        ("result", serde_json::json!("fn main() {}")),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6/tool:Read",
                "2026-01-01T00:00:02Z",
                vec!["step-001"],
                vec![(
                    "src/lib.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-002")),
                        ("name", serde_json::json!("Read")),
                        ("input", serde_json::json!({"file_path": "src/lib.rs"})),
                        ("result", serde_json::json!("pub mod foo;")),
                        ("category", serde_json::json!("file_read")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_uses.len(), 2);
        assert_eq!(turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(turns[0].tool_uses[1].id, "tu-002");
    }

    #[test]
    fn test_files_changed_from_file_write_tools() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Writing files.")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:Edit",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("Edit")),
                        ("input", serde_json::json!({})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
            make_step(
                "step-003",
                "agent:claude-opus-4-6/tool:Edit",
                "2026-01-01T00:00:02Z",
                vec!["step-001"],
                vec![(
                    "src/main.rs",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-002")),
                        ("name", serde_json::json!("Edit")),
                        ("input", serde_json::json!({})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // Deduped — src/main.rs appears only once.
        assert_eq!(view.files_changed, vec!["src/main.rs"]);
    }

    #[test]
    fn test_timestamps_parsed() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "human:alex",
                "2026-01-01T10:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6",
                "2026-01-01T10:05:00Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("hi")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        assert!(view.started_at.is_some());
        assert!(view.last_activity.is_some());
        assert!(view.last_activity.unwrap() > view.started_at.unwrap());
    }

    #[test]
    fn test_steps_without_structural_changes_skipped() {
        let path = make_path(vec![make_step(
            "step-001",
            "human:alex",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![], // no changes at all
        )]);

        let view = extract_conversation(&path);
        assert!(view.turns().next().is_none());
    }

    #[test]
    fn test_environment_from_cwd_and_git_branch() {
        let path = make_path(vec![make_step(
            "step-001",
            "human:alex",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-1",
                "conversation.append",
                extras(&[
                    ("role", serde_json::json!("user")),
                    ("text", serde_json::json!("hello")),
                    ("cwd", serde_json::json!("/home/alex/project")),
                    ("git_branch", serde_json::json!("feature/cool")),
                ]),
            )],
        )]);

        let view = extract_conversation(&path);
        let env = view.turns().next().unwrap().environment.as_ref().unwrap();
        assert_eq!(env.working_dir.as_deref(), Some("/home/alex/project"));
        assert_eq!(env.vcs_branch.as_deref(), Some("feature/cool"));
        assert!(env.vcs_revision.is_none());
    }

    #[test]
    fn test_environment_none_when_absent() {
        let path = make_path(vec![make_step(
            "step-001",
            "human:alex",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-1",
                "conversation.append",
                extras(&[
                    ("role", serde_json::json!("user")),
                    ("text", serde_json::json!("hello")),
                ]),
            )],
        )]);

        let view = extract_conversation(&path);
        assert!(view.turns().next().unwrap().environment.is_none());
    }

    #[test]
    fn test_agent_url_tool_not_in_files_changed() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "agent:claude-opus-4-6",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("assistant")),
                        ("text", serde_json::json!("Searching...")),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "agent:claude-opus-4-6/tool:WebSearch",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1/tool/network/tu-001",
                    "tool.invoke",
                    extras(&[
                        ("tool_use_id", serde_json::json!("tu-001")),
                        ("name", serde_json::json!("WebSearch")),
                        ("input", serde_json::json!({"query": "rust async"})),
                        ("category", serde_json::json!("file_write")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        // agent:// URL should NOT appear in files_changed even with file_write category.
        assert!(view.files_changed.is_empty());
    }

    #[test]
    fn test_conversation_event_extracted() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "tool:claude-code",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.event",
                    extras(&[
                        ("entry_type", serde_json::json!("attachment")),
                        ("cwd", serde_json::json!("/home/alex/project")),
                        ("version", serde_json::json!("1.0.30")),
                        (
                            "entry_extra",
                            serde_json::json!({"attachment": {"fileName": "test.png"}}),
                        ),
                    ]),
                )],
            ),
            make_step(
                "step-002",
                "tool:claude-code",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.event",
                    extras(&[
                        ("entry_type", serde_json::json!("file-history-snapshot")),
                        ("snapshot", serde_json::json!({"files": []})),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let events: Vec<&ConversationEvent> = view.events().collect();
        assert!(view.turns().next().is_none());
        assert_eq!(events.len(), 2);

        assert_eq!(events[0].id, "step-001");
        assert_eq!(events[0].event_type, "attachment");
        assert_eq!(
            events[0].data["cwd"],
            serde_json::json!("/home/alex/project")
        );
        assert_eq!(events[0].data["version"], serde_json::json!("1.0.30"));
        assert!(events[0].parent_id.is_none());

        assert_eq!(events[1].id, "step-002");
        assert_eq!(events[1].event_type, "file-history-snapshot");
        assert_eq!(events[1].parent_id.as_deref(), Some("step-001"));
        assert!(events[1].data.contains_key("snapshot"));
    }

    #[test]
    fn test_conversation_event_with_unknown_type() {
        let path = make_path(vec![make_step(
            "step-001",
            "tool:claude-code",
            "2026-01-01T00:00:00Z",
            vec![],
            vec![(
                "agent://claude-code/sess-1",
                "conversation.event",
                extras(&[("cwd", serde_json::json!("/tmp"))]),
            )],
        )]);

        let view = extract_conversation(&path);
        let events: Vec<&ConversationEvent> = view.events().collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "unknown");
    }

    #[test]
    fn test_compaction_round_trips_through_derive_and_extract() {
        use crate::DeriveConfig;

        let a = Turn {
            id: "a".into(),
            parent_id: None,
            role: Role::User,
            timestamp: "2026-01-01T00:00:00Z".into(),
            text: "first".into(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
            group_id: None,
            attributed_token_usage: None,
        };
        let b = Turn {
            id: "b".into(),
            parent_id: Some("c".into()),
            role: Role::Assistant,
            timestamp: "2026-01-01T00:00:02Z".into(),
            text: "second".into(),
            thinking: None,
            tool_uses: vec![],
            model: Some("m".into()),
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
            group_id: None,
            attributed_token_usage: None,
        };
        let c = Compaction {
            id: "c".into(),
            parent_id: Some("a".into()),
            timestamp: "2026-01-01T00:00:01Z".into(),
            trigger: Some(CompactionTrigger::Manual),
            summary: Some("condensed".into()),
            pre_tokens: Some(4096),
            kept_from: Some("a".into()),
            extra: Default::default(),
        };

        let source = ConversationView {
            id: "sess-1".into(),
            items: vec![Item::Turn(a), Item::Compaction(c), Item::Turn(b)],
            provider_id: Some("claude-code".into()),
            ..Default::default()
        };

        let path = crate::derive::derive_path(&source, &DeriveConfig::default());
        let view = extract_conversation(&path);

        // Item order [Turn, Compaction, Turn] is preserved.
        assert_eq!(view.items.len(), 3);
        assert!(matches!(view.items[0], Item::Turn(_)));
        assert!(matches!(view.items[2], Item::Turn(_)));

        let Item::Compaction(rc) = &view.items[1] else {
            panic!(
                "middle item should be a compaction, got {:?}",
                view.items[1]
            );
        };
        assert_eq!(rc.id, "c");
        assert_eq!(rc.parent_id.as_deref(), Some("a"));
        assert_eq!(rc.timestamp, "2026-01-01T00:00:01Z");
        assert_eq!(rc.trigger, Some(CompactionTrigger::Manual));
        assert_eq!(rc.summary.as_deref(), Some("condensed"));
        assert_eq!(rc.pre_tokens, Some(4096));
        assert_eq!(rc.kept_from.as_deref(), Some("a"));
    }

    fn bare_turn(id: &str, parent_id: Option<&str>, role: Role, timestamp: &str) -> Turn {
        Turn {
            id: id.into(),
            parent_id: parent_id.map(|s| s.into()),
            role,
            timestamp: timestamp.into(),
            text: "text".into(),
            thinking: None,
            tool_uses: vec![],
            model: None,
            stop_reason: None,
            token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
            group_id: None,
            attributed_token_usage: None,
        }
    }

    fn bare_event(id: &str, event_type: &str, timestamp: &str) -> ConversationEvent {
        ConversationEvent {
            id: id.into(),
            timestamp: timestamp.into(),
            parent_id: None,
            event_type: event_type.into(),
            data: HashMap::new(),
        }
    }

    #[test]
    fn test_turn_parent_resolves_past_spliced_event_steps() {
        use crate::DeriveConfig;

        // Wire truth: a1's parent is u1. An id-less event (e.g. a Claude
        // file-history-snapshot line) sits between them in the stream.
        let source = ConversationView {
            id: "sess-1".into(),
            items: vec![
                Item::Turn(bare_turn("u1", None, Role::User, "2026-01-01T00:00:00Z")),
                Item::Event(bare_event(
                    "",
                    "file-history-snapshot",
                    "2026-01-01T00:00:01Z",
                )),
                Item::Turn(bare_turn(
                    "a1",
                    Some("u1"),
                    Role::Assistant,
                    "2026-01-01T00:00:02Z",
                )),
            ],
            provider_id: Some("claude-code".into()),
            ..Default::default()
        };

        // derive splices a1 onto the event step so the event lands on the
        // head's ancestry...
        let path = crate::derive::derive_path(&source, &DeriveConfig::default());
        let a1_step = path.steps.iter().find(|s| s.step.id == "a1").unwrap();
        assert_eq!(a1_step.step.parents, vec!["event-0001".to_string()]);

        // ...and extract undoes the splice, restoring the wire parent.
        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns[1].id, "a1");
        assert_eq!(turns[1].parent_id.as_deref(), Some("u1"));

        // Re-derive is stable: the splice fires again onto the same DAG.
        let again = crate::derive::derive_path(&view, &DeriveConfig::default());
        let a1_again = again.steps.iter().find(|s| s.step.id == "a1").unwrap();
        assert_eq!(a1_again.step.parents, vec!["event-0001".to_string()]);
    }

    #[test]
    fn test_first_turn_parent_resolves_past_leading_events_to_root() {
        use crate::DeriveConfig;

        // Claude hoists headerless preamble lines to the front of the item
        // stream as events; derive splices the first turn onto them. The
        // extracted turn must come back a root — its wire parentUuid is null.
        let source = ConversationView {
            id: "sess-1".into(),
            items: vec![
                Item::Event(bare_event(
                    "claude-preamble-0",
                    "ai-title",
                    "2026-01-01T00:00:00Z",
                )),
                Item::Event(bare_event(
                    "claude-preamble-1",
                    "file-history-snapshot",
                    "2026-01-01T00:00:01Z",
                )),
                Item::Turn(bare_turn("u1", None, Role::User, "2026-01-01T00:00:02Z")),
            ],
            provider_id: Some("claude-code".into()),
            ..Default::default()
        };

        let path = crate::derive::derive_path(&source, &DeriveConfig::default());
        let u1_step = path.steps.iter().find(|s| s.step.id == "u1").unwrap();
        assert_eq!(u1_step.step.parents, vec!["claude-preamble-1".to_string()]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns[0].id, "u1");
        assert_eq!(
            turns[0].parent_id, None,
            "walk resolves through the whole event chain"
        );

        // The events keep their spliced chain — event-to-event parents are
        // legitimate wire data, and re-derive re-splices identically.
        let events: Vec<&ConversationEvent> = view.events().collect();
        assert_eq!(events[1].parent_id.as_deref(), Some("claude-preamble-0"));
    }

    #[test]
    fn test_compaction_parent_resolves_past_spliced_event_steps() {
        use crate::DeriveConfig;

        let compaction = Compaction {
            id: "c".into(),
            parent_id: Some("u1".into()),
            timestamp: "2026-01-01T00:00:02Z".into(),
            trigger: Some(CompactionTrigger::Auto),
            summary: Some("condensed".into()),
            pre_tokens: None,
            kept_from: None,
            extra: Default::default(),
        };
        let source = ConversationView {
            id: "sess-1".into(),
            items: vec![
                Item::Turn(bare_turn("u1", None, Role::User, "2026-01-01T00:00:00Z")),
                Item::Event(bare_event(
                    "",
                    "file-history-snapshot",
                    "2026-01-01T00:00:01Z",
                )),
                Item::Compaction(compaction),
                Item::Turn(bare_turn(
                    "b",
                    Some("c"),
                    Role::User,
                    "2026-01-01T00:00:03Z",
                )),
            ],
            provider_id: Some("claude-code".into()),
            ..Default::default()
        };

        let path = crate::derive::derive_path(&source, &DeriveConfig::default());
        let c_step = path.steps.iter().find(|s| s.step.id == "c").unwrap();
        assert_eq!(c_step.step.parents, vec!["event-0001".to_string()]);

        let view = extract_conversation(&path);
        let Some(Item::Compaction(rc)) =
            view.items.iter().find(|i| matches!(i, Item::Compaction(_)))
        else {
            panic!("compaction survives the roundtrip");
        };
        assert_eq!(
            rc.parent_id.as_deref(),
            Some("u1"),
            "compaction's logical parent is the last turn, not the spliced event"
        );
        let turns: Vec<&Turn> = view.turns().collect();
        assert_eq!(turns[1].parent_id.as_deref(), Some("c"));
    }

    #[test]
    fn test_conversation_event_mixed_with_turns() {
        let path = make_path(vec![
            make_step(
                "step-001",
                "tool:claude-code",
                "2026-01-01T00:00:00Z",
                vec![],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.event",
                    extras(&[("entry_type", serde_json::json!("system"))]),
                )],
            ),
            make_step(
                "step-002",
                "human:alex",
                "2026-01-01T00:00:01Z",
                vec!["step-001"],
                vec![(
                    "agent://claude-code/sess-1",
                    "conversation.append",
                    extras(&[
                        ("role", serde_json::json!("user")),
                        ("text", serde_json::json!("hello")),
                    ]),
                )],
            ),
        ]);

        let view = extract_conversation(&path);
        let turns: Vec<&Turn> = view.turns().collect();
        let events: Vec<&ConversationEvent> = view.events().collect();
        assert_eq!(turns.len(), 1);
        assert_eq!(events.len(), 1);
        assert_eq!(turns[0].text, "hello");
        assert_eq!(events[0].event_type, "system");
    }
}
