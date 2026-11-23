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

impl BusError {
    pub(crate) fn unknown_durable_event(r#type: &str) -> BusError {
        BusError::InvalidDurableEvent {
            r#type: r#type.to_string(),
            message: format!("Unknown durable event type {}", r#type),
        }
    }
}
