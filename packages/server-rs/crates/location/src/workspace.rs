use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_storage::{Database, StorageError, workspace};
use serde_json::Value;

use crate::environment::Environment;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("Workspace not found: {0}")]
    NotFound(String),
    #[error("Workspace provider not found: {0}")]
    ProviderNotFound(String),
    #[error("Workspace has no binding: {0}")]
    MissingBinding(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

pub trait WorkspaceDriver: Send + Sync {
    fn connect<'a>(
        &'a self,
        id: &'a str,
        binding: &'a Value,
    ) -> BoxFuture<'a, Result<Arc<dyn Environment>, WorkspaceError>>;
}

#[derive(Default)]
pub struct Workspaces {
    drivers: HashMap<String, Arc<dyn WorkspaceDriver>>,
}

impl Workspaces {
    pub fn new() -> Self {
        Workspaces { drivers: HashMap::new() }
    }

    pub fn register(&mut self, provider: impl Into<String>, driver: Arc<dyn WorkspaceDriver>) {
        self.drivers.insert(provider.into(), driver);
    }

    pub async fn connect(&self, database: &Database, id: &str) -> Result<Arc<dyn Environment>, WorkspaceError> {
        let row = database
            .read(|connection| workspace::get(connection, id))?
            .ok_or_else(|| WorkspaceError::NotFound(id.to_string()))?;
        let driver = self
            .drivers
            .get(&row.provider)
            .cloned()
            .ok_or_else(|| WorkspaceError::ProviderNotFound(row.provider.clone()))?;
        let binding = row.binding.as_ref().ok_or_else(|| WorkspaceError::MissingBinding(id.to_string()))?;
        let environment = driver.connect(id, binding).await?;
        database.read(|connection| workspace::touch(connection, id))?;
        Ok(environment)
    }
}
