use crate::repo::Repository;
use crate::{Git, RunOptions, WorktreeError, WorktreeOperation};

impl Git {
    async fn worktree_run(
        &self,
        operation: WorktreeOperation,
        cwd: &str,
        args: &[&str],
        directory: &str,
    ) -> Result<(), WorktreeError> {
        let output = self
            .execute(args, RunOptions { cwd: Some(cwd.to_string()), ..Default::default() })
            .await
            .map_err(|cause| WorktreeError {
                operation,
                message: cause.message.clone(),
                directory: Some(directory.to_string()),
                force_required: false,
                cause: Some(cause.message),
            })?;
        if output.status == 0 {
            return Ok(());
        }
        Err(WorktreeError::failed(operation, Some(directory.to_string()), &output))
    }

    pub async fn worktree_create(
        &self,
        repository: &Repository,
        directory: &str,
        reference: Option<&str>,
    ) -> Result<Repository, WorktreeError> {
        let reference = reference.unwrap_or("HEAD");
        self.worktree_run(
            WorktreeOperation::Create,
            &repository.worktree,
            &["worktree", "add", "--detach", "--", directory, reference],
            directory,
        )
        .await?;
        if let Some(created) = self.discover(directory).await {
            return Ok(created);
        }
        Err(WorktreeError {
            operation: WorktreeOperation::Create,
            message: "Created worktree could not be opened".to_string(),
            directory: Some(directory.to_string()),
            force_required: false,
            cause: None,
        })
    }

    pub async fn worktree_remove(
        &self,
        repository: &Repository,
        directory: &str,
        force: bool,
    ) -> Result<(), WorktreeError> {
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(directory);
        self.worktree_run(WorktreeOperation::Remove, &repository.common_directory, &args, directory).await
    }
}
