mod corpus;
mod id;
mod kv;
mod migration;
mod open;
mod permission;
mod project;
mod transaction;
mod workspace;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static IDENTIFIERS: Mutex<()> = Mutex::new(());

pub(crate) fn lock_identifiers() -> MutexGuard<'static, ()> {
    IDENTIFIERS.lock().unwrap_or_else(|error| error.into_inner())
}

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
