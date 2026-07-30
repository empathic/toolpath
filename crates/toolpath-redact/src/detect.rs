//! The detection contract.
//!
//! Detectors see strings and a little context, never the document. That is
//! what makes them testable in isolation and leaves a harness-time hook
//! path open.

use std::ops::Range;

/// What kind of text a candidate holds. Detectors use it to decide how to
/// read the string; the plan reports it so a reviewer can filter on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldShape {
    Prose,
    ToolInput,
    ToolOutput,
    UnifiedDiff,
    FileContent,
    Uri,
    OpaqueJson,
}

/// Where in the document a candidate came from, in terms a detector can
/// act on without resolving pointers itself.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub change_type: &'a str,
    pub tool_name: Option<&'a str>,
    pub actor: &'a str,
    pub kind: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub text: &'a str,
    pub shape: FieldShape,
    /// RFC 6901 pointer relative to the step. Passed through to the audit
    /// record verbatim, so a detector never constructs one.
    pub at: &'a str,
    pub ctx: Context<'a>,
}

/// One span a detector claims is a secret. `span` indexes `Candidate::text`
/// in bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub span: Range<usize>,
    /// Lands in the audit record, so it is part of the document contract.
    pub rule: String,
    pub score: f32,
    pub detector: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egress {
    LocalOnly,
    Network,
}

pub trait Detector: Send + Sync {
    fn id(&self) -> &'static str;

    fn detect(&self, c: &Candidate<'_>) -> crate::Result<Vec<Finding>>;

    /// A cheap rejection test run before `detect`. Returning `false` skips
    /// this detector for this candidate entirely.
    fn prefilter(&self, _text: &str) -> bool {
        true
    }

    /// The host refuses a `Network` detector unless explicitly allowed:
    /// validating a candidate against its issuing provider sends secret
    /// material off the machine.
    fn egress(&self) -> Egress {
        Egress::LocalOnly
    }
}

#[derive(Default)]
pub struct DetectorSet(Vec<Box<dyn Detector>>);

impl DetectorSet {
    pub fn push(&mut self, d: Box<dyn Detector>) {
        self.0.push(d);
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.0.iter().map(|d| d.id()).collect()
    }

    /// The detectors in the set, in insertion order.
    pub fn detectors(&self) -> &[Box<dyn Detector>] {
        &self.0
    }

    /// Run every detector and reconcile their output into one set of
    /// non-overlapping, applicable spans.
    pub fn detect_all(&self, _c: &Candidate<'_>) -> crate::Result<Vec<Finding>> {
        todo!("T1")
    }
}
