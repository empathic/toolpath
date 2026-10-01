//! Turn DAG → provider-agnostic `ConversationView`.

use crate::branch::Branches;
use crate::generation::{Generation, Usage};
use crate::harness::SourceHarness;
use crate::harness::cwd::find_cwd;
use crate::harness::shell_writes::{ShellState, Stamps, TurnWrites, turn_writes};
use crate::harness::tools::tool_category;
use crate::hash::derived_session_id;
use crate::normalize::{content_text, is_system_like};
use crate::session::Session;
use crate::stitch::{Node, TurnGraph};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use toolpath_convo::shell_writes::{ScriptDir, normalize_path};
use toolpath_convo::{
    ConversationView, DelegatedWork, ProducerInfo, Role, SessionBase, TokenUsage, ToolInvocation,
    ToolResult, Turn,
};

/// `view.provider_id`, `meta.source`, the extras key, and `producer.name`
/// when the harness is unknown.
pub const PROVIDER: &str = "otel";

#[cfg(test)]
pub fn session_to_view(session: &Session) -> ConversationView {
    let graph = crate::stitch::stitch(session);
    let harness = crate::stitch::settled_harness(session, true);
    view_and_stamps(
        session,
        &graph,
        &crate::branch::classify(&graph, harness),
        harness,
    )
    .0
}

/// `extra.otel` from shell calls, by turn id.
#[derive(Debug, Default)]
pub(crate) struct ShellStamps {
    /// For shell-derived file changes, by change key.
    pub files: BTreeMap<String, Stamps>,
    /// `unresolved_shell_writes` of the turn's own step.
    pub unresolved: BTreeMap<String, Vec<Value>>,
}

/// The view, and the stamps of its shell calls. Each turn starts from its
/// parent's [`ShellState`] and working directory (the parent's, else the
/// first one its own first generation's prompt names). A persistent shell
/// belongs to the process, not the branch, so a turn's shell directory is
/// also unknown when a generation sorting before its first one, off the
/// turn's ancestry, issued a call that may have left the shell anywhere
/// but there, and its start directory moved when such a call left it
/// moved. Nothing depends on a later generation.
pub(crate) fn view_and_stamps(
    session: &Session,
    graph: &TurnGraph,
    branches: &Branches,
    harness: SourceHarness,
) -> (ConversationView, ShellStamps) {
    let gens = &session.generations;
    let base = find_cwd(gens);
    let mut prompt_cwd: HashMap<usize, Option<Rc<str>>> = HashMap::new();
    // A turn's cwd, shell state, and the indices of its ancestry's
    // `shell_dirs` records.
    type Carried = (Option<Rc<str>>, Rc<ShellState>, Rc<Vec<usize>>);
    let mut carried: HashMap<&str, Carried> = HashMap::new();
    // (issuing generation, directory a persistent shell was left in, `None`
    // when unknown, whether the start directory was left moved) for each
    // turn whose calls touched it.
    let mut shell_dirs: Vec<(usize, Option<String>, bool)> = Vec::new();
    let mut stamps = ShellStamps::default();
    let mut turns: Vec<Turn> = Vec::with_capacity(graph.nodes.len());
    for (ni, n) in graph.nodes.iter().enumerate() {
        let (inherited, mut state, mut ancestry) = n
            .parent
            .as_deref()
            .and_then(|p| carried.get(p))
            .cloned()
            .unwrap_or_default();
        let cwd = inherited.or_else(|| {
            prompt_cwd
                .entry(n.first_generation)
                .or_insert_with(|| {
                    find_cwd(std::slice::from_ref(&gens[n.first_generation])).map(Rc::from)
                })
                .clone()
        });
        let here = state.shell_dir(cwd.as_deref()).map(normalize_path);
        let off_ancestry = shell_dirs
            .iter()
            .enumerate()
            .filter(|(i, (g, _, _))| *g < n.first_generation && !ancestry.contains(i))
            .map(|(_, r)| r);
        let (mut elsewhere, mut start_moved) = (false, false);
        for (_, dir, moved) in off_ancestry {
            elsewhere |= dir.is_none() || *dir != here;
            start_moved |= *moved;
        }
        if elsewhere && state.dir != ScriptDir::Unknown {
            Rc::make_mut(&mut state).dir = ScriptDir::Unknown;
        }
        if start_moved && !state.start_moved {
            Rc::make_mut(&mut state).start_moved = true;
        }
        let (turn, writes) = to_turn(
            n,
            gens,
            harness,
            delegations(n, ni, branches),
            cwd.as_deref(),
            &mut state,
        );
        if writes.shell_dir_touched {
            let left = state.shell_dir(cwd.as_deref()).map(normalize_path);
            Rc::make_mut(&mut ancestry).push(shell_dirs.len());
            shell_dirs.push((
                n.producer.unwrap_or(n.first_generation),
                left,
                state.start_moved,
            ));
        }
        if !writes.stamps.is_empty() {
            stamps.files.insert(n.id.clone(), writes.stamps);
        }
        if !writes.unresolved.is_empty() {
            stamps.unresolved.insert(n.id.clone(), writes.unresolved);
        }
        carried.insert(n.id.as_str(), (cwd, state, ancestry));
        turns.push(turn);
    }
    let mut files_changed: Vec<String> = Vec::new();
    for m in turns.iter().flat_map(|t| &t.file_mutations) {
        if !files_changed.contains(&m.path) {
            files_changed.push(m.path.clone());
        }
    }
    let view = ConversationView {
        id: derived_session_id(&session.key),
        started_at: gens.first().map(|g| datetime(g.start_ns)),
        last_activity: gens.iter().map(|g| g.end_ns).max().map(datetime),
        turns,
        total_usage: total_usage(gens),
        provider_id: Some(PROVIDER.to_string()),
        files_changed,
        session_ids: session.session_id.iter().cloned().collect(),
        base: base.map(|wd| SessionBase {
            working_dir: Some(wd),
            ..Default::default()
        }),
        producer: Some(ProducerInfo {
            name: producer_name(harness).to_string(),
            version: None,
        }),
        ..Default::default()
    };
    (view, stamps)
}

/// The harness's name as its own deriver writes `producer.name`, else
/// [`PROVIDER`].
pub fn producer_name(harness: SourceHarness) -> &'static str {
    match harness {
        SourceHarness::Unknown => PROVIDER,
        known => known.as_str(),
    }
}

/// Delegation calls of node `ni` that started a sub-agent thread; its
/// turns stay steps of this path (see [`crate::branch`]).
fn delegations(n: &Node, ni: usize, branches: &Branches) -> Vec<DelegatedWork> {
    branches
        .delegations
        .get(&ni)
        .into_iter()
        .flatten()
        .map(|d| DelegatedWork {
            agent_id: d.call_id.clone(),
            prompt: d.prompt.clone(),
            turns: Vec::new(),
            result: n.results.get(&d.call_id).map(|r| r.content.clone()),
        })
        .collect()
}

fn to_turn(
    n: &Node,
    gens: &[Generation],
    harness: SourceHarness,
    delegations: Vec<DelegatedWork>,
    cwd: Option<&str>,
    state: &mut Rc<ShellState>,
) -> (Turn, TurnWrites) {
    let produced = n.producer.map(|i| &gens[i]);
    let ts = match produced {
        Some(g) => g.end_ns,
        None => gens[n.first_generation].start_ns,
    };
    let tool_uses: Vec<ToolInvocation> = n
        .message
        .tool_calls
        .iter()
        .map(|c| ToolInvocation {
            id: c.id.clone(),
            name: c.function.name.clone(),
            input: c.function.parsed_arguments(),
            result: n.results.get(&c.id).map(|r| ToolResult {
                content: r.content.clone(),
                is_error: r.is_error,
            }),
            category: tool_category(harness, &c.function.name),
        })
        .collect();
    // Copy-on-write: a turn without calls shares its parent's state.
    let mut writes = if tool_uses.is_empty() {
        TurnWrites::default()
    } else {
        turn_writes(&tool_uses, cwd, Rc::make_mut(state))
    };
    let file_mutations = std::mem::take(&mut writes.mutations);
    let turn = Turn {
        id: n.id.clone(),
        parent_id: n.parent.clone(),
        group_id: None,
        role: role_of(&n.message.role),
        timestamp: rfc3339(ts),
        text: content_text(&n.message.content),
        thinking: produced
            .and_then(|g| g.completion.reasoning.clone())
            .filter(|s| !s.is_empty()),
        tool_uses,
        model: produced.and_then(|g| g.response_model.clone().or_else(|| g.request_model.clone())),
        stop_reason: produced.and_then(|g| g.finish_reason.clone()),
        token_usage: produced.and_then(|g| token_usage(&g.usage)),
        attributed_token_usage: None,
        environment: None,
        delegations,
        file_mutations,
    };
    (turn, writes)
}

fn role_of(role: &str) -> Role {
    match role {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        r if is_system_like(r) => Role::System,
        other => Role::Other(other.to_string()),
    }
}

fn datetime(ns: u64) -> DateTime<Utc> {
    DateTime::from_timestamp((ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as u32)
        .unwrap_or_default()
}

pub fn rfc3339(ns: u64) -> String {
    datetime(ns).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn clamp(v: Option<u64>) -> Option<u32> {
    v.map(|x| u32::try_from(x).unwrap_or(u32::MAX))
}

/// The generation's additive usage, reasoning (clamped to ≤ output) as a
/// breakdown; `None` when every class is zero or absent.
pub fn token_usage(u: &Usage) -> Option<TokenUsage> {
    let classes = [
        u.input_tokens,
        u.output_tokens,
        u.cached_input_tokens,
        u.cache_write_tokens,
    ];
    if classes.iter().all(|c| c.unwrap_or(0) == 0) {
        return None;
    }
    let reasoning = match (u.reasoning_tokens, u.output_tokens) {
        (Some(r), Some(o)) => Some(r.min(o)),
        (r, _) => r,
    };
    let mut breakdowns = BTreeMap::new();
    if let Some(r) = clamp(reasoning).filter(|r| *r > 0) {
        breakdowns.insert(
            "output".to_string(),
            BTreeMap::from([("reasoning".to_string(), r)]),
        );
    }
    Some(TokenUsage {
        input_tokens: clamp(u.input_tokens),
        output_tokens: clamp(u.output_tokens),
        cache_read_tokens: clamp(u.cached_input_tokens),
        cache_write_tokens: clamp(u.cache_write_tokens),
        breakdowns,
    })
}

fn total_usage(gens: &[Generation]) -> Option<TokenUsage> {
    let mut sum = Usage::default();
    let add = |acc: &mut Option<u64>, v: Option<u64>| {
        if let Some(v) = v {
            *acc = Some(acc.unwrap_or(0).saturating_add(v));
        }
    };
    for g in gens {
        add(&mut sum.input_tokens, g.usage.input_tokens);
        add(&mut sum.output_tokens, g.usage.output_tokens);
        add(&mut sum.cached_input_tokens, g.usage.cached_input_tokens);
        add(&mut sum.cache_write_tokens, g.usage.cache_write_tokens);
        add(&mut sum.reasoning_tokens, g.usage.reasoning_tokens);
    }
    token_usage(&sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{Completion, FunctionCall, Message, ToolCall};
    use serde_json::{Value, json};
    use toolpath_convo::ToolCategory;

    fn usage(input: u64, output: u64, reasoning: Option<u64>) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            reasoning_tokens: reasoning,
            ..Default::default()
        }
    }

    #[test]
    fn timestamps_are_rfc3339_millis_utc() {
        assert_eq!(
            rfc3339(1_790_605_442_792_000_000),
            "2026-09-28T14:24:02.792Z"
        );
    }

    #[test]
    fn zero_usage_is_none_and_reasoning_is_a_clamped_breakdown() {
        assert!(token_usage(&usage(0, 0, None)).is_none());
        let fully_cached = Usage {
            input_tokens: Some(0),
            cached_input_tokens: Some(9),
            ..Default::default()
        };
        assert_eq!(
            token_usage(&fully_cached).unwrap().cache_read_tokens,
            Some(9)
        );
        assert_eq!(
            token_usage(&usage(10, 5, Some(3))).unwrap().breakdowns["output"]["reasoning"],
            3
        );
        assert_eq!(
            token_usage(&usage(10, 5, Some(9))).unwrap().breakdowns["output"]["reasoning"],
            5
        );
        assert!(
            token_usage(&usage(10, 5, Some(0)))
                .unwrap()
                .breakdowns
                .is_empty()
        );
    }

    #[test]
    fn total_usage_saturates_on_huge_counts() {
        let big = |n: u64| Generation {
            usage: Usage {
                input_tokens: Some(n),
                output_tokens: Some(n),
                cached_input_tokens: Some(n),
                cache_write_tokens: Some(n),
                reasoning_tokens: Some(n),
                ..Default::default()
            },
            ..Default::default()
        };
        let gens = [big(u64::MAX - 1), big(i64::MAX as u64), big(u64::MAX)];
        let total = total_usage(&gens).unwrap();
        assert_eq!(total.input_tokens, Some(u32::MAX));
        assert_eq!(total.output_tokens, Some(u32::MAX));
        assert_eq!(total.cache_read_tokens, Some(u32::MAX));
        assert_eq!(total.cache_write_tokens, Some(u32::MAX));
        assert_eq!(total.breakdowns["output"]["reasoning"], u32::MAX);
    }

    #[test]
    fn unparseable_arguments_stay_raw_and_a_trailing_call_has_no_result() {
        let raw = "{\"command\": \"ls";
        let g = Generation {
            id: "g1".into(),
            start_ns: 1_000_000_000,
            end_ns: 2_000_000_000,
            usage: usage(7, 3, None),
            messages: vec![
                Message {
                    role: "system".into(),
                    content: json!("Primary working directory: /w"),
                    ..Default::default()
                },
                Message {
                    role: "user".into(),
                    content: json!("hi"),
                    ..Default::default()
                },
            ]
            .into(),
            completion: Completion {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    function: FunctionCall {
                        name: "Bash".into(),
                        arguments: Value::String(raw.into()),
                    },
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = Session::new("otel-cluster:0000000000000000".into(), None, vec![g]);
        let view = session_to_view(&s);
        let last = view.turns.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        let u = &last.tool_uses[0];
        assert_eq!(u.input, Value::String(raw.into()));
        assert!(u.result.is_none());
        assert_eq!(u.category, Some(ToolCategory::Shell));
        assert_eq!(view.base.and_then(|b| b.working_dir).as_deref(), Some("/w"));
        assert_eq!(view.id, derived_session_id(&s.key));
        assert_eq!(view.provider_id.as_deref(), Some("otel"));
    }

    #[test]
    fn heredoc_writes_join_files_changed() {
        let cmd = "cat <<'EOF' > wc.py\nprint(1)\nEOF";
        let g = Generation {
            id: "g1".into(),
            start_ns: 1,
            end_ns: 2,
            messages: vec![Message {
                role: "user".into(),
                content: json!("<environment_context><cwd>/w</cwd></environment_context>"),
                ..Default::default()
            }]
            .into(),
            completion: Completion {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    function: FunctionCall {
                        name: "exec_command".into(),
                        arguments: Value::String(json!({"cmd": cmd, "workdir": "/w"}).to_string()),
                    },
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = Session::new("otel-cluster:0000000000000000".into(), None, vec![g]);
        let graph = crate::stitch::stitch(&s);
        let harness = crate::stitch::settled_harness(&s, true);
        let branches = crate::branch::classify(&graph, harness);
        let (view, stamps) = view_and_stamps(&s, &graph, &branches, harness);
        assert_eq!(view.files_changed, vec!["wc.py".to_string()]);
        let turn = view.turns.last().unwrap();
        assert_eq!(turn.file_mutations[0].tool_id.as_deref(), Some("c1"));
        assert_eq!(stamps.files[&turn.id]["wc.py"]["outcome"], "unknown");
    }
}
