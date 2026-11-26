use std::process::Stdio;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_storage::Database;
use opencode_storage::project::{self, ProjectRow};
use opencode_storage::worktree as storage_worktree;
use opencode_util::path::Flavor;
use rusqlite::TransactionBehavior;
use serde_json::Value;

use crate::worktree_strategies::{
    StrategyCreateInput, StrategyRemoveInput, WorktreeStrategies, WorktreeStrategiesData, WorktreeStrategy,
    canonical_directory, create_slug,
};

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("Project not found: {0}")]
    ProjectNotFound(String),
    #[error("Source directory not found for project {project_id}: {directory:?}")]
    SourceDirectoryNotFound { project_id: String, directory: Option<String> },
    #[error("Destination already exists: {directory}")]
    DestinationExists { directory: String },
    #[error("Directory unavailable: {directory}")]
    DirectoryUnavailable { directory: String },
    #[error("Invalid worktree directory: {directory}")]
    InvalidDirectory { directory: String },
    #[error("Worktree strategy is unavailable: {strategy}")]
    StrategyUnavailable { strategy: String },
    #[error("{message}")]
    Operation { message: String, force_required: bool },
    #[error(transparent)]
    Git(#[from] opencode_git::WorktreeError),
    #[error(transparent)]
    Storage(#[from] opencode_storage::StorageError),
    #[error("{0}")]
    Message(String),
}

impl WorktreeError {
    pub fn force_required(&self) -> Option<bool> {
        match self {
            WorktreeError::Git(error) => Some(error.force_required),
            WorktreeError::Operation { force_required, .. } => Some(*force_required),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeDirectory {
    pub directory: String,
    pub strategy: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub directory: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateInput {
    pub project_id: String,
    pub from: Option<String>,
    pub branch: Option<String>,
    pub directory: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveInput {
    pub project_id: String,
    pub directory: String,
    pub force: bool,
}

pub type StrategiesLoader =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<Arc<WorktreeStrategies>, WorktreeError>> + Send + Sync>;

pub struct Worktree {
    database: Arc<Database>,
    bus: Arc<Bus>,
    load: StrategiesLoader,
}

impl Worktree {
    pub fn new(database: Arc<Database>, bus: Arc<Bus>, load: StrategiesLoader) -> Self {
        Worktree { database, bus, load }
    }

    pub async fn list(&self, project_id: &str) -> Result<Vec<WorktreeDirectory>, WorktreeError> {
        self.project(project_id).await?;
        let rows = self.database.read(|connection| storage_worktree::list(connection, project_id))?;
        Ok(rows.into_iter().map(|row| WorktreeDirectory { directory: row.directory, strategy: row.strategy }).collect())
    }

    pub async fn create(&self, input: CreateInput) -> Result<WorktreeInfo, WorktreeError> {
        let project = self.project(&input.project_id).await?;
        let strategies = self.strategies(&project.worktree).await?;
        let settings = strategies.get().await.map_err(state_failure)?;
        let selected = find_strategy(&settings, &settings.selected)?;
        let parent = input.directory.clone().unwrap_or_else(|| settings.directory.clone());
        let source = input.from.clone().unwrap_or_else(|| project.worktree.clone());
        let source_directory = canonical_directory(&source)?;
        let tracked = self
            .database
            .read(|connection| storage_worktree::find(connection, &input.project_id, &source_directory))?;
        if tracked.is_none() {
            return Err(WorktreeError::SourceDirectoryNotFound {
                project_id: input.project_id.clone(),
                directory: Some(source_directory.clone()),
            });
        }
        tokio::fs::create_dir_all(&parent).await.map_err(process_failure)?;
        let name = input.name.clone().unwrap_or_else(create_slug);
        let target = destination(&parent, &name).await?;
        let created = selected
            .create(StrategyCreateInput {
                source_directory: &source_directory,
                directory: &target,
                branch: input.branch.as_deref(),
            })
            .await
            .map_err(|error| operation_error(selected.id(), "create", error))?;
        let directory = canonical_directory(&created)?;
        let changed = self.database.read(|connection| {
            storage_worktree::upsert(connection, &input.project_id, &directory, Some(selected.id()), true)
        })?;
        if changed {
            self.publish(&input.project_id);
        }
        if let Some(command) = project.commands.as_ref().and_then(start_command) {
            run_setup(&command, &directory, &source_directory).await?;
        }
        Ok(WorktreeInfo { directory })
    }

    pub async fn remove(&self, input: RemoveInput) -> Result<(), WorktreeError> {
        let project = self.project(&input.project_id).await?;
        let directory = canonical_directory(&input.directory)?;
        let stored =
            self.database.read(|connection| storage_worktree::find(connection, &input.project_id, &directory))?;
        let Some(stored) = stored else {
            return Err(WorktreeError::InvalidDirectory { directory });
        };
        let Some(strategy_id) = stored.strategy else {
            return Err(WorktreeError::InvalidDirectory { directory });
        };
        let strategies = self.strategies(&project.worktree).await?;
        let settings = strategies.get().await.map_err(state_failure)?;
        let strategy = find_strategy(&settings, &strategy_id)?;
        strategy
            .remove(StrategyRemoveInput { directory: &directory, force: input.force })
            .await
            .map_err(|error| operation_error(strategy.id(), "remove", error))?;
        let removed =
            self.database.read(|connection| storage_worktree::delete(connection, &input.project_id, &directory))?;
        if removed {
            self.publish(&input.project_id);
        }
        Ok(())
    }

    pub async fn refresh(&self, project_id: &str) -> Result<(), WorktreeError> {
        let project = self.project(project_id).await?;
        let strategies = self.strategies(&project.worktree).await?;
        let settings = strategies.get().await.map_err(state_failure)?;
        let stored = self.database.read(|connection| storage_worktree::list(connection, project_id))?;
        let mut checked = Vec::with_capacity(stored.len());
        for item in stored {
            let exists = is_directory(&item.directory).await;
            checked.push((item, exists));
        }
        let mut roots: Vec<&str> = vec![&project.worktree];
        for (item, exists) in &checked {
            if *exists && item.strategy.is_none() && !roots.contains(&item.directory.as_str()) {
                roots.push(&item.directory);
            }
        }
        let ordered: Vec<&(String, Arc<dyn WorktreeStrategy>)> = settings.strategies.iter().rev().collect();
        let mut discovered: Vec<(String, Option<String>)> = Vec::new();
        for root in roots {
            if !is_directory(root).await {
                continue;
            }
            for entry in &ordered {
                let (id, strategy) = *entry;
                let entries = match strategy.list(root).await {
                    Ok(entries) => entries,
                    Err(error) => {
                        log::warn!("worktree discovery failed directory={root} strategy={id}: {error}");
                        Vec::new()
                    }
                };
                for entry in entries {
                    let Ok(directory) = canonical_directory(&entry.directory) else {
                        continue;
                    };
                    if discovered.iter().any(|(existing, _)| existing == &directory) {
                        continue;
                    }
                    let strategy =
                        (entry.kind == crate::worktree_strategies::WorktreeEntryKind::Worktree).then(|| id.clone());
                    discovered.push((directory, strategy));
                }
            }
        }
        let removed: Vec<String> =
            checked.iter().filter(|(_, exists)| !exists).map(|(item, _)| item.directory.clone()).collect();
        let (updated, deleted) = self.database.transaction(TransactionBehavior::Immediate, |transaction| {
            let mut updated = Vec::new();
            for (directory, strategy) in &discovered {
                if storage_worktree::upsert(transaction, project_id, directory, strategy.as_deref(), false)? {
                    updated.push(directory.clone());
                }
            }
            let mut deleted = Vec::new();
            for directory in &removed {
                if storage_worktree::delete(transaction, project_id, directory)? {
                    deleted.push(directory.clone());
                }
            }
            Ok::<(Vec<String>, Vec<String>), opencode_storage::StorageError>((updated, deleted))
        })?;
        if !updated.is_empty() || !deleted.is_empty() {
            self.publish(project_id);
        }
        Ok(())
    }

    async fn project(&self, project_id: &str) -> Result<ProjectRow, WorktreeError> {
        self.database
            .read(|connection| project::get(connection, project_id))?
            .ok_or_else(|| WorktreeError::ProjectNotFound(project_id.to_owned()))
    }

    async fn strategies(&self, directory: &str) -> Result<Arc<WorktreeStrategies>, WorktreeError> {
        if !is_directory(directory).await {
            return Err(WorktreeError::DirectoryUnavailable { directory: directory.to_owned() });
        }
        (self.load)(directory.to_owned()).await
    }

    fn publish(&self, project_id: &str) {
        let data = serde_json::json!({ "projectID": project_id });
        if let Err(error) =
            self.bus.publish(&Definition::ephemeral("worktree.updated"), data, PublishOptions::default())
        {
            log::warn!("failed to publish the worktree update: {error}");
        }
    }
}

fn find_strategy<'a>(
    settings: &'a WorktreeStrategiesData,
    id: &str,
) -> Result<&'a Arc<dyn WorktreeStrategy>, WorktreeError> {
    settings
        .strategies
        .iter()
        .find(|(candidate, _)| candidate == id)
        .map(|(_, strategy)| strategy)
        .ok_or_else(|| WorktreeError::StrategyUnavailable { strategy: id.to_owned() })
}

fn state_failure(error: opencode_location::state::StateError) -> WorktreeError {
    WorktreeError::Message(error.to_string())
}

fn operation_error(strategy: &str, operation: &str, error: WorktreeError) -> WorktreeError {
    match error {
        error @ (WorktreeError::Git(_)
        | WorktreeError::DirectoryUnavailable { .. }
        | WorktreeError::Operation { .. }) => error,
        error => WorktreeError::Operation {
            message: format!("Worktree strategy {strategy} failed to {operation}: {error}"),
            force_required: false,
        },
    }
}

fn start_command(commands: &Value) -> Option<String> {
    let command = commands.get("start").and_then(Value::as_str)?.trim();
    (!command.is_empty()).then(|| command.to_owned())
}

async fn destination(parent: &str, name: &str) -> Result<String, WorktreeError> {
    let mut suffix = 1usize;
    let mut target = Flavor::NATIVE.join(&[parent, name]);
    while tokio::fs::metadata(&target).await.is_ok() {
        suffix += 1;
        if suffix > 10 {
            return Err(WorktreeError::DestinationExists { directory: target });
        }
        target = Flavor::NATIVE.join(&[parent, &format!("{name}-{suffix}")]);
    }
    Ok(target)
}

async fn is_directory(path: &str) -> bool {
    tokio::fs::metadata(path).await.is_ok_and(|metadata| metadata.is_dir())
}

fn process_failure(error: std::io::Error) -> WorktreeError {
    WorktreeError::Operation { message: error.to_string(), force_required: false }
}

async fn run_setup(command: &str, directory: &str, source_directory: &str) -> Result<(), WorktreeError> {
    #[cfg(windows)]
    let mut process = {
        let mut process = tokio::process::Command::new("cmd");
        process.args(["/C", command]);
        process
    };
    #[cfg(not(windows))]
    let mut process = {
        let mut process = tokio::process::Command::new("bash");
        process.args(["-lc", command]);
        process
    };
    let output = process
        .current_dir(directory)
        .env("OPENCODE_WORKTREE_BASE", source_directory)
        .env("OPENCODE_WORKTREE_PATH", directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(process_failure)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let code = output.status.code().map_or_else(|| "terminated".to_owned(), |code| code.to_string());
    Err(WorktreeError::Operation {
        message: format!("Worktree setup command exited with {code}: {detail}"),
        force_required: false,
    })
}
