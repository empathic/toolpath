use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CursorError>;

#[derive(Debug, Error)]
pub enum CursorError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Cursor data directory not found at path: {0}")]
    CursorDataDirectoryNotFound(PathBuf),

    #[error("Cursor global state database not found at path: {0}")]
    DatabaseNotFound(PathBuf),

    #[error("Composer not found: {0}")]
    ComposerNotFound(String),

    #[error("Malformed composer or bubble payload for {what}: {detail}")]
    MalformedPayload { what: String, detail: String },

    #[error("Generic error: {0}")]
    Other(#[from] anyhow::Error),
}
