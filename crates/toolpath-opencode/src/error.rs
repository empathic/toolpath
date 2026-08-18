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

    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[error("opencode directory not found at path: {0}")]
    OpencodeDirectoryNotFound(PathBuf),

    #[error("opencode database not found at path: {0}")]
    DatabaseNotFound(PathBuf),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Malformed message or part payload: {0}")]
    MalformedPayload(String),

    #[error("Generic error: {0}")]
    Other(#[from] anyhow::Error),
}
