//! Stateless incremental JSONL sends for a session that is still growing.

use crate::branch::{Branches, classify};
use crate::derive::{categories, derive_steps, derive_stitched, unplaced_step_ids};
use crate::error::{OtelError, Result};
use crate::generation::Generation;
use crate::harness::SourceHarness;
use crate::record::{GenerationRecord, MessageHash, StoredMessage};
use crate::session::{Session, derived_key};
use crate::stitch::{TurnGraph, emitted_turns, stitch, stitch_with_prefix};
use crate::stitch::{first_settle, held_harness};
use crate::{DeriveConfig, Derived, ToolClassifier, session_from_records};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use toolpath::v1::Path;
use toolpath::v1::jsonl::{BatchLimits, Body, DeltaError, HeadRule, delta_bodies};

/// What the store already holds of the session, all of it readable from
/// the store itself.
#[derive(Debug, Clone, Default)]
pub struct Remote {
    /// The target path exists. The first body then starts with a `PathMeta`
    /// patch instead of a `PathOpen`.
    pub opened: bool,
    /// The target path's `meta.otel.generation_ids`: the feed order of the
    /// generations derived so far; on a continuation's first send, the
    /// frozen path's. Empty when nothing is held.
    pub fed: Vec<String>,
    /// Ids of steps the target path has. Any subset is correct: a step left
    /// out is sent again, identically, and the store skips it. An empty set
    /// (resend everything settled) is correct until the store can list its
    /// step ids. An id the path does not have is not correct: its children
    /// would be sent without it.
    pub stored: HashSet<String>,
    /// The target path's `meta.otel.harness`: the harness the session's
    /// first settled send decided. Every later send derives with it, so a
    /// later generation never changes a sent step's tool categories or
    /// marks. `None` (nothing stored yet) decides it from the feed order
    /// ([`derive_path`](crate::derive_path)'s rule, applied to the feed).
    pub harness: Option<String>,
    /// Every step id of the frozen path the target path continues; empty
    /// for a path that continues none. These steps live in the frozen path:
    /// they are never sent and never named by `Head`, only as parents. Pass
    /// the same set on every send of the continuation.
    pub base: HashSet<String>,
}

/// How much of the session a [`derive_jsonl`] call sends.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Settle {
    /// Only turns no later generation can change: the session may still
    /// grow.
    #[default]
    Settled,
    /// Every turn: the session is over.
    Final,
}

/// The request bodies that bring the store `remote` describes up to the
/// session's settled turns, deriving the session in feed order so that no
/// step the store holds ever changes. `docs/agents/formats/otel.md`
/// ("Incremental JSONL") gives the rationale; `docs/RFC-jsonl.md` the delta
/// rules.
///
/// `records` are all the session's [`GenerationRecord`]s so far, from
/// [`read_generations`](crate::read_generations) of its request bodies in
/// any grouping (one call per delivery is the intended use), in the order
/// the store received them: of several records with one generation id the
/// first is derived and the others count as duplicates, so a copy that
/// arrives later (another profile's, say) never replaces one already sent.
/// The one-shot derives keep the better-ranked profile's copy instead, so
/// a stream reads back to them only when that copy arrives first; with one
/// profile consulted ([`ProfileSelection::OpenRouter`](crate::ProfileSelection::OpenRouter),
/// say) the case cannot arise. `messages` looks up the prompt messages the
/// records name and must return the message stored under exactly the hash
/// asked for ([`StoredMessage::hash`]); it is not checked, and a wrong
/// message derives wrong content under the right ids.
///
/// Each returned [`Body`] is one append (`text`, NDJSON), split by
/// `toolpath`'s batcher within `limits` (`docs/RFC-jsonl.md`, "Batching").
/// The first starts with a `PathOpen` (`remote.opened` false) or a
/// `PathMeta` patch carrying the new feed order as
/// `meta.otel.generation_ids`, so the feed order commits with the first
/// steps. Every body ends with a `Head` naming a step of the target path
/// stored by then; the last body's is the real head. No bodies means
/// nothing new has settled. Sending the same bodies again is harmless.
///
/// To continue a frozen path, pass its step ids as `remote.base` on every
/// send of the new path: on the first, with the frozen path's `fed` and
/// `opened` false; later, with the new path's `fed`, `stored` and `opened`.
/// The bodies open the new path once, hold only new steps, never a `base`
/// id, and name `base` steps only as the parents the new steps continue
/// from, never as `Head`.
///
/// The skip counts are only the duplicate generation ids among `records`;
/// [`read_generations`](crate::read_generations) counted the rest.
///
/// # Feed order
///
/// The session is derived with the `remote.fed` generations first, in that
/// order, then the rest by `(start_ns, id)`. A generation delivered after
/// later-starting ones therefore only appends: its new turns are new
/// steps, and when it produced a turn the store already holds, its record
/// is a new unplaced dead-end step. With in-order delivery feed
/// order is start order and the sends read back to `derive_path`. A session
/// without a client session id is keyed by the first fed generation. A
/// sub-agent request fed before the request that made its delegation call
/// cannot be matched to that call, so its thread is an ordinary root (and
/// the main line, should it produce two turns first); this holds for the
/// rest of the stream but differs from `derive_path`.
///
/// With [`Settle::Settled`] only settled turns are sent; [`Settle::Final`]
/// settles every turn and is passed once the session is over. `Head` is the
/// step, among those settled, that `derive_path` (start order) places last
/// in view order, so a late sub-agent never takes it from the main line.
///
/// # Errors
///
/// As [`derive_path_from_records`](crate::derive_path_from_records), and:
///
/// - [`OtelError::FedGenerationMissing`] when `remote.fed` names a
///   generation `records` lack.
/// - [`OtelError::UnknownHarness`] when `remote.harness` names no harness
///   this crate records.
/// - [`OtelError::Delta`] with [`DeltaError::Amended`]: a sent step would
///   change. Only a step a [`Settle::Final`] call sent unsettled can, when
///   a later generation settles it differently. That is a mutation, never
///   an append: record it through the store's mutation log, never by
///   sending or copying the step again. A step `remote.stored` omits cannot
///   be checked; its changed version goes out and the store refuses the
///   body (Pathbase: `400 invalid_document`) with nothing written.
pub fn derive_jsonl<'m>(
    records: &[GenerationRecord],
    messages: impl Fn(&MessageHash) -> Option<&'m StoredMessage>,
    config: &DeriveConfig,
    remote: &Remote,
    settle: Settle,
    limits: BatchLimits,
) -> Result<Derived<Vec<Body>>> {
    let (mut session, skipped) =
        session_from_records(records, messages, config, crate::record::Pick::Arrival)?;
    if session.session_id.is_none()
        && let Some(first) = remote.fed.first()
        && let Some(g) = session.generations.iter().find(|g| &g.id == first)
    {
        session.key = derived_key(g);
    }
    let final_ = settle == Settle::Final;
    let classifier = config.tool_category.as_ref();
    let output = send(&session, &config.convo, classifier, remote, final_, limits)?;
    Ok(Derived { output, skipped })
}

fn send(
    session: &Session,
    config: &toolpath_convo::DeriveConfig,
    classifier: Option<&ToolClassifier>,
    remote: &Remote,
    final_: bool,
    limits: BatchLimits,
) -> Result<Vec<Body>> {
    let (feed, held) = in_feed_order(session, &remote.fed)?;
    // Frozen steps are held too, only by another path: checked like any
    // stored step, never sent, never a head.
    let claimed: HashSet<String> = remote.stored.union(&remote.base).cloned().collect();
    let (graph, held_graph) = stitch_with_prefix(&feed, (!claimed.is_empty()).then_some(held));
    // One harness for every derivation of this call: the stored one, else
    // the one the feed order decides. Without a stored one, the fed
    // generations are compared under the harness they decided, so a
    // change is reported as `Amended`.
    let (harness, held_with) = match &remote.harness {
        Some(name) => {
            let h = SourceHarness::from_name(name)
                .ok_or_else(|| OtelError::UnknownHarness(name.clone()))?;
            (h, h)
        }
        None => {
            let first = first_settle(&feed, final_, classifier);
            (first.1, held_harness(&feed, held, first, classifier))
        }
    };
    let (mut path, branches) = derive_stitched(&feed, &graph, config, harness, classifier);
    let stored = match &held_graph {
        Some(held_graph) => held_stored(
            &path, &feed, held_graph, held_with, classifier, config, &claimed,
        )?,
        None => HashSet::new(),
    };
    let rank = match &feed {
        Cow::Borrowed(_) => view_rank(&graph, &branches),
        Cow::Owned(_) => {
            let graph = stitch(session);
            view_rank(&graph, &classify(&graph, &categories(classifier, harness)))
        }
    };
    if !final_ {
        let mut emitted = emitted_turns(&graph, &branches, false);
        // An unplaced step never changes; it goes out under its parent.
        emitted.extend(unplaced_step_ids(&feed, &graph));
        path.steps.retain(|s| emitted.contains(&s.step.id));
        keep_parented(&mut path);
    }
    // Never sent: what the store holds, here or in the frozen path.
    let skip: HashSet<String> = stored.union(&remote.base).cloned().collect();
    // What the target path holds, the only steps a `Head` may name besides
    // those it sends: nothing before it is opened, and never a frozen step.
    let here: HashSet<String> = if remote.opened {
        stored.difference(&remote.base).cloned().collect()
    } else {
        HashSet::new()
    };
    let Some(head) = latest(&path, &rank, |id| here.contains(id) || !skip.contains(id)) else {
        return Ok(Vec::new());
    };
    path.path.head = head;
    // A body's provisional head: the latest step the target path holds
    // once the body lands.
    let provisional = |ctx: &toolpath::v1::jsonl::HeadContext<'_>| {
        latest(&path, &rank, |id| {
            ctx.held.contains(id) && (here.contains(id) || !skip.contains(id))
        })
        .expect("a body holds a step")
    };
    Ok(delta_bodies(
        &path,
        &skip,
        remote.opened,
        limits,
        &HeadRule::Custom(&provisional),
    )?)
}

/// Drops, to a fixpoint, every step with a parent (extra parents
/// included) that is not a kept step.
fn keep_parented(path: &mut Path) {
    loop {
        let ids: HashSet<String> = path.steps.iter().map(|s| s.step.id.clone()).collect();
        let before = path.steps.len();
        path.steps
            .retain(|s| s.step.parents.iter().all(|p| ids.contains(p)));
        if path.steps.len() == before {
            return;
        }
    }
}

/// The ids in `claimed` that `held` (the stitch of the fed generations)
/// derives: a claimed id it does not derive was not stored under this
/// feed order, so it is not trusted and the step goes out again.
/// `Amended` when one of the trusted steps changes in `path`.
fn held_stored(
    path: &Path,
    feed: &Session,
    held: &TurnGraph,
    harness: SourceHarness,
    classifier: Option<&ToolClassifier>,
    config: &toolpath_convo::DeriveConfig,
    claimed: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut held_ids: HashSet<String> = held.nodes.iter().map(|n| n.id.clone()).collect();
    held_ids.extend(unplaced_step_ids(feed, held));
    let stored: HashSet<String> = claimed
        .iter()
        .filter(|id| held_ids.contains(id.as_str()))
        .cloned()
        .collect();
    // Only a step a `final_` call sent unsettled can change; compare every
    // trusted one so that no change goes unreported.
    if !stored.is_empty() {
        let (held_path, ..) = derive_steps(feed, held, config, harness, classifier);
        let amended = amended(path, &held_path, &stored);
        if !amended.is_empty() {
            return Err(DeltaError::Amended { steps: amended }.into());
        }
    }
    Ok(stored)
}

/// Turn id → whether `derive_path` places it on the main line, and its
/// position in view order: the head is the greatest main-line turn.
fn view_rank(graph: &TurnGraph, branches: &Branches) -> HashMap<String, (bool, usize)> {
    graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), (branches.kind[i].is_none(), i)))
        .collect()
}

/// The id of the step among `path`'s that `keep` admits latest in
/// start-order `rank` (main line first), then in feed-order view order.
fn latest(
    path: &Path,
    rank: &HashMap<String, (bool, usize)>,
    keep: impl Fn(&str) -> bool,
) -> Option<String> {
    path.steps
        .iter()
        .enumerate()
        .filter(|(_, s)| keep(&s.step.id))
        .max_by_key(|(i, s)| (rank.get(&s.step.id), *i))
        .map(|(_, s)| s.step.id.clone())
}

/// The session in feed order, and how many of its generations `fed` names.
fn in_feed_order<'a>(session: &'a Session, fed: &[String]) -> Result<(Cow<'a, Session>, usize)> {
    let mut rank: HashMap<&str, usize> = HashMap::new();
    for id in fed {
        let next = rank.len();
        rank.entry(id.as_str()).or_insert(next);
    }
    let present: HashSet<&str> = session.generations.iter().map(|g| g.id.as_str()).collect();
    if let Some(missing) = fed.iter().find(|id| !present.contains(id.as_str())) {
        return Err(OtelError::FedGenerationMissing(missing.clone()));
    }
    let order =
        |a: &Generation, b: &Generation| match (rank.get(a.id.as_str()), rank.get(b.id.as_str())) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => (a.start_ns, &a.id).cmp(&(b.start_ns, &b.id)),
        };
    let held = session
        .generations
        .iter()
        .filter(|g| rank.contains_key(g.id.as_str()))
        .count();
    if session.generations.is_sorted_by(|a, b| order(a, b).is_le()) {
        return Ok((Cow::Borrowed(session), held));
    }
    let mut generations = session.generations.clone();
    generations.sort_by(order);
    Ok((
        Cow::Owned(Session {
            key: session.key.clone(),
            session_id: session.session_id.clone(),
            generations,
            truncated: session.truncated,
            prompt_hashes: session.prompt_hashes.clone(),
        }),
        held,
    ))
}

/// Stored ids whose step in `path` differs from the one in `held`, the
/// derivation of the generations already fed. Ids absent from either are
/// not compared.
fn amended(path: &Path, held: &Path, stored: &HashSet<String>) -> Vec<String> {
    // Key-sorted values, since `Step.change` and `extra` are HashMaps. Both
    // sides come from the same derivation code, so equal steps spell their
    // numbers alike and value equality is canonical equality, without
    // serializing every stored step.
    let value = |s: &toolpath::v1::Step| serde_json::to_value(s).expect("Step serializes");
    let before: HashMap<&str, &toolpath::v1::Step> = held
        .steps
        .iter()
        .filter(|s| stored.contains(&s.step.id))
        .map(|s| (s.step.id.as_str(), s))
        .collect();
    path.steps
        .iter()
        .filter(|s| {
            before
                .get(s.step.id.as_str())
                .is_some_and(|b| value(b) != value(s))
        })
        .map(|s| s.step.id.clone())
        .collect()
}

/// `meta.otel.generation_ids` of a path's meta: the `fed` a send recorded.
#[cfg(test)]
pub(crate) fn fed_of(path: &Path) -> Vec<String> {
    path.meta
        .as_ref()
        .and_then(|m| m.extra.get(crate::derive::EXTRA_KEY))
        .and_then(|o| serde_json::from_value(o["generation_ids"].clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
