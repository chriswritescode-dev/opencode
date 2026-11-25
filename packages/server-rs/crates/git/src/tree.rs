use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::watch;

use crate::error::{Operation, OperationError};
use crate::index::{literal_pathspec, nuls};
use crate::repo::Repository;
use crate::store::join;
use crate::{Git, RunOptions};

#[derive(Clone, Debug)]
pub struct CaptureInput {
    pub repository: Repository,
    pub scopes: Vec<String>,
    pub ignores: Option<Repository>,
    pub maximum_untracked_file_bytes: Option<u64>,
}

#[derive(Default)]
pub(crate) struct State {
    last: Mutex<HashMap<String, LastCapture>>,
    prepared: Mutex<HashSet<String>>,
    pending: Mutex<HashMap<CaptureKey, Arc<PendingCapture>>>,
    oversized: Mutex<HashMap<String, HashMap<String, String>>>,
}

type CaptureKey = (String, String, Vec<String>, Option<String>, Option<u64>);

#[derive(Clone)]
struct LastCapture {
    fingerprint: String,
    tree: String,
}

struct PendingCapture {
    result: watch::Receiver<Option<Result<String, OperationError>>>,
}

struct PendingGuard<'a> {
    pending: &'a Mutex<HashMap<CaptureKey, Arc<PendingCapture>>>,
    key: CaptureKey,
    capture: Arc<PendingCapture>,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        if pending.get(&self.key).is_some_and(|entry| Arc::ptr_eq(entry, &self.capture)) {
            pending.remove(&self.key);
        }
    }
}

impl Git {
    pub async fn tree_write(&self, repository: &Repository, index: Option<&str>) -> Result<String, OperationError> {
        let output = self
            .repository_operation_retry(
                Operation::WriteTree,
                repository,
                &["write-tree"],
                RunOptions { index: index.map(str::to_string), ..Default::default() },
            )
            .await?;
        let tree = output.text().trim().to_string();
        if (40..=64).contains(&tree.len())
            && tree.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(tree);
        }
        Err(OperationError {
            operation: Operation::WriteTree,
            directory: Some(repository.worktree.clone()),
            message: format!("Invalid tree ID: {tree}"),
            cause: None,
        })
    }

    pub async fn tree_files(
        &self,
        repository: &Repository,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, OperationError> {
        let output = self
            .repository_operation(
                Operation::ListFiles,
                repository,
                &["diff", "--name-only", "--no-renames", "-z", from, to],
                RunOptions::default(),
            )
            .await?;
        Ok(nuls(&output.text()))
    }

    pub async fn tree_capture(&self, input: CaptureInput) -> Result<String, OperationError> {
        let key = (
            input.repository.git_directory.clone(),
            input.repository.worktree.clone(),
            input.scopes.clone(),
            input.ignores.as_ref().map(|ignores| ignores.git_directory.clone()),
            input.maximum_untracked_file_bytes,
        );
        loop {
            let existing = { self.tree.pending.lock().unwrap_or_else(|error| error.into_inner()).get(&key).cloned() };
            if let Some(existing) = existing {
                let mut receiver = existing.result.clone();
                loop {
                    if let Some(result) = receiver.borrow_and_update().clone() {
                        return result;
                    }
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
                continue;
            }
            let (sender, receiver) = watch::channel::<Option<Result<String, OperationError>>>(None);
            let capture = Arc::new(PendingCapture { result: receiver });
            {
                let mut pending = self.tree.pending.lock().unwrap_or_else(|error| error.into_inner());
                if pending.contains_key(&key) {
                    continue;
                }
                pending.insert(key.clone(), capture.clone());
            }
            let _guard = PendingGuard { pending: &self.tree.pending, key: key.clone(), capture: capture.clone() };
            let result = self.capture_locked(&key, &capture, &input).await;
            let _ = sender.send(Some(result.clone()));
            return result;
        }
    }

    async fn capture_locked(
        &self,
        key: &CaptureKey,
        pending: &Arc<PendingCapture>,
        input: &CaptureInput,
    ) -> Result<String, OperationError> {
        let directory = input.repository.git_directory.clone();
        self.with_lock(&directory, async {
            {
                let mut captures = self.tree.pending.lock().unwrap_or_else(|error| error.into_inner());
                if captures.get(key).is_some_and(|entry| Arc::ptr_eq(entry, pending)) {
                    captures.remove(key);
                }
            }
            let prepared = {
                let mut prepared = self.tree.prepared.lock().unwrap_or_else(|error| error.into_inner());
                prepared.insert(input.repository.git_directory.clone())
            };
            if prepared {
                self.sweep_temporary_indexes(&input.repository).await;
                self.ensure_snapshot_config(&input.repository).await;
            }
            match self.attempt_capture(input).await {
                Ok(tree) => Ok(tree),
                Err(error) => {
                    if !self.is_index_unreadable(&input.repository).await {
                        return Err(error);
                    }
                    log::warn!(
                        "rebuilding unreadable snapshot index: {} ({})",
                        error.message,
                        input.repository.git_directory
                    );
                    self.rebuild_index(&input.repository, input.ignores.as_ref()).await;
                    self.attempt_capture(input).await
                }
            }
        })
        .await
    }

    async fn attempt_capture(&self, input: &CaptureInput) -> Result<String, OperationError> {
        let exclude = match input.ignores.as_ref() {
            Some(ignores) => {
                let path = join(&ignores.common_directory, "info/exclude");
                tokio::fs::metadata(&path).await.is_ok().then_some(path)
            }
            None => None,
        };
        let repository = &input.repository;
        let (tree, installed) = self
            .with_temporary_index(repository, async |index| {
                let scanned = index_fingerprint(index).await;
                let mut changes = Vec::with_capacity(input.scopes.len());
                for scope in &input.scopes {
                    changes.push(self.list_changes(repository, scope, Some(index), exclude.as_deref()).await?);
                }
                let last = {
                    self.tree
                        .last
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&repository.git_directory)
                        .cloned()
                };
                let untracked = changes.iter().flat_map(|change| change.untracked.iter().cloned()).collect::<Vec<_>>();
                let oversized = {
                    self.tree
                        .oversized
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&repository.git_directory)
                        .cloned()
                };
                let mut still_oversized = HashSet::new();
                if let Some(oversized) = &oversized {
                    for file in untracked.iter().filter(|file| oversized.contains_key(*file)) {
                        if let Some(current) = oversized_stamp(input, file).await
                            && oversized.get(file) == Some(&current)
                        {
                            still_oversized.insert(file.clone());
                        }
                    }
                }
                let excluded = match (&scanned, &last) {
                    (Some(scanned), Some(last))
                        if last.fingerprint == *scanned && changes.iter().all(|change| change.tracked.is_empty()) =>
                    {
                        Some(match input.ignores.as_ref() {
                            Some(ignores) => {
                                let paths = untracked
                                    .iter()
                                    .filter(|file| !still_oversized.contains(*file))
                                    .cloned()
                                    .collect::<Vec<_>>();
                                self.index_ignored(ignores, &paths).await?
                            }
                            None => HashSet::new(),
                        })
                    }
                    _ => None,
                };
                if let (Some(last), Some(excluded)) = (&last, &excluded)
                    && untracked.iter().all(|file| excluded.contains(file) || still_oversized.contains(file))
                {
                    return Ok(last.tree.clone());
                }
                let mut skipped = Vec::new();
                for change in &changes {
                    let updated = self
                        .update_index(
                            repository,
                            change,
                            input.ignores.as_ref(),
                            input.maximum_untracked_file_bytes,
                            Some(index),
                            excluded.as_ref(),
                        )
                        .await?;
                    skipped.extend(updated);
                }
                let mut stamps = HashMap::new();
                for file in &skipped {
                    if let Some(stamp) = oversized_stamp(input, file).await {
                        stamps.insert(file.clone(), stamp);
                    }
                }
                {
                    self.tree
                        .oversized
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(repository.git_directory.clone(), stamps);
                }
                let tree = self.tree_write(repository, Some(index)).await?;
                Ok(tree)
            })
            .await?;
        if let Some(fingerprint) = installed {
            self.tree
                .last
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(repository.git_directory.clone(), LastCapture { fingerprint, tree: tree.clone() });
        } else {
            self.tree.last.lock().unwrap_or_else(|error| error.into_inner()).remove(&repository.git_directory);
        }
        Ok(tree)
    }

    async fn with_temporary_index<T>(
        &self,
        repository: &Repository,
        use_index: impl AsyncFnOnce(&str) -> Result<T, OperationError>,
    ) -> Result<(T, Option<String>), OperationError> {
        let current = Self::index_file(repository);
        let temporary = Self::temporary_index(repository);
        if tokio::fs::metadata(&current).await.is_ok() && tokio::fs::hard_link(&current, &temporary).await.is_err() {
            Git::copy_index(&current, &temporary).await.map_err(|error| OperationError {
                operation: Operation::Refresh,
                directory: Some(repository.git_directory.clone()),
                message: "Failed to prepare a temporary index".to_string(),
                cause: Some(error.to_string()),
            })?;
        }
        let value = match use_index(&temporary).await {
            Ok(value) => value,
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(error);
            }
        };
        let fingerprint = index_fingerprint(&temporary).await;
        let mut renamed = false;
        for attempt in 0..4 {
            if tokio::fs::rename(&temporary, &current).await.is_ok() {
                renamed = true;
                break;
            }
            if attempt < 3 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        Ok((value, if renamed { fingerprint } else { None }))
    }

    async fn is_index_unreadable(&self, repository: &Repository) -> bool {
        if tokio::fs::metadata(Self::index_file(repository)).await.is_err() {
            return false;
        }
        let probe = literal_pathspec(".opencode-probe");
        match self
            .repository_operation(
                Operation::Refresh,
                repository,
                &["ls-files", "-z", "--", &probe],
                RunOptions::default(),
            )
            .await
        {
            Ok(_) => false,
            Err(error) => error.cause.is_none(),
        }
    }

    async fn rebuild_index(&self, repository: &Repository, source: Option<&Repository>) {
        self.tree.last.lock().unwrap_or_else(|error| error.into_inner()).remove(&repository.git_directory);
        let temporary = Self::temporary_index(repository);
        let copied = match source {
            Some(source) => self.seed_index(repository, source, &temporary).await.is_ok(),
            None => false,
        };
        if copied {
            let _ = tokio::fs::rename(&temporary, Self::index_file(repository)).await;
        } else {
            let _ = tokio::fs::remove_file(Self::index_file(repository)).await;
        }
        let _ = tokio::fs::remove_file(&temporary).await;
    }
}

async fn index_fingerprint(path: &str) -> Option<String> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis();
    Some(format!("{}:{}:{mtime}", inode(&metadata), metadata.len()))
}

async fn oversized_stamp(input: &CaptureInput, file: &str) -> Option<String> {
    let maximum = input.maximum_untracked_file_bytes?;
    let metadata = tokio::fs::metadata(join(&input.repository.worktree, file)).await.ok()?;
    if !metadata.is_file() || metadata.len() <= maximum {
        return None;
    }
    let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis();
    Some(format!("{}:{mtime}", metadata.len()))
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(metadata)
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}
