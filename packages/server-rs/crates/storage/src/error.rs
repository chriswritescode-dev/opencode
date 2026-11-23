//! The storage layer's errors.

use thiserror::Error;

/// A failure opening the database, querying it or running a transaction.
#[derive(Debug, Error)]
pub enum StorageError {
    /// SQLite rejected the connection or statement.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The database file, a sidecar or their directory could not be accessed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A migration failed with the message the TypeScript server dies with.
    #[error("{0}")]
    Migration(String),
}
