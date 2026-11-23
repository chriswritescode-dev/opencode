use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Migration(String),
    #[error("Path is not absolute: {0}")]
    NotAbsolute(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
