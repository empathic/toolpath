//! Session → Toolpath `Path` via `toolpath_convo::derive_path`, plus the
//! otel path meta and per-step retention extras.

use crate::branch::{BranchKind, Branches, classify};
use crate::generation::{Cost, Generation};
use crate::harness::{SourceHarness, infer_harness, signals};
use crate::profile;
use crate::provider::{PROVIDER, rfc3339, token_usage, view_from_graph};
use crate::session::Session;
use crate::stitch::{Node, TurnGraph, stitch};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use toolpath::v1::Path;
use toolpath_convo::{DeriveConfig, Role, Turn};

/// The key under which this crate stamps step extras and path meta.
pub const EXTRA_KEY: &str = PROVIDER;

/// Dropped texts by the generation index that stores them.
type Homes = BTreeMap<usize, BTreeMap<String, String>>;

pub fn derive_session(session: &Session, config: &DeriveConfig) -> Path {
    let graph = stitch(session);
    let harness = infer_harness(&signals(session));
    let branches = classify(&graph, harness);
    let mut view = view_from_graph(session, &graph, &branches, harness);
    let placed: BTreeSet<usize> = graph.nodes.iter().filter_map(|n| n.producer).collect();
    view.turns.extend(unplaced_turns(session, &graph, &placed));
    let mut path = toolpath_convo::derive_path(&view, config);
    link_branches(&mut path, &graph, &branches);
    let homes = dropped_homes(&graph);
    stamp_steps(
        &mut path,
        &conversation_key(&view.id),
        session,
        &graph,
        &branches,
        &homes,
    );
    stamp_unplaced(
        &mut path,
        &conversation_key(&view.id),
        session,
        &graph,
        &placed,
        &homes,
    );
    stamp_meta(&mut path, session, harness, &graph, &view.id);
    path
}

/// The step id of unplaced generation `generation_id` whose completion is
/// turn `completion`. `~` never occurs in a turn id (hex).
fn unplaced_step_id(completion: &str, generation_id: &str) -> String {
    format!("{completion}~{generation_id}")
}

fn unplaced(graph: &TurnGraph, placed: &BTreeSet<usize>) -> impl Iterator<Item = usize> {
    (0..graph.links.len()).filter(move |gi| !placed.contains(gi))
}

/// One empty assistant turn per unplaced generation, a sibling of the
/// completion turn it repeated, so its spend is on a step.
fn unplaced_turns(session: &Session, graph: &TurnGraph, placed: &BTreeSet<usize>) -> Vec<Turn> {
    let parents: HashMap<&str, Option<&String>> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.parent.as_ref()))
        .collect();
    unplaced(graph, placed)
        .map(|gi| {
            let g = &session.generations[gi];
            let completion = graph.links[gi].completion.as_str();
            Turn {
                id: unplaced_step_id(completion, &g.id),
                parent_id: parents[completion].cloned(),
                group_id: None,
                role: Role::Assistant,
                timestamp: rfc3339(g.end_ns),
                text: String::new(),
                thinking: g.completion.reasoning.clone().filter(|s| !s.is_empty()),
                tool_uses: Vec::new(),
                model: g.response_model.clone().or_else(|| g.request_model.clone()),
                stop_reason: g.finish_reason.clone(),
                token_usage: token_usage(&g.usage),
                attributed_token_usage: None,
                environment: None,
                delegations: Vec::new(),
                file_mutations: Vec::new(),
            }
        })
        .collect()
}

fn stamp_unplaced(
    path: &mut Path,
    key: &str,
    session: &Session,
    graph: &TurnGraph,
    placed: &BTreeSet<usize>,
    homes: &Homes,
) {
    let mut extras: HashMap<String, Map<String, Value>> = unplaced(graph, placed)
        .map(|gi| {
            let completion = &graph.links[gi].completion;
            let mut m = generation_extra(gi, &session.generations, graph);
            m.insert("completion".into(), json!(completion));
            if let Some(texts) = homes.get(&gi) {
                m.insert("dropped_content".into(), json!(texts));
            }
            m.insert("branch".into(), json!("unplaced"));
            let id = unplaced_step_id(completion, &session.generations[gi].id);
            (id, m)
        })
        .collect();
    for step in &mut path.steps {
        let Some(m) = extras.remove(&step.step.id) else {
            continue;
        };
        if let Some(structural) = step.change.get_mut(key).and_then(|c| c.structural.as_mut()) {
            structural
                .extra
                .insert(EXTRA_KEY.to_string(), Value::Object(m));
        }
    }
}

/// The conversation artifact key convo gives a view: `otel://<derived>`.
pub fn conversation_key(derived_id: &str) -> String {
    format!("{PROVIDER}://{derived_id}")
}

/// Canonical (JCS) serialization: the form step
/// stability is defined on, since `Step.change` and `extra` are HashMaps.
#[cfg(test)]
pub fn canonical_step_json(step: &toolpath::v1::Step) -> String {
    crate::hash::canonical_json(&serde_json::to_value(step).expect("Step serializes"))
}

/// Each dropped text is stored once, on the step of the first generation
/// (placed or unplaced) carrying it.
fn dropped_homes(graph: &TurnGraph) -> Homes {
    let mut homes = Homes::new();
    let mut stored: BTreeSet<&str> = BTreeSet::new();
    for gi in 0..graph.links.len() {
        for d in &graph.links[gi].dropped {
            if stored.insert(d.content_hash.as_str()) {
                let text = graph.dropped_content[&d.content_hash].1.clone();
                homes
                    .entry(gi)
                    .or_default()
                    .insert(d.content_hash.clone(), text);
            }
        }
    }
    homes
}

fn put(m: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        m.insert(key.to_string(), json!(v));
    }
}

/// Head on the main line's last turn, and each finished sub-agent's last
/// turn as an extra parent where its delegating thread resumes.
fn link_branches(path: &mut Path, graph: &TurnGraph, branches: &Branches) {
    let id = |i: usize| graph.nodes[i].id.clone();
    if let Some(h) = branches.head {
        path.path.head = id(h);
    }
    for &(node, extra_parent) in &branches.merges {
        let (node, extra_parent) = (id(node), id(extra_parent));
        if let Some(step) = path.steps.iter_mut().find(|s| s.step.id == node) {
            step.step.parents.push(extra_parent);
        }
    }
}

fn stamp_steps(
    path: &mut Path,
    key: &str,
    session: &Session,
    graph: &TurnGraph,
    branches: &Branches,
    homes: &Homes,
) {
    let nodes: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    for step in &mut path.steps {
        let Some(&ni) = nodes.get(step.step.id.as_str()) else {
            continue;
        };
        let node = &graph.nodes[ni];
        let Some(structural) = step.change.get_mut(key).and_then(|c| c.structural.as_mut()) else {
            continue;
        };
        let mut extra = step_extra(node, &session.generations, graph, homes);
        // Intrinsic: the turn comes from a metadata-only generation.
        let skeleton = extra.contains_key("absent");
        if let Some(name) = branches.branch_name(ni).or(skeleton.then_some("skeleton")) {
            extra.insert("branch".into(), json!(name));
        }
        if let Some(BranchKind::Subagent(call)) = &branches.kind[ni] {
            extra.insert("delegation".into(), json!(call));
        }
        structural
            .extra
            .insert(EXTRA_KEY.to_string(), Value::Object(extra));
    }
}

/// Non-text content parts, verbatim (text is already in the step).
fn non_text_parts(content: &Value) -> Option<Value> {
    let parts: Vec<Value> = content
        .as_array()?
        .iter()
        .filter(|p| {
            p.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t != "text")
        })
        .cloned()
        .collect();
    (!parts.is_empty()).then_some(Value::Array(parts))
}

fn step_extra(
    node: &Node,
    gens: &[Generation],
    graph: &TurnGraph,
    homes: &Homes,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("content_hash".into(), json!(node.content_hash));
    m.insert(
        "first_generation_id".into(),
        json!(gens[node.first_generation].id),
    );
    m.insert("message_role".into(), json!(node.message.role));
    if let Some(parts) = non_text_parts(&node.message.content) {
        m.insert("parts".into(), parts);
    }
    if let Some(gi) = node.producer {
        m.extend(generation_extra(gi, gens, graph));
        if let Some(texts) = homes.get(&gi) {
            m.insert("dropped_content".into(), json!(texts));
        }
    } else if let Some(flags) = absent_flags(&gens[node.first_generation]) {
        m.insert("absent".into(), flags);
    }
    if let Some(echo) = &node.echo {
        m.insert("echo".into(), json!(echo));
    }
    m
}

/// Per-request extras of one generation: on its produced step, or on its
/// unplaced step.
fn generation_extra(gi: usize, gens: &[Generation], graph: &TurnGraph) -> Map<String, Value> {
    let g = &gens[gi];
    let link = &graph.links[gi];
    let mut m = Map::new();
    m.insert("generation_id".into(), json!(g.id));
    m.insert("trace_id".into(), json!(g.trace_id));
    m.insert("prompt_tip".into(), json!(link.prompt_tip));
    put(&mut m, "request_model", g.request_model.as_deref());
    put(&mut m, "provider", g.provider.as_deref());
    m.insert("usage".into(), json!(g.usage));
    if g.cost != Cost::default() {
        m.insert("cost".into(), json!(g.cost));
    }
    m.insert("dropped".into(), json!(link.dropped));
    if !g.source_meta.is_empty() {
        m.insert(g.profile.clone(), Value::Object(g.source_meta.clone()));
    }
    if let Some(flags) = absent_flags(g) {
        m.insert("absent".into(), flags);
    }
    put(&mut m, "continues", g.continues.as_deref());
    if g.compacted {
        m.insert("compacted".into(), json!(true));
    }
    if !g.completion.reasoning_details.is_empty() {
        m.insert(
            "reasoning_details".into(),
            json!(g.completion.reasoning_details),
        );
    }
    m
}

/// `{prompt?: true, completion?: true}` for a skeleton generation (only the
/// true flags), else `None`.
fn absent_flags(g: &Generation) -> Option<Value> {
    if !g.absent.any() {
        return None;
    }
    let mut flags = Map::new();
    if g.absent.prompt {
        flags.insert("prompt".into(), json!(true));
    }
    if g.absent.completion {
        flags.insert("completion".into(), json!(true));
    }
    Some(Value::Object(flags))
}

fn stamp_meta(
    path: &mut Path,
    session: &Session,
    harness: SourceHarness,
    graph: &TurnGraph,
    derived_id: &str,
) {
    let gens = &session.generations;
    let mut m = Map::new();
    let profiles: BTreeSet<&str> = gens.iter().map(|g| g.profile.as_str()).collect();
    stamp_profiles(&mut m, &profiles);
    m.insert("harness".into(), json!(harness.as_str()));
    stamp_hint_and_missing_continuations(&mut m, gens, graph);
    put(&mut m, "session_id", session.session_id.as_deref());
    m.insert("derived_session_id".into(), json!(derived_id));
    m.insert("session_key".into(), json!(session.key));
    put(
        &mut m,
        "request_session_id",
        gens.iter().find_map(|g| g.request_session_id.as_deref()),
    );
    put(
        &mut m,
        "user_id",
        gens.iter().find_map(|g| g.user_id.as_deref()),
    );
    put(
        &mut m,
        "client_key",
        gens.iter().find_map(|g| g.client_key.as_deref()),
    );
    m.insert(
        "generation_ids".into(),
        json!(gens.iter().map(|g| &g.id).collect::<Vec<_>>()),
    );
    m.insert(
        "trace_ids".into(),
        json!(gens.iter().map(|g| &g.trace_id).collect::<Vec<_>>()),
    );
    if let Some(cost) = cost_usd(gens) {
        m.insert("cost_usd".into(), cost);
    }
    let providers: BTreeSet<&str> = gens.iter().filter_map(|g| g.provider.as_deref()).collect();
    m.insert("providers".into(), json!(providers));
    m.insert("truncated".into(), json!(session.truncated));
    for name in &profiles {
        let Some(p) = profile::by_name(name) else {
            continue;
        };
        let mut pm = Map::new();
        for k in p.session_meta_keys() {
            let first = gens
                .iter()
                .filter(|g| g.profile == *name)
                .find_map(|g| g.source_meta.get(*k));
            if let Some(v) = first {
                pm.insert((*k).to_string(), v.clone());
            }
        }
        if !pm.is_empty() {
            m.insert((*name).to_string(), Value::Object(pm));
        }
    }
    let meta = path.meta.get_or_insert_with(Default::default);
    meta.source = Some(PROVIDER.to_string());
    meta.extra.insert(EXTRA_KEY.to_string(), Value::Object(m));
}

/// `{total, by_model, priced_generations, generations}` when any generation
/// is priced. A total (or a model's subtotal) is `null` when any
/// generation it covers is unpriced: an unknown price is never $0.
fn cost_usd(gens: &[Generation]) -> Option<Value> {
    let priced = gens.iter().filter(|g| g.cost.total.is_some()).count();
    if priced == 0 {
        return None;
    }
    let mut by_model: BTreeMap<&str, Option<f64>> = BTreeMap::new();
    let mut total = Some(0.0);
    // Float sums depend on order: sum in start order whatever the feed order.
    let mut by_start: Vec<&Generation> = gens.iter().collect();
    by_start.sort_by(|a, b| (a.start_ns, &a.id).cmp(&(b.start_ns, &b.id)));
    let add = |acc: Option<f64>, c: Option<f64>| Some(acc? + c?);
    for g in by_start {
        let model = g
            .response_model
            .as_deref()
            .or(g.request_model.as_deref())
            .unwrap_or("unknown");
        total = add(total, g.cost.total);
        let sub = by_model.entry(model).or_insert(Some(0.0));
        *sub = add(*sub, g.cost.total);
    }
    Some(json!({
        "total": total,
        "by_model": by_model,
        "priced_generations": priced,
        "generations": gens.len(),
    }))
}

/// `meta.otel.profile`: the shared profile name, else `"mixed"` plus the
/// sorted `profiles`. No key for a session without generations.
fn stamp_profiles(otel: &mut Map<String, Value>, profiles: &BTreeSet<&str>) {
    match profiles.len() {
        0 => {}
        1 => {
            otel.insert("profile".into(), json!(profiles.first()));
        }
        _ => {
            otel.insert("profile".into(), json!("mixed"));
            otel.insert("profiles".into(), json!(profiles));
        }
    }
}

/// `meta.otel.harness_hint` (the first hint that does not map to a known
/// harness) and `meta.otel.missing_continuations`.
fn stamp_hint_and_missing_continuations(
    otel: &mut Map<String, Value>,
    gens: &[Generation],
    graph: &TurnGraph,
) {
    if let Some(h) = gens
        .iter()
        .filter_map(|g| g.harness_hint.as_deref())
        .find(|h| SourceHarness::from_hint(h).is_none())
    {
        otel.insert("harness_hint".into(), json!(h));
    }
    if !graph.missing_continuations.is_empty() {
        otel.insert(
            "missing_continuations".into(),
            json!(graph.missing_continuations),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn priced(id: &str, start: u64, model: &str, total: Option<f64>) -> Generation {
        Generation {
            id: id.into(),
            start_ns: start,
            response_model: Some(model.into()),
            cost: Cost {
                total,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn all_priced_sums_per_model_and_overall() {
        let gens = [
            priced("a", 1, "m1", Some(0.5)),
            priced("b", 2, "m2", Some(0.25)),
            priced("c", 3, "m1", Some(1.0)),
        ];
        assert_eq!(
            cost_usd(&gens).unwrap(),
            json!({"total": 1.75, "by_model": {"m1": 1.5, "m2": 0.25},
                   "priced_generations": 3, "generations": 3})
        );
    }

    #[test]
    fn an_unpriced_generation_makes_its_totals_unknown_not_zero() {
        let gens = [
            priced("a", 1, "m1", Some(0.5)),
            priced("b", 2, "m2", None),
            priced("c", 3, "m1", Some(1.0)),
        ];
        assert_eq!(
            cost_usd(&gens).unwrap(),
            json!({"total": null, "by_model": {"m1": 1.5, "m2": null},
                   "priced_generations": 2, "generations": 3})
        );
    }

    #[test]
    fn no_priced_generation_has_no_cost_block() {
        assert_eq!(cost_usd(&[priced("a", 1, "m1", None)]), None);
        assert_eq!(cost_usd(&[]), None);
    }
}
