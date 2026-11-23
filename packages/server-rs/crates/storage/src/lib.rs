//! The SQLite database: its path, the shared connection, pragmas and transactions.

pub mod database;
pub mod dump;
pub mod error;
pub mod generated;
pub mod id;
pub mod migration;
pub mod path;

pub use database::Database;
pub use error::StorageError;
pub use migration::MigrationReport;
pub use path::{DatabaseLocation, database_path};
