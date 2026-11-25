use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ConvoError>;

#[derive(Debug, Error)]
pub enum ConvoError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Home directory not found")]
    NoHomeDirectory,

    #[error("antigravity-cli directory not found at path: {0}")]
    AntigravityDirectoryNotFound(PathBuf),

    #[error("conversation database not found at path: {0}")]
    DatabaseNotFound(PathBuf),

    #[error("conversation not found: {0}")]
    ConversationNotFound(String),

    #[error("malformed step payload: {0}")]
    MalformedPayload(String),

    #[error("generic error: {0}")]
    Other(#[from] anyhow::Error),
}
