use std::future::Future;
use std::io;
use std::path::Path;

use opencode_local::lock;
use tokio::task::spawn_blocking;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("Timed out waiting for lock: {0}")]
    Timeout(String),
    #[error("{0}")]
    Compromised(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<lock::Error> for LockError {
    fn from(error: lock::Error) -> Self {
        match error {
            lock::Error::Timeout(key) => Self::Timeout(key),
            lock::Error::Compromised(reason) => Self::Compromised(reason),
            lock::Error::Io(error) => Self::Io(error),
        }
    }
}

impl From<tokio::task::JoinError> for LockError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Io(io::Error::other(error))
    }
}

pub async fn with_lock<T>(
    dir: &Path,
    key: &str,
    options: lock::Options,
    future: impl Future<Output = T>,
) -> Result<T, LockError> {
    let directory = dir.to_path_buf();
    let name = key.to_owned();
    let held = spawn_blocking(move || lock::acquire(&directory, &name, &options)).await??;
    let value = future.await;
    spawn_blocking(move || held.release()).await??;
    Ok(value)
}
