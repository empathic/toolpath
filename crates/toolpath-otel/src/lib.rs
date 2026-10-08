#![doc = include_str!("../README.md")]

mod branch;
mod derive;
mod entries;
mod error;
mod generation;
mod group;
mod harness;
mod hash;
mod input;
mod jsonl;
mod normalize;
mod otlp;
mod profile;
#[cfg(feature = "protobuf")]
mod proto;
#[cfg(feature = "protobuf")]
mod protojson;
mod provider;
mod record;
mod session;
mod stitch;
mod walk;

#[cfg(test)]
mod tests;

pub use error::{OtelError, Result};
pub use group::{SessionRequests, derive_session, group_sessions};
pub use harness::SourceHarness;
pub use hash::derived_session_id;
pub use input::{DecodeLimits, decode_input, decode_input_with_limit, decode_input_with_limits};
pub use jsonl::{Remote, Settle, derive_jsonl};
pub use profile::ProfileSelection;
#[cfg(feature = "protobuf")]
pub use protojson::{decode_protobuf, encode_protobuf};
pub use record::{GenerationBatch, GenerationRecord, MessageHash, StoredMessage, read_generations};
/// The `toolpath` types [`derive_jsonl`] takes and returns, so a caller
/// needs no `toolpath` dependency of its own to use it.
pub use toolpath::v1::jsonl::{BatchLimits, Body, DeltaError};

use walk::SkipReason;

use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use toolpath::v1::{Graph, GraphIdentity, GraphMeta, Path, PathOrRef};
use toolpath_convo::ToolCategory;

/// Configuration for deriving Toolpath documents from OTLP telemetry.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Which telemetry dialects are read.
    pub profile: ProfileSelection,
    /// Optional title for graph output.
    pub title: Option<String>,
    /// Per-path options for the shared conversation derivation.
    pub convo: toolpath_convo::DeriveConfig,
    /// Names each tool call's [`ToolCategory`]. Without one no tool call is
    /// categorized, so no sub-agent is recognized (that needs
    /// [`ToolCategory::Delegation`]) and no file change is read from a tool
    /// call's input (that needs [`ToolCategory::FileWrite`]).
    pub tool_category: Option<ToolClassifier>,
}

impl DeriveConfig {
    /// Set [`DeriveConfig::tool_category`] to `f`.
    pub fn with_tool_category(
        mut self,
        f: impl Fn(&str, &str) -> Option<ToolCategory> + Send + Sync + 'static,
    ) -> Self {
        self.tool_category = Some(ToolClassifier::new(f));
        self
    }
}

/// A caller's tool classifier: `(harness, tool name) → category`, where
/// `harness` is the coding agent inferred from the telemetry
/// (`"claude-code"`, `"codex"`, `"opencode"`, `"pi"`, or `"unknown"`; the
/// value recorded as `meta.extra.otel.harness`).
#[derive(Clone)]
pub struct ToolClassifier(Arc<ClassifyFn>);

type ClassifyFn = dyn Fn(&str, &str) -> Option<ToolCategory> + Send + Sync;

impl ToolClassifier {
    pub fn new(f: impl Fn(&str, &str) -> Option<ToolCategory> + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// The category of tool `name` called in a `harness` session.
    pub fn classify(&self, harness: &str, name: &str) -> Option<ToolCategory> {
        (self.0)(harness, name)
    }
}

impl fmt::Debug for ToolClassifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ToolClassifier(..)")
    }
}

/// A derived document and what was left out of it.
#[derive(Debug, Clone)]
pub struct Derived<T> {
    /// The derived document (for `derive_jsonl`, the request bodies).
    pub output: T,
    /// Telemetry read but not derived, by reason.
    pub skipped: SkipCounts,
}

/// Telemetry read but not derived, by reason.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipCounts {
    /// Generations whose span status is an error (e.g. client disconnected).
    pub error_status: usize,
    /// OpenRouter's destination test and settings test generations.
    pub connection_test: usize,
    /// Generations already read from an earlier request.
    pub duplicate: usize,
    /// Generations whose prompt or completion is cut off or malformed.
    pub truncated: usize,
    /// Claimed spans with no generation id or nothing to build from.
    pub missing_payload: usize,
    /// Spans and orphan log records no consulted profile claims.
    pub unclaimed: usize,
}

impl SkipCounts {
    /// The sum of every count.
    pub fn total(&self) -> usize {
        self.error_status
            + self.connection_test
            + self.duplicate
            + self.truncated
            + self.missing_payload
            + self.unclaimed
    }

    fn from_outcome(out: &walk::ReadOutcome) -> Self {
        let mut c = SkipCounts {
            unclaimed: out.unclaimed,
            ..Default::default()
        };
        for s in &out.skipped {
            *match s.reason {
                SkipReason::ErrorStatus => &mut c.error_status,
                SkipReason::ConnectionTest => &mut c.connection_test,
                SkipReason::Duplicate => &mut c.duplicate,
                SkipReason::Truncated => &mut c.truncated,
                SkipReason::MissingPayload => &mut c.missing_payload,
            } += 1;
        }
        c
    }

    fn add(&mut self, o: &SkipCounts) {
        self.error_status += o.error_status;
        self.connection_test += o.connection_test;
        self.duplicate += o.duplicate;
        self.truncated += o.truncated;
        self.missing_payload += o.missing_payload;
        self.unclaimed += o.unclaimed;
    }
}

/// Derive a Toolpath [`Graph`] from sessions, each given as the parsed
/// OTLP/HTTP JSON request bodies (`resourceSpans` and/or `resourceLogs`)
/// of one session. One session yields a single-path graph.
///
/// # Errors
///
/// As [`derive_path`], for the first session that fails.
pub fn derive(sessions: &[&[Value]], config: &DeriveConfig) -> Result<Derived<Graph>> {
    match sessions {
        [one] => {
            let d = derive_path(one, config)?;
            let mut output = Graph::from_path(d.output);
            output.meta = graph_meta(config);
            Ok(Derived {
                output,
                skipped: d.skipped,
            })
        }
        _ => derive_graph(sessions, config),
    }
}

/// Derive a Toolpath [`Path`] from the OTLP/HTTP JSON request bodies of one
/// session, in start order whatever the order of `requests`.
///
/// The caller owns session grouping. Every generation that carries a
/// client session id (`session.id`, or `gen_ai.conversation.id` for
/// `semconv`) must carry the same one; generations without one belong to
/// the session as given. The session key is that id, else the first
/// generation's: a hash of its leading system message, first user message
/// and generation id (full-history requests), or of its trace id (delta
/// requests).
///
/// # Errors
///
/// [`OtelError::NotOtlp`] for a body that is not an OTLP object;
/// [`OtelError::MixedSessions`] when generations carry more than one
/// session id; [`OtelError::NoGenerations`] when no generation can be read.
pub fn derive_path(requests: &[Value], config: &DeriveConfig) -> Result<Derived<Path>> {
    derive_keyed(requests, config, None)
}

fn derive_keyed(
    requests: &[Value],
    config: &DeriveConfig,
    key: Option<&str>,
) -> Result<Derived<Path>> {
    let (mut session, skipped) = read_session(requests, config)?;
    if let Some(key) = key {
        session.key = key.to_string();
    }
    Ok(Derived {
        output: derive::derive_session(&session, &config.convo, config.tool_category.as_ref()),
        skipped,
    })
}

/// Derive a Toolpath [`Path`] from the [`GenerationRecord`]s of one
/// session, as [`derive_path`] does from the request bodies they were read
/// from, with `messages` looking up the prompt messages they name. The
/// skip counts are only the duplicate generation ids among `records`; the
/// rest were counted by [`read_generations`].
///
/// Of several records with one generation id the better-ranked profile's
/// is derived, else the first, as [`derive_path`] does, whatever the order
/// of `records`. `messages` must return the message stored under exactly
/// the hash asked for ([`StoredMessage::hash`]); it is not checked, and a
/// wrong message derives wrong content under the right ids.
///
/// # Errors
///
/// [`OtelError::MessageMissing`] when `messages` lacks a named message;
/// [`OtelError::MixedSessions`] and [`OtelError::NoGenerations`] as for
/// [`derive_path`].
pub fn derive_path_from_records<'m>(
    records: &[GenerationRecord],
    messages: impl Fn(&MessageHash) -> Option<&'m StoredMessage>,
    config: &DeriveConfig,
) -> Result<Derived<Path>> {
    let (session, skipped) = session_from_records(records, messages, config, record::Pick::Rank)?;
    Ok(Derived {
        output: derive::derive_session(&session, &config.convo, config.tool_category.as_ref()),
        skipped,
    })
}

/// The one session `requests` hold, in start order, and what was skipped.
fn read_session(
    requests: &[Value],
    config: &DeriveConfig,
) -> Result<(session::Session, SkipCounts)> {
    let out = walk::read_deliveries(requests, config.profile)?;
    let skipped = SkipCounts::from_outcome(&out);
    record::session_of(
        record::entries(out).collect(),
        config.profile,
        record::Pick::Rank,
        skipped,
    )
}

/// The one session `records` hold, in start order, one copy of each
/// generation id as `pick` says, and the duplicates among them.
fn session_from_records<'m>(
    records: &[GenerationRecord],
    messages: impl Fn(&MessageHash) -> Option<&'m StoredMessage>,
    config: &DeriveConfig,
    pick: record::Pick,
) -> Result<(session::Session, SkipCounts)> {
    let entries = record::rebuild(records, messages)?;
    record::session_of(entries, config.profile, pick, SkipCounts::default())
}

/// Derive a Toolpath [`Graph`] with one path per session; the skip counts
/// are summed over the sessions.
///
/// # Errors
///
/// As [`derive_path`], for the first session that fails.
pub fn derive_graph(sessions: &[&[Value]], config: &DeriveConfig) -> Result<Derived<Graph>> {
    let mut skipped = SkipCounts::default();
    let mut paths = Vec::with_capacity(sessions.len());
    for s in sessions {
        let d = derive_path(s, config)?;
        skipped.add(&d.skipped);
        paths.push(d.output);
    }
    let id = paths.first().map_or_else(
        || "graph-otel-empty".to_string(),
        |p| format!("graph-{}", p.path.id.trim_start_matches("path-")),
    );
    let output = Graph {
        graph: GraphIdentity { id },
        paths: paths
            .into_iter()
            .map(|p| PathOrRef::Path(Box::new(p)))
            .collect(),
        meta: graph_meta(config),
    };
    Ok(Derived { output, skipped })
}

fn graph_meta(config: &DeriveConfig) -> Option<GraphMeta> {
    config.title.as_ref().map(|t| GraphMeta {
        title: Some(t.clone()),
        ..Default::default()
    })
}
