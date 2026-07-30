#![doc = include_str!("../README.md")]

pub mod apply;
pub mod detect;
pub mod exec;
pub mod internal;
pub mod plan;
pub mod surface;
pub mod transform;

pub use apply::apply;
pub use detect::{
    Candidate, Context, Detector, DetectorSet, Egress, FieldShape, Finding, FixedDetector,
};
pub use plan::{
    Action, Cmp, Decision, Plan, PlanFinding, Predicate, RedactionPolicy, parse_predicate,
};
pub use surface::{Surface, surfaces};
pub use transform::{Fingerprint, Transform};

use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error)]
pub enum RedactError {
    #[error("detector {0} performs network I/O; pass --allow-network-detectors to permit it")]
    NetworkDetectorRefused(String),
    #[error("plan does not match document: {0}")]
    PlanMismatch(String),
    #[error("document carries signatures over redacted content; pass --drop-signatures")]
    SignedDocument,
    #[error("pointer {0} does not resolve")]
    BadPointer(String),
    #[error("bad predicate: {0}")]
    BadPredicate(String),
    /// A third-party detector's own failure. Carries a message rather than
    /// a source error so the crate can keep advertising no filesystem.
    #[error("detector {0} failed: {1}")]
    DetectorFailed(String, String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RedactError>;

/// Everything the engine needs, supplied by the caller. No env, no
/// filesystem, no clock, no globals - see the purity rule in the plan.
#[derive(Debug, Clone)]
pub struct RedactConfig {
    pub threshold: f32,
    pub mode: Transform,
    pub mode_for: Vec<(String, Transform)>,
    pub key: Vec<u8>,
    pub now: DateTime<Utc>,
    pub drop_signatures: bool,
    pub reveal: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RedactReport {
    pub steps_touched: usize,
    pub replaced: std::collections::BTreeMap<String, usize>,
    pub flagged: std::collections::BTreeMap<String, usize>,
    pub signatures_dropped: usize,
    pub surfaces_scanned: usize,
}
