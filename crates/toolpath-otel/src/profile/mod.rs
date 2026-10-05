//! Profiles: one per telemetry dialect, and the only modules that name
//! attribute keys. Every profile yields the neutral [`Generation`].

pub mod openinference;
pub mod openrouter;
pub mod semconv;

use crate::generation::Generation;
use crate::otlp::{LogRecord, Resource, Scope, Span};
use crate::walk::{ReadCx, SkipReason};
use std::collections::HashMap;

/// A telemetry dialect.
pub trait Profile: Sync {
    /// Stable, lowercase: "openrouter", "semconv", "openinference".
    /// Recorded in meta and in `Generation::profile`.
    fn name(&self) -> &'static str;
    /// This span is one model call (or a profile-specific skip such as a
    /// connection test).
    fn claims(&self, resource: &Resource, scope: &Scope, span: &Span) -> bool;
    /// Known auxiliary telemetry: not a generation, not unclaimed, visible
    /// to `extract` through [`TraceView`].
    fn absorbs(&self, _resource: &Resource, _scope: &Scope, _span: &Span) -> bool {
        false
    }
    /// A skip that needs no ids and precedes the status gate.
    fn pre_skip(&self, _unit: &Unit<'_>) -> Option<SkipReason> {
        None
    }
    /// An orphan log record this profile turns into (part of) a generation.
    fn claims_log(&self, _resource: &Resource, _log: &LogRecord) -> bool {
        false
    }
    /// Partition this profile's claimed orphan records into one group per
    /// generation.
    fn group_logs<'a>(&self, logs: Vec<LogRef<'a>>) -> Vec<Vec<LogRef<'a>>> {
        default_group_logs(logs)
    }
    /// `source_meta` keys that describe the session; derive lifts the first
    /// value of each into `meta.extra.otel.<name>`.
    fn session_meta_keys(&self) -> &'static [&'static str] {
        &[]
    }
    /// Generation id and session id, read before the status gate and dedupe.
    ///
    /// Contract: for a unit that `extract` turns into a generation, this
    /// `generation_id` is `Some` and equals that generation's `id`, so the
    /// walker dedupes before extracting (a mismatch is debug-asserted). The
    /// walker tolerates `None`, deduping only after the extract. An empty
    /// id is `None`.
    fn identify(&self, unit: &Unit<'_>) -> Ident;
    /// Build the neutral generation, with the read's memos. `Err` only for
    /// a skip.
    fn extract<'a>(
        &self,
        unit: &Unit<'a>,
        trace: &TraceView<'a>,
        cx: &mut ReadCx<'a>,
    ) -> std::result::Result<Generation, SkipReason>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ident {
    pub generation_id: Option<String>,
    pub session_id: Option<String>,
}

/// One candidate generation: a claimed span or an orphan log group.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Unit<'a> {
    pub resource: &'a Resource<'a>,
    pub scope: &'a Scope<'a>,
    /// `None` for an orphan log group.
    pub span: Option<&'a Span<'a>>,
    /// Correlated records (span unit) or the group (orphan unit), in
    /// `(timeUnixNano, input order)`.
    pub logs: Vec<LogRef<'a>>,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LogRef<'a> {
    pub resource: &'a Resource<'a>,
    pub scope: &'a Scope<'a>,
    pub record: &'a LogRecord<'a>,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SpanRef<'a> {
    pub resource: &'a Resource<'a>,
    pub scope: &'a Scope<'a>,
    pub span: &'a Span<'a>,
}

/// Everything in the batch that shares the unit's trace id; empty for an
/// orphan unit with no trace id. Borrows the walker's per-trace index, so
/// it copies nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct TraceView<'a> {
    spans: &'a [SpanRef<'a>],
    all_logs: &'a [LogRef<'a>],
}

impl<'a> TraceView<'a> {
    pub(crate) fn new(spans: &'a [SpanRef<'a>], all_logs: &'a [LogRef<'a>]) -> Self {
        Self { spans, all_logs }
    }
    pub fn spans(&self) -> impl Iterator<Item = SpanRef<'a>> + '_ {
        self.spans.iter().copied()
    }
    /// Every distinct log record of the trace, whatever it correlates to, in
    /// `(timeUnixNano, input order)`.
    pub fn all_logs(&self) -> impl Iterator<Item = LogRef<'a>> + '_ {
        self.all_logs.iter().copied()
    }
}

/// Default partition: by `(traceId, spanId)` in first-seen
/// order; a record with neither forms its own group.
pub fn default_group_logs<'a>(logs: Vec<LogRef<'a>>) -> Vec<Vec<LogRef<'a>>> {
    let mut groups: Vec<Vec<LogRef<'a>>> = Vec::new();
    let mut by_key: HashMap<(&str, &str), usize> = HashMap::new();
    for l in logs {
        let key = (&*l.record.trace_id, &*l.record.span_id);
        if key.0.is_empty() && key.1.is_empty() {
            groups.push(vec![l]);
            continue;
        }
        match by_key.get(&key) {
            Some(&i) => groups[i].push(l),
            None => {
                by_key.insert(key, groups.len());
                groups.push(vec![l]);
            }
        }
    }
    groups
}

/// Which telemetry dialects are read.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProfileSelection {
    /// `openrouter`, then `semconv`, in rank order.
    #[default]
    Auto,
    /// OpenRouter Broadcast only.
    OpenRouter,
    /// The OpenTelemetry GenAI semantic conventions only.
    Semconv,
    /// OpenInference only; never consulted by `Auto`.
    OpenInference,
}

/// Every profile, explicit-only ones (not in [`AUTO`]) included.
pub(crate) const BUILTIN: &[&dyn Profile] = &[
    &openrouter::OpenRouter,
    &semconv::Semconv,
    &openinference::OpenInference,
];
/// The auto list in rank order. `openrouter` first: its root nested under a
/// semconv span stays an OpenRouter unit, and its marked children are absorbed.
const AUTO: &[&dyn Profile] = &[&openrouter::OpenRouter, &semconv::Semconv];

pub fn by_name(name: &str) -> Option<&'static dyn Profile> {
    BUILTIN.iter().copied().find(|p| p.name() == name)
}

/// The profiles `sel` consults, in rank order.
pub(crate) fn consulted(sel: ProfileSelection) -> &'static [&'static dyn Profile] {
    match sel {
        ProfileSelection::Auto => AUTO,
        ProfileSelection::OpenRouter => &BUILTIN[0..1],
        ProfileSelection::Semconv => &BUILTIN[1..2],
        ProfileSelection::OpenInference => &BUILTIN[2..3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_consult_the_right_profiles() {
        let names = |sel| -> Vec<&str> { consulted(sel).iter().map(|p| p.name()).collect() };
        assert_eq!(names(ProfileSelection::Auto), ["openrouter", "semconv"]);
        assert_eq!(names(ProfileSelection::OpenRouter), ["openrouter"]);
        assert_eq!(names(ProfileSelection::Semconv), ["semconv"]);
        assert_eq!(names(ProfileSelection::OpenInference), ["openinference"]);
        for name in ["openrouter", "semconv", "openinference"] {
            assert_eq!(by_name(name).map(|p| p.name()), Some(name));
        }
    }

    #[test]
    fn default_log_groups_by_trace_and_span() {
        let (r, s) = (Resource::default(), Scope::default());
        let rec = |t: &'static str, sp: &'static str| LogRecord {
            trace_id: t.into(),
            span_id: sp.into(),
            ..Default::default()
        };
        let recs = [
            rec("t", "a"),
            rec("t", "b"),
            rec("t", "a"),
            rec("", ""),
            rec("", ""),
        ];
        let refs: Vec<LogRef> = recs
            .iter()
            .map(|record| LogRef {
                resource: &r,
                scope: &s,
                record,
            })
            .collect();
        let sizes: Vec<usize> = default_group_logs(refs).iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![2, 1, 1, 1]);
    }

    #[test]
    fn default_log_grouping_ignores_id_case() {
        let (r, s) = (Resource::default(), Scope::default());
        let rec = |t: &str, sp: &str| -> LogRecord<'static> {
            crate::otlp::test_read::from_value(serde_json::json!({"traceId": t, "spanId": sp}))
                .unwrap()
        };
        let recs = [
            rec("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331"),
            rec("0AF7651916CD43DD8448EB211C80319C", "B7AD6B7169203331"),
        ];
        let refs: Vec<LogRef> = recs
            .iter()
            .map(|record| LogRef {
                resource: &r,
                scope: &s,
                record,
            })
            .collect();
        let sizes: Vec<usize> = default_group_logs(refs).iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![2]);
    }
}
