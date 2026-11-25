mod corpus;
mod instructions;
mod model_request;
mod retry;
mod runner;
mod search_index;
mod store;
mod usage;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opencode_config::entry::Entry;
use opencode_location::state::Registration;
use opencode_registry::agent::AgentService;
use opencode_registry::builtin;
use opencode_registry::config::agent;
use opencode_registry::events::RegistryEvents;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "opencode-session-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
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

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

struct TestAgents {
    service: AgentService,
    _registration: Registration,
}

async fn test_agents(entries: &[Entry], home: &str) -> TestAgents {
    let service = AgentService::new(Arc::new(NoEvents));
    let documents = agent::load(entries);
    let home = PathBuf::from(home);
    let registration = service
        .transform(move |editor| {
            builtin::agent::agent(editor);
            builtin::agent::plan(editor, &home);
            agent::apply(&documents, &home, editor);
        })
        .await;
    TestAgents { service, _registration: registration }
}
