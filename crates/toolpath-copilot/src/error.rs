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

    #[error("Copilot directory not found at path: {0}")]
    CopilotDirectoryNotFound(PathBuf),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Invalid session file format: {0}")]
    InvalidFormat(PathBuf),

    #[error("Generic error: {0}")]
    Other(#[from] anyhow::Error),
}
