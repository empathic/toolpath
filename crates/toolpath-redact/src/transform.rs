//! What a redacted span is replaced with.

/// How a detected span is rewritten. Every variant except `Partial` is
/// guaranteed to emit nothing derived from the value's characters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    #[default]
    Marker,
    Remove,
    Hash,
    /// Length-preserving, and therefore publishes the exact length.
    Mask,
    /// Keeps 4 leading and 4 trailing chars: leaks provider and format.
    Partial,
}

/// A short stable handle for a secret value, used to correlate the same
/// secret across occurrences without publishing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub String);

impl Fingerprint {
    /// Keyed, never a bare hash: a hash of a low-entropy secret is a
    /// dictionary attack away from the secret (EDPB 01/2025 para 88).
    pub fn new(_key: &[u8], _value: &str) -> Self {
        todo!("T4")
    }
}

pub trait Transformer: Send + Sync {
    fn id(&self) -> &'static str;
    fn replace(&self, rule: &str, value: &str, fp: &Fingerprint) -> String;
}
