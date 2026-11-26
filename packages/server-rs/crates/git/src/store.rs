use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use tokio::io::AsyncWriteExt;

use crate::error::{Operation, OperationError};
use crate::repo::Repository;
use crate::{Git, RunOptions};

pub(crate) const SNAPSHOT_CONFIG_FILE: &str = "opencode.gitconfig";

const SNAPSHOT_CONFIG_INCLUDE: &str = "[include]\n\tpath = opencode.gitconfig\n";
const SNAPSHOT_CONFIG: &str = "[core]\n\tautocrlf = false\n\tlongpaths = true\n\tsymlinks = true\n\tfsmonitor = false\n\tuntrackedCache = true\n\t# A split index cannot name its shared file once manyFiles skips index checksums.\n\tsplitIndex = false\n[feature]\n\tmanyFiles = true\n[index]\n\tversion = 4\n\tthreads = true\n";
const TEMPORARY_INDEX_PREFIX: &str = "index.opencode-";
const TEMPORARY_INDEX_MAX_AGE_MILLIS: u128 = 60 * 60 * 1000;

impl Git {
    pub async fn create_repository(
        &self,
        worktree: &str,
        git_directory: &str,
        seed: Option<&Repository>,
    ) -> Result<Repository, OperationError> {
        let repository = Repository {
            worktree: worktree.to_string(),
            git_directory: git_directory.to_string(),
            common_directory: git_directory.to_string(),
        };
        if tokio::fs::metadata(join(git_directory, "HEAD")).await.is_ok() {
            self.init_repository(worktree, git_directory, git_directory, seed).await?;
            return Ok(repository);
        }
        let temporary = format!("{git_directory}.init-{}", unique_suffix());
        let result = async {
            self.init_repository(worktree, git_directory, &temporary, seed).await?;
            let error = match tokio::fs::rename(&temporary, git_directory).await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            if tokio::fs::metadata(join(git_directory, "HEAD")).await.is_ok() {
                return Ok(());
            }
            Err(OperationError {
                operation: Operation::Create,
                message: "Failed to move Git storage into place".to_string(),
                directory: Some(git_directory.to_string()),
                cause: Some(error.to_string()),
            })
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&temporary).await;
        result?;
        Ok(repository)
    }

    async fn init_repository(
        &self,
        worktree: &str,
        git_directory: &str,
        directory: &str,
        seed: Option<&Repository>,
    ) -> Result<(), OperationError> {
        tokio::fs::create_dir_all(directory).await.map_err(|error| OperationError {
            operation: Operation::Create,
            message: "Failed to create Git storage".to_string(),
            directory: Some(git_directory.to_string()),
            cause: Some(error.to_string()),
        })?;
        let store = Repository {
            worktree: worktree.to_string(),
            git_directory: directory.to_string(),
            common_directory: directory.to_string(),
        };
        self.repository_operation(Operation::Create, &store, &["init"], RunOptions::default()).await?;
        let configured = async {
            tokio::fs::write(join(directory, SNAPSHOT_CONFIG_FILE), SNAPSHOT_CONFIG).await?;
            let config = join(directory, "config");
            let current = tokio::fs::read_to_string(&config).await?;
            if current.contains(SNAPSHOT_CONFIG_INCLUDE) {
                return Ok::<(), std::io::Error>(());
            }
            let separator = if current.ends_with('\n') { "\n" } else { "\n\n" };
            let mut file = tokio::fs::OpenOptions::new().append(true).open(&config).await?;
            file.write_all(format!("{separator}{SNAPSHOT_CONFIG_INCLUDE}").as_bytes()).await?;
            Ok(())
        }
        .await;
        configured.map_err(|error| OperationError {
            operation: Operation::Create,
            message: "Failed to configure Git storage".to_string(),
            directory: Some(git_directory.to_string()),
            cause: Some(error.to_string()),
        })?;
        let Some(seed) = seed else { return Ok(()) };
        let shared = async {
            let info = join(directory, "objects/info");
            tokio::fs::create_dir_all(&info).await?;
            tokio::fs::write(join(&info, "alternates"), format!("{}\n", join(&seed.common_directory, "objects"))).await
        }
        .await;
        shared.map_err(|error| OperationError {
            operation: Operation::Create,
            message: "Failed to configure shared Git objects".to_string(),
            directory: Some(git_directory.to_string()),
            cause: Some(error.to_string()),
        })?;
        let index = join(directory, "index");
        if self.seed_index(&store, seed, &index).await.is_err() {
            let _ = tokio::fs::remove_file(&index).await;
        }
        Ok(())
    }

    pub(crate) async fn copy_index(from: &str, to: &str) -> std::io::Result<()> {
        let metadata = tokio::fs::metadata(from).await?;
        tokio::fs::copy(from, to).await?;
        let Some(modified) = metadata.modified().ok() else { return Ok(()) };
        let accessed = metadata.accessed().ok().unwrap_or(modified);
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(to) {
            let _ = file.set_times(std::fs::FileTimes::new().set_accessed(accessed).set_modified(modified));
        }
        Ok(())
    }

    pub(crate) async fn seed_index(
        &self,
        store: &Repository,
        source: &Repository,
        target: &str,
    ) -> Result<(), OperationError> {
        Self::copy_index(&join(&source.git_directory, "index"), target).await.map_err(|error| OperationError {
            operation: Operation::Refresh,
            message: error.to_string(),
            directory: Some(store.worktree.clone()),
            cause: Some(error.to_string()),
        })?;
        let Ok(mut entries) = tokio::fs::read_dir(&source.git_directory).await else { return Ok(()) };
        let mut shared = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("sharedindex.") {
                shared.push(name);
            }
        }
        if shared.is_empty() {
            return Ok(());
        }
        let result = async {
            for entry in &shared {
                let from = join(&source.git_directory, entry);
                let to = join(&store.git_directory, entry);
                if tokio::fs::hard_link(&from, &to).await.is_err() {
                    tokio::fs::copy(&from, &to).await.map_err(|error| OperationError {
                        operation: Operation::Refresh,
                        message: error.to_string(),
                        directory: Some(store.worktree.clone()),
                        cause: Some(error.to_string()),
                    })?;
                }
            }
            self.repository_operation(
                Operation::Refresh,
                store,
                &["update-index", "--no-split-index"],
                RunOptions { index: Some(target.to_string()), ..Default::default() },
            )
            .await
        }
        .await;
        for entry in &shared {
            let _ = tokio::fs::remove_file(join(&store.git_directory, entry)).await;
        }
        result.map(|_| ())
    }

    pub(crate) fn temporary_index(repository: &Repository) -> String {
        join(&repository.git_directory, &format!("{TEMPORARY_INDEX_PREFIX}{}-{}", now_millis(), unique_suffix()))
    }

    pub(crate) fn index_file(repository: &Repository) -> String {
        join(&repository.git_directory, "index")
    }

    pub(crate) async fn sweep_temporary_indexes(&self, repository: &Repository) {
        let Ok(mut entries) = tokio::fs::read_dir(&repository.git_directory).await else { return };
        let cutoff = now_millis().saturating_sub(TEMPORARY_INDEX_MAX_AGE_MILLIS);
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix(TEMPORARY_INDEX_PREFIX) else { continue };
            let Some(created) = rest.split('-').next().and_then(|value| value.parse::<u128>().ok()) else { continue };
            if created < cutoff {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }

    pub(crate) async fn ensure_snapshot_config(&self, repository: &Repository) {
        let file = join(&repository.git_directory, SNAPSHOT_CONFIG_FILE);
        let Ok(current) = tokio::fs::read_to_string(&file).await else { return };
        if current == SNAPSHOT_CONFIG {
            return;
        }
        let temporary = format!("{file}.{}", unique_suffix());
        let _ = async {
            tokio::fs::write(&temporary, SNAPSHOT_CONFIG).await?;
            tokio::fs::rename(&temporary, &file).await
        }
        .await;
        let _ = tokio::fs::remove_file(&temporary).await;
    }
}

pub(crate) fn join(base: &str, path: &str) -> String {
    Path::new(base).join(path).to_string_lossy().into_owned()
}

pub(crate) fn now_millis() -> u128 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis()).unwrap_or_default()
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos =
        std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or_default();
    format!("{}-{nanos}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}
