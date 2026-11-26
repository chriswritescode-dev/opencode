use std::sync::Arc;

use opencode_contract::generated::{PermissionSavedID, PermissionSavedInfo, PermissionSavedInfoTime, ProjectID};
use opencode_storage::permission::{self, PermissionRow, UpdateOutcome};
use opencode_storage::{Database, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum SavedError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("Saved permission not found: {id:?}")]
    NotFound { id: PermissionSavedID },
    #[error("Saved permission {id:?} conflicts with an existing {action} {resource}")]
    Conflict { id: PermissionSavedID, action: String, resource: String },
}

pub struct PermissionSaved {
    db: Arc<Database>,
}

impl PermissionSaved {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn list(&self, project_id: Option<&str>) -> Result<Vec<PermissionSavedInfo>, StorageError> {
        let rows = self.db.read(|connection| permission::list(connection, project_id))?;
        Ok(rows.into_iter().map(info).collect())
    }

    pub fn add(&self, project_id: &str, action: &str, resources: &[String]) -> Result<(), StorageError> {
        self.db.read(|connection| permission::add(connection, project_id, action, resources))
    }

    pub fn update(
        &self,
        id: &str,
        action: Option<&str>,
        resource: Option<&str>,
    ) -> Result<PermissionSavedInfo, SavedError> {
        let found = self.db.read(|connection| permission::get(connection, id))?;
        let Some(current) = found else {
            return Err(SavedError::NotFound { id: PermissionSavedID(id.to_string()) });
        };
        let action = action.unwrap_or(&current.action);
        let resource = resource.unwrap_or(&current.resource);
        let outcome = self.db.read(|connection| permission::update(connection, id, Some(action), Some(resource)))?;
        match outcome {
            UpdateOutcome::Updated(row) => Ok(info(row)),
            UpdateOutcome::NotFound => Err(SavedError::NotFound { id: PermissionSavedID(id.to_string()) }),
            UpdateOutcome::Conflict => Err(SavedError::Conflict {
                id: PermissionSavedID(id.to_string()),
                action: action.to_string(),
                resource: resource.to_string(),
            }),
        }
    }

    pub fn remove(&self, id: &str) -> Result<(), StorageError> {
        self.db.read(|connection| permission::remove(connection, id))
    }
}

fn info(row: PermissionRow) -> PermissionSavedInfo {
    PermissionSavedInfo {
        id: PermissionSavedID(row.id),
        project_id: ProjectID(row.project_id),
        action: row.action,
        resource: row.resource,
        time: PermissionSavedInfoTime { created: row.time_created as f64, updated: row.time_updated as f64 },
    }
}
