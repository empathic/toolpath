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
    /// Input bytes that do not decode: JSON syntax, or a non-OTLP line
    /// among OTLP lines.
    #[error("not OTLP/JSON: {0}")]
    Json(String),
    /// A compressed input whose stream is corrupt, or that exceeds the
    /// nesting cap.
    #[error("cannot decompress: {0}")]
    Decompress(String),
    /// Decompressing the input, across all its layers and frames, would
    /// produce more than `limit` bytes.
    #[error(
        "cannot decompress: decompressed output exceeds {}",
        crate::input::human_bytes(*limit)
    )]
    TooLarge {
        /// The decompressed-size cap, in bytes.
        limit: u64,
    },
    /// Input that needs a crate feature this build lacks
    /// (`"compression"`, `"protobuf"`).
    #[error("input needs the `{0}` feature, which this build does not have")]
    FeatureDisabled(&'static str),
    /// Bytes that are neither JSON text nor an OTLP traces or logs protobuf
    /// request. Inside a Collector frame the same bytes are
    /// [`OtelError::Protobuf`].
    #[error("not an OTLP request: {0}")]
    NotOtlpBody(String),
    /// A malformed OTLP protobuf request, or a request `encode_protobuf`
    /// cannot encode.
    #[error("OTLP protobuf: {0}")]
    Protobuf(String),
    /// A Collector file-exporter frame sequence (4-byte big-endian length
    /// prefixes) that is cut short or declares more bytes than remain.
    #[error("OTLP file framing: {0}")]
    Framing(String),
}

impl OtelError {
    /// The input is not OTLP at all, as opposed to OTLP that is malformed,
    /// cut short or unreadable in this build.
    pub fn is_not_otlp(&self) -> bool {
        matches!(self, OtelError::NotOtlp | OtelError::NotOtlpBody(_))
    }

    /// The input is over the decompressed-size limit, as opposed to
    /// compressed data that is corrupt.
    pub fn is_too_large(&self) -> bool {
        matches!(self, OtelError::TooLarge { .. })
    }
}

/// A `Result` whose error is [`OtelError`].
pub type Result<T> = std::result::Result<T, OtelError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn others() -> Vec<OtelError> {
        vec![
            OtelError::NoGenerations {
                skipped: SkipCounts::default(),
            },
            OtelError::MixedSessions(vec!["a".into(), "b".into()]),
            OtelError::FedGenerationMissing("g".into()),
            OtelError::MessageMissing("m".into()),
            OtelError::UnknownHarness("h".into()),
            OtelError::Delta(DeltaError::Amended {
                steps: vec!["s".into()],
            }),
            OtelError::Json("x".into()),
            OtelError::Decompress("x".into()),
            OtelError::FeatureDisabled("protobuf"),
            OtelError::Protobuf("x".into()),
            OtelError::Framing("x".into()),
        ]
    }

    #[test]
    fn only_the_not_otlp_classes_are_not_otlp() {
        assert!(OtelError::NotOtlp.is_not_otlp());
        assert!(OtelError::NotOtlpBody("x".into()).is_not_otlp());
        assert!(!OtelError::TooLarge { limit: 1 }.is_not_otlp());
        for e in others() {
            assert!(!e.is_not_otlp(), "{e:?}");
        }
    }

    #[test]
    fn only_too_large_is_too_large() {
        assert!(OtelError::TooLarge { limit: 1 }.is_too_large());
        assert!(!OtelError::NotOtlp.is_too_large());
        assert!(!OtelError::NotOtlpBody("x".into()).is_too_large());
        for e in others() {
            assert!(!e.is_too_large(), "{e:?}");
        }
    }

    #[test]
    fn too_large_names_its_limit_in_exact_binary_units() {
        for (limit, text) in [
            (0, "0 B"),
            (1000, "1000 B"),
            (1536, "1536 B"),
            (3 << 10, "3 KiB"),
            (crate::input::MAX_DECOMPRESSED, "1 GiB"),
            (u64::MAX, "18446744073709551615 B"),
        ] {
            assert_eq!(
                OtelError::TooLarge { limit }.to_string(),
                format!("cannot decompress: decompressed output exceeds {text}")
            );
        }
    }
}
