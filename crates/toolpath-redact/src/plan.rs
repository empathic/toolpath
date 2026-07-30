//! The reviewable artifact between detection and rewriting.

use crate::{detect::FieldShape, transform::Transform};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Redact,
    Skip,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanFinding {
    pub id: String,
    pub step: String,
    pub at: String,
    pub rule: String,
    pub span: (usize, usize),
    pub score: f32,
    pub detector: String,
    pub shape: FieldShape,
    /// Surrounding line with the match replaced by its rule name. Never
    /// the value, never its length, unless `reveal` was set.
    pub context: String,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanDefaults {
    pub transform: Transform,
    pub threshold: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub v: u32,
    pub document: String,
    pub generated: DateTime<Utc>,
    pub detectors: Vec<String>,
    pub defaults: PlanDefaults,
    pub surfaces: Vec<crate::surface::Surface>,
    pub findings: Vec<PlanFinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub predicate: Predicate,
    pub action: Action,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Rule(String),
    Shape(FieldShape),
    Step(String),
    Detector(String),
    AtPrefix(String),
    Score(Cmp, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

/// Persisted in the sync manifest so a re-derive can replay redaction.
/// Rule-based only: individual finding ids cannot be replayed against
/// content that has moved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RedactionPolicy {
    pub detectors: Vec<String>,
    pub threshold: f32,
    pub mode: Transform,
    #[serde(default)]
    pub mode_for: Vec<(String, Transform)>,
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub reject: Vec<String>,
    pub key_id: String,
}

// ── Plan machinery (T5) ────────────────────────────────────────────────

pub fn parse_predicate(_s: &str) -> crate::Result<Predicate> {
    todo!("T5")
}

/// Later decisions override earlier ones, so a caller can express
/// "redact everything, except this" by ordering.
pub fn apply_decisions(_plan: &mut Plan, _decisions: &[Decision]) {
    todo!("T5")
}

/// Stable, ordinal finding id (`f01`, `f02`, …). Stability is what makes a
/// regenerated plan byte-identical to its predecessor.
pub fn finding_id(_index: usize) -> String {
    todo!("T5")
}

/// The line around `span` with the match replaced by `<rule>` - never the
/// value and never anything from which its length can be read, unless
/// `reveal` was set.
pub fn elide_context(
    _text: &str,
    _span: std::ops::Range<usize>,
    _rule: &str,
    _reveal: bool,
) -> String {
    todo!("T5")
}

/// Refuse a plan that no longer describes this document, naming the first
/// divergence.
pub fn verify(_plan: &Plan, _path: &toolpath::v1::Path) -> crate::Result<()> {
    todo!("T5")
}

// ── Plan generation (T8) ───────────────────────────────────────────────

pub fn generate(
    _path: &toolpath::v1::Path,
    _detectors: &crate::detect::DetectorSet,
    _cfg: &crate::RedactConfig,
) -> Plan {
    todo!("T8")
}

/// `generate`, plus the egress check: a detector that would send candidate
/// material off the machine is refused unless the caller allowed it.
pub fn generate_checked(
    _path: &toolpath::v1::Path,
    _detectors: &crate::detect::DetectorSet,
    _cfg: &crate::RedactConfig,
    _allow_network: bool,
) -> crate::Result<Plan> {
    todo!("T8")
}
