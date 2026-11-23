//! The storage crate's integration tests: the database path, the connection and transactions.

mod open;
mod transaction;

use std::path::{Path, PathBuf};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("opencode-storage-{:016x}", u64::from_ne_bytes(rand::random::<[u8; 8]>())));
        std::fs::create_dir_all(&path).expect("create the temp directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
