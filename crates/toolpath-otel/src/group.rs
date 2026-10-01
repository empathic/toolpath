//! Session grouping: generations → sessions in layer order 1 → C → 2 → T,
//! and a batch of requests → the requests of each session.

use crate::error::{OtelError, Result};
use crate::generation::Generation;
use crate::normalize::{NormMessage, kept_prompt};
use crate::otlp::{LogRecord, ResourceLogs, ResourceSpans, ScopeLogs, ScopeSpans, Span};
use crate::profile::ProfileSelection;
use crate::session::{Session, cluster_key, trace_key};
use crate::walk::{self, Attribution, UnitId};
use crate::{Derived, SkipCounts};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The requests of one session, as [`group_sessions`] splits a batch.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRequests {
    /// The client session id, else `otel-cluster:<16 hex>` (full-history
    /// requests clustered by prompt prefix) or `otel-trace:<16 hex>` (delta
    /// requests by trace). A derived key equal to a client session id of
    /// the batch takes a `-<n>` suffix.
    pub key: String,
    /// The client session id, when the requests carry one.
    pub session_id: Option<String>,
    /// The request bodies cut down to this session's spans and log records,
    /// in batch order; bodies holding none of them are left out.
    pub requests: Vec<Value>,
}

impl SessionRequests {
    /// The derived session id of [`key`](Self::key): the id the derived
    /// path records as `meta.otel.derived_session_id`.
    pub fn derived_session_id(&self) -> String {
        crate::hash::derived_session_id(&self.key)
    }
}

/// Split a batch of OTLP/HTTP JSON request bodies into sessions, for
/// callers whose traffic carries no session id or mixes sessions. Each
/// generation joins, first match wins in start order:
///
/// - Layer 1: the session of its client session id;
/// - Layer C: the session of the generation it continues (`continues`);
/// - Layer 2: full-history requests without an id: the session whose
///   latest prompt its prompt extends, per client key;
/// - Layer T: delta requests: one session per client key and trace.
///
/// Spans and log records travel with the generation they belong to; the
/// other spans and records of a trace go to every session with a
/// generation in that trace. Sessions are in the start order of their
/// first generation, and the skip counts are the batch's.
///
/// Deriving a session's `requests` with [`crate::derive_path`] gives the
/// path [`derive_session`] gives, unless its key took a suffix.
///
/// # Errors
///
/// [`OtelError::NotOtlp`] for a body that is not an OTLP object.
pub fn group_sessions(
    requests: &[Value],
    profile: ProfileSelection,
) -> Result<Derived<Vec<SessionRequests>>> {
    let pruned: Vec<Value> = requests.iter().map(prune).collect::<Result<_>>()?;
    let (mut out, attribution) = walk::read_attributed(&pruned, profile)?;
    let skipped = SkipCounts::from_outcome(&out);
    let sessions = group_generations(std::mem::take(&mut out.generations));

    let mut of_generation: HashMap<&str, usize> = HashMap::new();
    let mut of_session_id: HashMap<&str, usize> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        for g in &s.generations {
            of_generation.insert(g.id.as_str(), i);
        }
        if let Some(id) = &s.session_id {
            of_session_id.insert(id.as_str(), i);
        }
    }
    let unit_session: HashMap<UnitId, usize> = attribution
        .units
        .iter()
        .filter_map(|(u, o)| {
            let by_generation = o
                .generation_id
                .as_deref()
                .and_then(|id| of_generation.get(id));
            let by_session = o.session_id.as_deref().and_then(|id| of_session_id.get(id));
            by_generation.or(by_session).map(|s| (*u, *s))
        })
        .collect();

    let split = Split::new(&pruned, &attribution, &unit_session);
    let mut grouped: Vec<SessionRequests> = sessions
        .into_iter()
        .map(|s| SessionRequests {
            key: s.key,
            session_id: s.session_id,
            requests: Vec::new(),
        })
        .collect();
    let mut cursor = Cursor::default();
    for body in &pruned {
        for (session, cut) in split.cut(body, &mut cursor) {
            grouped[session].requests.push(cut);
        }
    }
    Ok(Derived {
        output: grouped,
        skipped,
    })
}

/// Derive the [`Path`](toolpath::v1::Path) of one session from
/// [`group_sessions`], under the session's own key.
///
/// # Errors
///
/// As [`crate::derive_path`].
pub fn derive_session(
    session: &SessionRequests,
    config: &crate::DeriveConfig,
) -> Result<Derived<toolpath::v1::Path>> {
    crate::derive_keyed(&session.requests, config, Some(&session.key))
}

/// `body` with every list element the walker cannot read dropped, so that
/// the walker and [`Split`] number spans and records alike.
fn prune(body: &Value) -> Result<Value> {
    if !crate::otlp::is_otlp(body) {
        return Err(OtelError::NotOtlp);
    }
    let mut body = body.clone();
    if let Some(list) = body.get_mut("resourceSpans") {
        retain(
            list,
            |v| ResourceSpans::read(v).is_some(),
            |rs| {
                if let Some(list) = rs.get_mut("scopeSpans") {
                    retain(
                        list,
                        |v| ScopeSpans::read(v).is_some(),
                        |ss| {
                            if let Some(list) = ss.get_mut("spans") {
                                retain(list, |v| Span::read(v).is_some(), |_| {});
                            }
                        },
                    );
                }
            },
        );
    }
    if let Some(list) = body.get_mut("resourceLogs") {
        retain(
            list,
            |v| ResourceLogs::read(v).is_some(),
            |rl| {
                if let Some(list) = rl.get_mut("scopeLogs") {
                    retain(
                        list,
                        |v| ScopeLogs::read(v).is_some(),
                        |sl| {
                            if let Some(list) = sl.get_mut("logRecords") {
                                retain(list, |v| LogRecord::read(v).is_some(), |_| {});
                            }
                        },
                    );
                }
            },
        );
    }
    Ok(body)
}

/// Keep the elements of `list` that `reads` accepts, then visit each.
fn retain(list: &mut Value, reads: fn(&Value) -> bool, mut visit: impl FnMut(&mut Value)) {
    if let Value::Array(items) = list {
        items.retain(reads);
        items.iter_mut().for_each(&mut visit);
    }
}

/// Where each span and record of the batch goes.
struct Split {
    spans: Vec<Vec<usize>>,
    logs: Vec<Vec<usize>>,
}

/// Running span and record numbers across the batch's bodies.
#[derive(Default)]
struct Cursor {
    span: usize,
    log: usize,
}

fn trace_of(v: &Value) -> String {
    v.get("traceId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn items<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn all_spans(bodies: &[Value]) -> impl Iterator<Item = &Value> {
    bodies
        .iter()
        .flat_map(|b| items(b, "resourceSpans"))
        .flat_map(|rs| items(rs, "scopeSpans"))
        .flat_map(|ss| items(ss, "spans"))
}

fn all_logs(bodies: &[Value]) -> impl Iterator<Item = &Value> {
    bodies
        .iter()
        .flat_map(|b| items(b, "resourceLogs"))
        .flat_map(|rl| items(rl, "scopeLogs"))
        .flat_map(|sl| items(sl, "logRecords"))
}

impl Split {
    fn new(
        bodies: &[Value],
        attribution: &Attribution,
        unit_session: &HashMap<UnitId, usize>,
    ) -> Self {
        let span_session = |n: usize| {
            attribution.spans[n].and_then(|u| unit_session.get(&UnitId::Span(u)).copied())
        };
        let log_session =
            |n: usize| attribution.logs[n].and_then(|u| unit_session.get(&u).copied());
        let mut by_trace: HashMap<String, BTreeSet<usize>> = HashMap::new();
        for (n, span) in all_spans(bodies).enumerate() {
            if let Some(s) = span_session(n) {
                by_trace.entry(trace_of(span)).or_default().insert(s);
            }
        }
        for (n, record) in all_logs(bodies).enumerate() {
            if let Some(s) = log_session(n) {
                by_trace.entry(trace_of(record)).or_default().insert(s);
            }
        }
        by_trace.remove("");
        let followers = |v: &Value| -> Vec<usize> {
            by_trace
                .get(&trace_of(v))
                .map_or_else(Vec::new, |s| s.iter().copied().collect())
        };
        Split {
            spans: all_spans(bodies)
                .enumerate()
                .map(|(n, v)| span_session(n).map_or_else(|| followers(v), |s| vec![s]))
                .collect(),
            logs: all_logs(bodies)
                .enumerate()
                .map(|(n, v)| log_session(n).map_or_else(|| followers(v), |s| vec![s]))
                .collect(),
        }
    }

    /// Each session's cut of `body`, in session order.
    fn cut(&self, body: &Value, cursor: &mut Cursor) -> Vec<(usize, Value)> {
        let mut spans: Vec<&[usize]> = Vec::new();
        for _ in all_spans(std::slice::from_ref(body)) {
            spans.push(&self.spans[cursor.span]);
            cursor.span += 1;
        }
        let mut logs: Vec<&[usize]> = Vec::new();
        for _ in all_logs(std::slice::from_ref(body)) {
            logs.push(&self.logs[cursor.log]);
            cursor.log += 1;
        }
        let sessions: BTreeSet<usize> = spans
            .iter()
            .chain(&logs)
            .flat_map(|s| *s)
            .copied()
            .collect();
        sessions
            .into_iter()
            .map(|s| {
                let mut cut = body.clone();
                let mut n = 0;
                filter(
                    &mut cut,
                    ["resourceSpans", "scopeSpans", "spans"],
                    &mut |_| {
                        n += 1;
                        spans[n - 1].contains(&s)
                    },
                );
                let mut n = 0;
                filter(
                    &mut cut,
                    ["resourceLogs", "scopeLogs", "logRecords"],
                    &mut |_| {
                        n += 1;
                        logs[n - 1].contains(&s)
                    },
                );
                (s, cut)
            })
            .collect()
    }
}

/// Keep the leaves under `path` that `keep` accepts, in order, dropping
/// containers left empty.
fn filter(body: &mut Value, path: [&str; 3], keep: &mut dyn FnMut(&Value) -> bool) {
    let [outer, middle, leaf] = path;
    let Some(Value::Array(resources)) = body.get_mut(outer) else {
        return;
    };
    resources.retain_mut(|r| {
        let Some(Value::Array(scopes)) = r.get_mut(middle) else {
            return false;
        };
        scopes.retain_mut(|sc| {
            let Some(Value::Array(leaves)) = sc.get_mut(leaf) else {
                return false;
            };
            leaves.retain(|l| keep(l));
            !leaves.is_empty()
        });
        !scopes.is_empty()
    });
    if resources.is_empty()
        && let Value::Object(map) = body
    {
        map.remove(outer);
    }
}

struct Cluster {
    session: usize,
    client_key: Option<String>,
    /// Kept prompt of the cluster's latest generation.
    tip: Vec<NormMessage>,
}

/// Generations → sessions in layer order (see [`group_sessions`]), each
/// sorted by `(start_ns, id)`, keyed as [`Session::from_generations`]
/// keys them unless a key collides: then a derived key takes a `-<n>`
/// suffix, and a client id equal to a derived key never joins that session.
pub(crate) fn group_generations(mut gens: Vec<Generation>) -> Vec<Session> {
    gens.sort_by(|a, b| (a.start_ns, &a.id).cmp(&(b.start_ns, &b.id)));
    let client_ids: HashSet<String> = gens.iter().filter_map(|g| g.session_id.clone()).collect();
    let mut sessions: Vec<Session> = Vec::new();
    let mut by_session_id: HashMap<String, usize> = HashMap::new();
    let mut taken_keys: HashSet<String> = HashSet::new();
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut by_trace_key: HashMap<String, usize> = HashMap::new();
    // Generation id → its session, for Layer C. First occurrence wins.
    let mut session_of: HashMap<String, usize> = HashMap::new();

    for g in gens {
        let idx = if let Some(sid) = g.session_id.clone() {
            *by_session_id.entry(sid.clone()).or_insert_with(|| {
                sessions.push(Session::new(sid.clone(), Some(sid), Vec::new()));
                sessions.len() - 1
            })
        } else if let Some(&s) = g.continues.as_ref().and_then(|c| session_of.get(c)) {
            // A Layer C join does not advance the target's Layer 2 cluster tip.
            s
        } else if !g.is_delta() {
            let prompt = kept_prompt(&g.messages);
            // Known limitation: deterministic reruns can merge mid-way, since
            // run 2's prompt may equal run 1's tip once the replies coincide.
            let best = clusters
                .iter_mut()
                .filter(|c| c.client_key == g.client_key && prompt.starts_with(&c.tip))
                .max_by_key(|c| c.tip.len());
            match best {
                Some(c) => {
                    c.tip = prompt;
                    c.session
                }
                None => {
                    let base = cluster_key(g.client_key.as_deref(), &prompt, &g.id);
                    let key = unique_key(base, &mut taken_keys, &client_ids);
                    clusters.push(Cluster {
                        session: sessions.len(),
                        client_key: g.client_key.clone(),
                        tip: prompt,
                    });
                    sessions.push(Session::new(key, None, Vec::new()));
                    sessions.len() - 1
                }
            }
        } else {
            let base = trace_key(g.client_key.as_deref(), &g.trace_id);
            match by_trace_key.get(&base) {
                Some(&s) => s,
                None => {
                    let key = unique_key(base.clone(), &mut taken_keys, &client_ids);
                    sessions.push(Session::new(key, None, Vec::new()));
                    by_trace_key.insert(base, sessions.len() - 1);
                    sessions.len() - 1
                }
            }
        };
        session_of.entry(g.id.clone()).or_insert(idx);
        sessions[idx].generations.push(g);
    }
    sessions
}

/// `base`, or `base-<n>` (`n` from 2) when `base` is already a derived key
/// or any client session id in the batch. Records the result as taken.
fn unique_key(base: String, taken: &mut HashSet<String>, client_ids: &HashSet<String>) -> String {
    let mut key = base.clone();
    let mut n = 2;
    while taken.contains(&key) || client_ids.contains(&key) {
        key = format!("{base}-{n}");
        n += 1;
    }
    taken.insert(key.clone());
    key
}
