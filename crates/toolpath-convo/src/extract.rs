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
    Actor, ConversationEvent, ConversationView, DelegatedWork, EnvironmentSnapshot, FileMutation,
    ProducerInfo, Role, SessionBase, TokenUsage, ToolCategory, ToolInvocation, ToolResult, Turn,
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

    // Map from step ID → index into view.turns, for parent lookups.
    let mut step_to_turn: HashMap<&str, usize> = HashMap::new();
    // Track files_changed for dedup in insertion order.
    let mut files_seen: HashSet<String> = HashSet::new();

    for step in &path.steps {
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

            match structural.change_type.as_str() {
                "conversation.init" => {
                    handle_init(&mut view, artifact_key, &structural.extra);
                }
                "conversation.append" => {
                    // The shared-derive path doesn't emit conversation.init;
                    // it encodes provider + session in the artifact key of
                    // each append step (e.g. `gemini-cli://<session>`).
                    // Pick them up the first time we see one.
                    if view.id.is_empty()
                        && let Some((provider, session)) = artifact_key.split_once("://")
                        && !provider.is_empty()
                        && !session.is_empty()
                    {
                        view.provider_id = Some(provider.to_string());
                        view.id = session.to_string();
                    }

                    let mut turn = build_turn(step, &structural.extra);
                    // Attach pre-collected file mutations to the turn.
                    // `tool_id` on each mutation links back to the
                    // specific `ToolInvocation` (when set by derive).
                    if !step_mutations.is_empty() {
                        turn.file_mutations = std::mem::take(&mut step_mutations);
                    }
                    let idx = view.turns.len();
                    step_to_turn.insert(&step.step.id, idx);
                    view.turns.push(turn);
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
                    view.events.push(event);
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
                    {
                        view.turns[turn_idx].tool_uses.push(invocation);
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
    for turn in &view.turns {
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

    // Parse timestamps from first/last turns.
    if let Some(first) = view.turns.first() {
        view.started_at = DateTime::parse_from_rfc3339(&first.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }
    if let Some(last) = view.turns.last() {
        view.last_activity = DateTime::parse_from_rfc3339(&last.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc));
    }

    view
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

    // Authorship is attributed via the step actor.
    let author = author_from_actor(&step.step.actor);

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
        author,
        timestamp: step.step.timestamp.clone(),
        text,
        thinking,
        tool_uses,
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

/// Recover a turn's author from its step actor string — the inverse of the
/// attribution the deriver writes, so derive → extract → derive is stable.
///
/// [`Actor`] reads the reference, whatever the prefix, and drops the sub-actor
/// suffix of the form `agent:{model}/tool:…` (which only appears on non-turn
/// tool steps, but is handled for robustness).
///
/// A string that is not an actor reference at all — no prefix, an empty
/// segment, characters outside the grammar — decodes to an unnamed agent,
/// which is what an actor of no recognizable shape has always meant here.
fn author_from_actor(actor: &str) -> Actor {
    actor
        .parse()
        .unwrap_or_else(|_| crate::actor::unnamed_agent())
}

fn role_from_actor(actor: &str) -> Role {
    let (base, sub) = Actor::split_sub_actor(actor);
    if matches!(sub.map(str::parse::<Actor>), Some(Ok(ref a)) if crate::actor::is_tool(a)) {
        // Tool step — shouldn't be a turn, but if it is, treat as Other.
        return Role::Other("tool".to_string());
    }
    // Only the prefixes this crate attributes turns to map to a role; any
    // other actor is something else's, and keeps its reference as the label.
    match base.parse::<Actor>() {
        Ok(a) if crate::actor::is_human(&a) => Role::User,
        Ok(a) if crate::actor::is_agent(&a) => Role::Assistant,
        Ok(a) if crate::actor::is_tool(&a) => Role::System,
        _ => Role::Other(actor.to_string()),
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

    fn agent(name: &str) -> Actor {
        crate::actor::agent(Some(name))
    }

    #[test]
    fn test_author_from_actor_variants() {
        // The generic user and a named one.
        assert_eq!(
            author_from_actor("human:user"),
            crate::actor::generic_human()
        );
        assert_eq!(
            author_from_actor("human:ada"),
            crate::actor::human(Some("ada"))
        );
        // Model calls, named and not.
        assert_eq!(
            author_from_actor("agent:claude-opus-4-7"),
            agent("claude-opus-4-7")
        );
        assert_eq!(
            author_from_actor("agent:gemini-3-flash-preview"),
            agent("gemini-3-flash-preview")
        );
        assert_eq!(
            author_from_actor("agent:unknown"),
            crate::actor::unnamed_agent()
        );
        // Sub-actor form (tool steps): model is the part before "/".
        assert_eq!(
            author_from_actor("agent:claude-code/tool:Write"),
            agent("claude-code")
        );
        // The harness itself.
        assert_eq!(
            author_from_actor("tool:gemini-cli"),
            crate::actor::harness("gemini-cli")
        );
        // The grammar is open, so a prefix this crate has no convention for
        // still reads back as the actor it is.
        assert_eq!(
            author_from_actor("system:gemini-cli").to_string(),
            "system:gemini-cli"
        );
        // Anything that is not an actor reference reads as an unnamed agent.
        assert_eq!(author_from_actor(""), crate::actor::unnamed_agent());
        assert_eq!(author_from_actor("agent:"), crate::actor::unnamed_agent());
        assert_eq!(author_from_actor("tool:"), crate::actor::unnamed_agent());
        assert_eq!(
            author_from_actor("no-prefix"),
            crate::actor::unnamed_agent()
        );
    }

    #[test]
    fn test_role_from_actor_variants() {
        assert_eq!(role_from_actor("human:alex"), Role::User);
        assert_eq!(role_from_actor("agent:claude-opus-4-7"), Role::Assistant);
        assert_eq!(role_from_actor("tool:pi"), Role::System);
        // A sub-actor naming a tool is a tool step, not a turn.
        assert_eq!(
            role_from_actor("agent:claude-code/tool:Write"),
            Role::Other("tool".to_string())
        );
        // A non-tool suffix qualifies the actor and doesn't change its kind.
        assert_eq!(role_from_actor("tool:rustfmt/1.5.0"), Role::System);
        assert_eq!(
            role_from_actor("ci:github-actions"),
            Role::Other("ci:github-actions".to_string())
        );
    }

    #[test]
    fn test_author_survives_derive_extract_derive() {
        use crate::{ConversationView, DeriveConfig, derive_path};

        let base = |id: &str, role: Role, author: Actor| Turn {
            id: id.to_string(),
            parent_id: None,
            group_id: None,
            role,
            author,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            text: "t".to_string(),
            thinking: None,
            tool_uses: vec![],
            stop_reason: None,
            token_usage: None,
            attributed_token_usage: None,
            environment: None,
            delegations: vec![],
            file_mutations: vec![],
        };

        let view = ConversationView {
            id: "s1".to_string(),
            provider_id: Some("pi".to_string()),
            turns: vec![
                base("t1", Role::User, crate::actor::generic_human()),
                base("t2", Role::Assistant, agent("claude-opus-4-7")),
                base("t3", Role::Assistant, crate::actor::unnamed_agent()),
                // The harness speaking in the assistant slot.
                base("t4", Role::Assistant, crate::actor::harness("pi")),
                base("t5", Role::System, crate::actor::harness("pi")),
            ],
            ..Default::default()
        };

        let first = derive_path(&view, &DeriveConfig::default());
        let back = extract_conversation(&first);
        let second = derive_path(&back, &DeriveConfig::default());

        let actors =
            |p: &Path| -> Vec<String> { p.steps.iter().map(|s| s.step.actor.clone()).collect() };
        assert_eq!(
            actors(&first),
            vec![
                "human:user",
                "agent:claude-opus-4-7",
                "agent:unknown",
                "tool:pi",
                "tool:pi",
            ]
        );
        assert_eq!(actors(&first), actors(&second));
        // Roles ride in the payload and are unaffected by attribution.
        assert_eq!(back.turns[3].role, Role::Assistant);
        assert_eq!(back.turns[4].role, Role::System);
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
        assert!(view.turns.is_empty());
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
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[0].text, "Fix the bug");
        assert_eq!(view.turns[0].id, "step-002");
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[1].text, "I'll fix that.");
        assert_eq!(
            crate::actor::model_name(&view.turns[1].author),
            Some("claude-opus-4-6")
        );
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
        assert_eq!(view.turns[0].group_id.as_deref(), Some("msg_01abc"));
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
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].tool_uses.len(), 1);
        assert_eq!(view.turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(view.turns[0].tool_uses[0].name, "Read");
        assert_eq!(
            view.turns[0].tool_uses[0].category,
            Some(ToolCategory::FileRead)
        );
        assert!(view.turns[0].tool_uses[0].result.is_some());
        assert!(!view.turns[0].tool_uses[0].result.as_ref().unwrap().is_error);
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
        assert_eq!(view.turns.len(), 1);
        assert_eq!(
            view.turns[0].thinking.as_deref(),
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
        assert!(view.turns[0].parent_id.is_none());
        assert_eq!(view.turns[1].parent_id.as_deref(), Some("step-001"));
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
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].text, "hello");
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
        assert_eq!(view.turns[0].role, Role::User);
        assert_eq!(view.turns[1].role, Role::Assistant);
        assert_eq!(view.turns[2].role, Role::System);
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
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].tool_uses.len(), 2);
        assert_eq!(view.turns[0].tool_uses[0].id, "tu-001");
        assert_eq!(view.turns[0].tool_uses[1].id, "tu-002");
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
        assert!(view.turns.is_empty());
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
        let env = view.turns[0].environment.as_ref().unwrap();
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
        assert!(view.turns[0].environment.is_none());
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
        assert!(view.turns.is_empty());
        assert_eq!(view.events.len(), 2);

        assert_eq!(view.events[0].id, "step-001");
        assert_eq!(view.events[0].event_type, "attachment");
        assert_eq!(
            view.events[0].data["cwd"],
            serde_json::json!("/home/alex/project")
        );
        assert_eq!(view.events[0].data["version"], serde_json::json!("1.0.30"));
        assert!(view.events[0].parent_id.is_none());

        assert_eq!(view.events[1].id, "step-002");
        assert_eq!(view.events[1].event_type, "file-history-snapshot");
        assert_eq!(view.events[1].parent_id.as_deref(), Some("step-001"));
        assert!(view.events[1].data.contains_key("snapshot"));
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
        assert_eq!(view.events.len(), 1);
        assert_eq!(view.events[0].event_type, "unknown");
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
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.events.len(), 1);
        assert_eq!(view.turns[0].text, "hello");
        assert_eq!(view.events[0].event_type, "system");
    }
}
