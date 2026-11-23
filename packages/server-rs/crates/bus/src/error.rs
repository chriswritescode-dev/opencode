use opencode_storage::StorageError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BusError {
    #[error("{message}")]
    InvalidDurableEvent { r#type: String, message: String },
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
