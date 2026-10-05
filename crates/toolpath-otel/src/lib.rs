#![doc = include_str!("../README.md")]

mod branch;
mod derive;
mod error;
mod generation;
mod harness;
mod hash;
mod normalize;
mod otlp;
mod profile;
mod provider;
mod session;
mod stitch;
mod walk;

#[cfg(test)]
mod tests;

pub use error::{OtelError, Result};
pub use profile::ProfileSelection;

use walk::SkipReason;

use serde_json::Value;
use std::collections::BTreeSet;
use toolpath::v1::{Graph, GraphIdentity, GraphMeta, Path, PathOrRef};

/// Configuration for deriving Toolpath documents from OTLP telemetry.
#[derive(Debug, Clone, Default)]
pub struct DeriveConfig {
    /// Which telemetry dialects are read.
    pub profile: ProfileSelection,
    /// Optional title for graph output.
    pub title: Option<String>,
    /// Per-path options for the shared conversation derivation.
    pub convo: toolpath_convo::DeriveConfig,
}

/// A derived document and what was left out of it.
#[derive(Debug, Clone)]
pub struct Derived<T> {
    /// The derived document.
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
/// generation's: a hash of its leading system and first user message
/// (full-history requests), or of its trace id (delta requests).
///
/// # Errors
///
/// [`OtelError::NotOtlp`] for a body that is not an OTLP object;
/// [`OtelError::MixedSessions`] when generations carry more than one
/// session id; [`OtelError::NoGenerations`] when no generation can be read.
pub fn derive_path(requests: &[Value], config: &DeriveConfig) -> Result<Derived<Path>> {
    let out = walk::read_deliveries(requests, config.profile)?;
    let skipped = SkipCounts::from_outcome(&out);
    let ids: BTreeSet<&str> = out
        .generations
        .iter()
        .filter_map(|g| g.session_id.as_deref())
        .collect();
    if ids.len() > 1 {
        return Err(OtelError::MixedSessions(
            ids.into_iter().map(str::to_string).collect(),
        ));
    }
    // Every request belongs to this session by contract, so a truncated
    // skip without a session id counts too.
    let truncated = out.skipped.iter().any(|s| {
        s.reason == SkipReason::Truncated
            && s.session_id
                .as_deref()
                .is_none_or(|id| ids.is_empty() || ids.contains(id))
    });
    let mut session = session::Session::from_generations(out.generations)
        .ok_or(OtelError::NoGenerations { skipped })?;
    session.truncated = truncated;
    Ok(Derived {
        output: derive::derive_session(&session, &config.convo),
        skipped,
    })
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
