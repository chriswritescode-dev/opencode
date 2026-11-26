mod activity;
mod environment;
mod lifecycle;
mod map;
mod project;
mod resolve;
mod state;
mod state_group;
mod state_replay;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opencode_location::project::ProjectService;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn test_git() -> Arc<opencode_git::Git> {
    Arc::new(opencode_git::Git::new(Arc::new(|name: &str| std::env::var_os(name))))
}

pub fn project_service(database: Arc<opencode_storage::Database>) -> ProjectService {
    ProjectService::new(database, test_git())
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "opencode-location-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Self { path }
    }

    fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
