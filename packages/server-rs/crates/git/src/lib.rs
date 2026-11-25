mod error;
mod executable;
mod history;
mod repo;
mod run;
mod sync;
mod worktree;

pub use error::{CommandError, Operation, OperationError, WorktreeError, WorktreeOperation};
pub use executable::{Platform, darwin_developer_dirs, resolve_git_executable};
pub use repo::{Repository, Worktree, WorktreeKind};
pub use run::{RunOptions, RunOutput, SpawnError};

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Mutex as AsyncMutex;

pub type Vars = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

pub struct Git {
    vars: Vars,
    executable: OnceLock<OsString>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    executions: AtomicUsize,
}

impl Git {
    pub fn new(vars: Vars) -> Self {
        Git { vars, executable: OnceLock::new(), locks: Mutex::new(HashMap::new()), executions: AtomicUsize::new(0) }
    }

    /// Number of git processes this handle started, including failed spawns. Tests use it to
    /// observe a cached discovery that spawns nothing.
    pub fn executions(&self) -> usize {
        self.executions.load(Ordering::Relaxed)
    }

    pub(crate) fn count_execution(&self) {
        self.executions.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn var(&self, name: &str) -> Option<String> {
        (self.vars)(name).map(|value| value.to_string_lossy().into_owned()).filter(|value| !value.is_empty())
    }

    pub(crate) fn executable(&self) -> &OsStr {
        self.executable.get_or_init(|| executable::git_executable(&self.vars)).as_os_str()
    }

    /// Serialises writes to one git directory. Keys are released once the last holder leaves.
    pub async fn with_lock<T>(&self, key: &str, future: impl Future<Output = T>) -> T {
        let entry = {
            let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
            locks.entry(key.to_string()).or_default().clone()
        };
        let guard = entry.clone().lock_owned().await;
        let result = future.await;
        drop(guard);
        let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        if Arc::strong_count(&entry) == 2 {
            locks.remove(key);
        }
        result
    }
}
