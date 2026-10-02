use crate::SkipCounts;
use thiserror::Error;
use toolpath::v1::jsonl::DeltaError;

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
    /// `Remote::fed` names a generation the records do not hold (the
    /// first such id in feed order).
    #[error("fed generation {0} is not in the records")]
    FedGenerationMissing(String),
    /// `Remote::harness` names no harness this crate records.
    #[error("unknown harness {0:?} in the stored path's meta")]
    UnknownHarness(String),
    /// A record's prompt names a message (by this hash) the message lookup
    /// does not hold, or a chain of more than 2^20 messages (a looping
    /// store).
    #[error("message {0} is not in the message store")]
    MessageMissing(String),
    /// The incremental send cannot be expressed as appends; `Amended` names
    /// stored steps the session now derives with other content.
    #[error("incremental JSONL: {0}")]
    Delta(#[from] DeltaError),
}

/// A `Result` whose error is [`OtelError`].
pub type Result<T> = std::result::Result<T, OtelError>;
