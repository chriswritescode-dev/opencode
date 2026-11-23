//! The SQLite database: its path, the shared connection, pragmas and transactions.

pub mod database;
pub mod error;
pub mod generated;
pub mod path;

pub use database::Database;
pub use error::StorageError;
pub use path::{DatabaseLocation, database_path};
