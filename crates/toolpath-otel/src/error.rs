use crate::SkipCounts;
use thiserror::Error;

/// What can go wrong reading OTLP telemetry or deriving from it.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum OtelError {
    /// A request body that is not a JSON object holding a `resourceSpans`,
    /// `resourceLogs` or `resourceMetrics` array (or `null`).
    #[error("not OTLP: expected a `resourceSpans`, `resourceLogs` or `resourceMetrics` array")]
    NotOtlp,
    /// The requests hold no generation the consulted profiles can read.
    #[error("no generation in the requests ({} skipped)", skipped.total())]
    NoGenerations {
        /// What was read but not derived, by reason.
        skipped: SkipCounts,
    },
    /// The requests' generations carry these distinct client session ids
    /// (sorted); one call derives one session.
    #[error("requests mix sessions: {}", .0.join(", "))]
    MixedSessions(Vec<String>),
}

/// A `Result` whose error is [`OtelError`].
pub type Result<T> = std::result::Result<T, OtelError>;
