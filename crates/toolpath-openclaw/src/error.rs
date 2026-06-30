//! Error types for `toolpath-openclaw`.

use std::path::PathBuf;
use thiserror::Error;

/// Errors produced by the `toolpath-openclaw` crate.
#[derive(Debug, Error)]
pub enum OpenClawError {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A session file was expected but could not be located.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// An agent directory (`agents/<agentId>/sessions`) was expected but not
    /// found on disk.
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    /// A session JSONL file exists but cannot be interpreted.
    ///
    /// Carries the offending path and a short human-readable reason.
    #[error("invalid session file {path}: {reason}")]
    InvalidSessionFile {
        /// Path to the offending file.
        path: PathBuf,
        /// Short human-readable reason.
        reason: String,
    },

    /// A session header line was present but malformed (missing required
    /// fields, unexpected shape, etc.).
    #[error("malformed session header: {0}")]
    MalformedHeader(String),

    /// The session header declares a format version this crate does not
    /// understand. OpenClaw's reader hard-rejects anything but version 3.
    #[error("unsupported session format version: {0} (expected 3)")]
    UnsupportedVersion(u32),

    /// Wrapped error from `toolpath-convo`.
    #[error("conversation error: {0}")]
    Convo(#[from] toolpath_convo::ConvoError),

    /// Catch-all for arbitrary `anyhow` errors bubbling up from dependencies.
    #[error("{0}")]
    Anyhow(#[from] anyhow::Error),

    /// Generic free-form error.
    #[error("{0}")]
    Other(String),
}

impl OpenClawError {
    /// Construct a `SessionNotFound` error.
    pub fn session_not_found(id: impl Into<String>) -> Self {
        Self::SessionNotFound(id.into())
    }

    /// Construct an `AgentNotFound` error.
    pub fn agent_not_found(agent_id: impl Into<String>) -> Self {
        Self::AgentNotFound(agent_id.into())
    }

    /// Construct an `InvalidSessionFile` error.
    pub fn invalid_session_file(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self::InvalidSessionFile {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Construct a `MalformedHeader` error.
    pub fn malformed_header(reason: impl Into<String>) -> Self {
        Self::MalformedHeader(reason.into())
    }

    /// Construct an `Other` error.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, OpenClawError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn io_error_conversion() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "missing");
        let err: OpenClawError = io_err.into();
        assert!(matches!(err, OpenClawError::Io(_)));
    }

    #[test]
    fn json_error_display() {
        let json_err = serde_json::from_str::<u32>("x").unwrap_err();
        let err: OpenClawError = json_err.into();
        assert!(err.to_string().to_lowercase().contains("json"));
    }

    #[test]
    fn session_not_found_display() {
        assert!(
            OpenClawError::SessionNotFound("abc".into())
                .to_string()
                .contains("abc")
        );
    }

    #[test]
    fn agent_not_found_display() {
        assert!(
            OpenClawError::agent_not_found("main")
                .to_string()
                .contains("main")
        );
    }

    #[test]
    fn unsupported_version_display() {
        let msg = OpenClawError::UnsupportedVersion(2).to_string();
        assert!(msg.contains('2') && msg.contains('3'));
    }

    #[test]
    fn invalid_session_file_display() {
        let err = OpenClawError::invalid_session_file(PathBuf::from("/tmp/a.jsonl"), "bad line 3");
        let msg = err.to_string();
        assert!(msg.contains("/tmp/a.jsonl") && msg.contains("bad line 3"));
    }

    #[test]
    fn helper_constructors() {
        assert!(matches!(
            OpenClawError::session_not_found("s"),
            OpenClawError::SessionNotFound(_)
        ));
        assert!(matches!(
            OpenClawError::agent_not_found("a"),
            OpenClawError::AgentNotFound(_)
        ));
        assert!(matches!(
            OpenClawError::other("o"),
            OpenClawError::Other(_)
        ));
    }
}
