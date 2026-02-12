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

    #[error("Claude directory not found at path: {0}")]
    ClaudeDirectoryNotFound(PathBuf),

    #[error("Project directory not found: {0}")]
    ProjectNotFound(String),

    #[error("Conversation not found: {0}")]
    ConversationNotFound(String),

    #[error("Invalid conversation format in file: {0}")]
    InvalidFormat(PathBuf),

    #[error("Path conversion error: {0}")]
    PathConversion(String),

    #[error("Timestamp parsing error: {0}")]
    TimestampParse(String),

    #[error("Generic error: {0}")]
    Other(#[from] anyhow::Error),
}
