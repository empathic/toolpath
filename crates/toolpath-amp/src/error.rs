use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ConvoError>;

#[derive(Debug, Error)]
pub enum ConvoError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Home directory not found")]
    NoHomeDirectory,

    #[error("Amp data directory not found at path: {0}")]
    AmpDirectoryNotFound(PathBuf),

    #[error(
        "amp CLI not found on PATH (install from https://ampcode.com, or import a pre-exported file)"
    )]
    AmpCliNotFound,

    #[error("amp CLI failed ({command}): {stderr}")]
    AmpCliFailed { command: String, stderr: String },

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Invalid thread export format: {0}")]
    InvalidFormat(String),

    #[error("Generic error: {0}")]
    Other(#[from] anyhow::Error),
}
