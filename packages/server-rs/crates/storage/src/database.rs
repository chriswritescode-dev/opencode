use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::StorageError;
use crate::migration::{MigrationReport, apply};
use crate::path::DatabaseLocation;

static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

pub struct Database {
    connection: Mutex<Connection>,
    location: DatabaseLocation,
}

impl Database {
    pub fn open(location: DatabaseLocation, data: &Path) -> Result<(Database, MigrationReport), StorageError> {
        let lock = match &location {
            DatabaseLocation::File(path) => Some(path_lock(path)),
            DatabaseLocation::Memory => None,
        };
        let _guard = lock.as_ref().map(|lock| lock.lock().unwrap_or_else(|error| error.into_inner()));
        if let DatabaseLocation::File(path) = &location {
            restrict_to_owner(path)?;
        }
        let connection = match &location {
            DatabaseLocation::File(path) => Connection::open(path)?,
            DatabaseLocation::Memory => Connection::open_in_memory()?,
        };
        let _journal_mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "busy_timeout", 5000)?;
        connection.pragma_update(None, "cache_size", -64000)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let database = Database { connection: Mutex::new(connection), location };
        let legacy_credentials = match database.location() {
            DatabaseLocation::File(_) => Some(data.join("auth.json")),
            DatabaseLocation::Memory => None,
        };
        let report = apply(&database, legacy_credentials.as_deref())?;
        Ok((database, report))
    }

    pub fn location(&self) -> &DatabaseLocation {
        &self.location
    }

    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, StorageError>) -> Result<T, StorageError> {
        let connection = self.connection.lock().unwrap_or_else(|error| error.into_inner());
        f(&connection)
    }

    pub fn transaction<T, E: From<rusqlite::Error>>(
        &self,
        behavior: TransactionBehavior,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut connection = self.connection.lock().unwrap_or_else(|error| error.into_inner());
        let mut tx = connection.transaction_with_behavior(behavior)?;
        let value = match f(&mut tx) {
            Ok(value) => value,
            Err(error) => {
                let _ = tx.rollback();
                return Err(error);
            }
        };
        match tx.commit() {
            Ok(()) => Ok(value),
            Err(error) => {
                if !connection.is_autocommit() {
                    let _ = connection.execute_batch("ROLLBACK");
                }
                Err(error.into())
            }
        }
    }
}

fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
    locks.entry(path.to_path_buf()).or_default().clone()
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if !path.exists() {
        OpenOptions::new().append(true).create(true).mode(0o600).open(path)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match fs::set_permissions(PathBuf::from(sidecar), fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}
