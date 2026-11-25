use std::path::Path;

use crate::repo::Repository;
use crate::{Git, Operation, OperationError, RunOptions};

impl Git {
    async fn operation(&self, operation: Operation, directory: &str, args: &[&str]) -> Result<(), OperationError> {
        let output = self
            .execute(args, RunOptions { cwd: Some(directory.to_string()), ..Default::default() })
            .await
            .map_err(|cause| OperationError {
                operation,
                message: cause.message.clone(),
                directory: Some(directory.to_string()),
                cause: Some(cause.message),
            })?;
        if output.status == 0 {
            return Ok(());
        }
        let stderr = output.stderr_text();
        let stdout = output.text();
        let message = if stderr.trim().is_empty() {
            if stdout.trim().is_empty() { format!("Git {operation} failed") } else { stdout.trim().to_string() }
        } else {
            stderr.trim().to_string()
        };
        Err(OperationError { operation, message, directory: Some(directory.to_string()), cause: None })
    }

    pub async fn clone_repository(
        &self,
        remote: &str,
        directory: &str,
        branch: Option<&str>,
        depth: Option<u32>,
    ) -> Result<Repository, OperationError> {
        let depth = depth.unwrap_or(100).to_string();
        let mut args = vec!["clone", "--depth", depth.as_str()];
        if let Some(branch) = branch {
            args.extend(["--branch", branch]);
        }
        args.extend(["--", remote, directory]);
        let parent = Path::new(directory)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|parent| !parent.is_empty())
            .unwrap_or_else(|| ".".to_string());
        self.operation(Operation::Clone, &parent, &args).await?;
        if let Some(repository) = self.discover(directory).await {
            return Ok(repository);
        }
        Err(OperationError {
            operation: Operation::Clone,
            message: "Cloned repository could not be opened".to_string(),
            directory: Some(directory.to_string()),
            cause: None,
        })
    }

    pub async fn fetch_remotes(&self, repository: &Repository, prune: bool) -> Result<(), OperationError> {
        let mut args = vec!["fetch", "--all"];
        if prune {
            args.push("--prune");
        }
        self.operation(Operation::Fetch, &repository.worktree, &args).await
    }

    pub async fn fetch_branch(
        &self,
        repository: &Repository,
        remote: &str,
        branch: &str,
        force: bool,
    ) -> Result<(), OperationError> {
        let spec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        let spec = if force { format!("+{spec}") } else { spec };
        self.operation(Operation::Fetch, &repository.worktree, &["fetch", "--end-of-options", remote, &spec]).await
    }

    pub async fn checkout_remote_branch(
        &self,
        repository: &Repository,
        remote: &str,
        branch: &str,
        reset: bool,
    ) -> Result<(), OperationError> {
        let reference = format!("{remote}/{branch}");
        if remote.starts_with('-') || branch.starts_with('-') {
            return Err(OperationError {
                operation: Operation::Checkout,
                message: format!("Invalid checkout reference: {reference}"),
                directory: Some(repository.worktree.clone()),
                cause: None,
            });
        }
        let args = if reset { vec!["checkout", "-B", branch, &reference] } else { vec!["checkout", branch] };
        self.operation(Operation::Checkout, &repository.worktree, &args).await
    }

    pub async fn reset_hard(&self, repository: &Repository, revision: &str) -> Result<(), OperationError> {
        self.operation(Operation::Reset, &repository.worktree, &["reset", "--hard", revision]).await
    }
}
