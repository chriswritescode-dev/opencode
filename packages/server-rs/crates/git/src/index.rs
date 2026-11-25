use std::collections::HashSet;

use crate::error::{Operation, OperationError};
use crate::repo::Repository;
use crate::store::join;
use crate::{Git, RunOptions};

pub(crate) struct Changes {
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
}

impl Git {
    pub async fn index_refresh(
        &self,
        repository: &Repository,
        scope: &str,
        ignores: Option<&Repository>,
        maximum_untracked_file_bytes: Option<u64>,
    ) -> Result<Vec<String>, OperationError> {
        let changes = self.list_changes(repository, scope, None, None).await?;
        self.update_index(repository, &changes, ignores, maximum_untracked_file_bytes, None, None).await
    }

    pub async fn index_ignored(
        &self,
        repository: &Repository,
        paths: &[String],
    ) -> Result<HashSet<String>, OperationError> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let output = self
            .repository_run(
                Operation::ListFiles,
                repository,
                &["check-ignore", "--no-index", "--stdin", "-z"],
                RunOptions { stdin: Some(stdin_records(paths)), ..Default::default() },
            )
            .await?;
        if output.status != 0 && output.status != 1 {
            let stderr = output.stderr_text();
            let message = if stderr.trim().is_empty() {
                "Failed to check ignored paths".to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(OperationError {
                operation: Operation::ListFiles,
                message,
                directory: Some(repository.worktree.clone()),
                cause: None,
            });
        }
        Ok(nuls(&output.text()).into_iter().collect())
    }

    pub(crate) async fn list_changes(
        &self,
        repository: &Repository,
        scope: &str,
        index: Option<&str>,
        exclude_from: Option<&str>,
    ) -> Result<Changes, OperationError> {
        let scope = literal_pathspec(scope);
        let options = || RunOptions { index: index.map(str::to_string), ..Default::default() };
        let tracked = async {
            let output = self
                .repository_operation(
                    Operation::Refresh,
                    repository,
                    &["diff-files", "--name-only", "-z", "--", &scope],
                    options(),
                )
                .await?;
            Ok::<_, OperationError>(records(&output.text()))
        };
        let mut others = vec!["ls-files", "--others"];
        let exclude = exclude_from.map(|file| format!("--exclude-from={file}"));
        if let Some(exclude) = exclude.as_deref() {
            others.push(exclude);
        }
        others.extend(["--exclude-standard", "-z", "--", &scope]);
        let untracked = async {
            let output = self.repository_operation(Operation::Refresh, repository, &others, options()).await?;
            Ok::<_, OperationError>(records(&output.text()))
        };
        let (tracked, untracked) = tokio::join!(tracked, untracked);
        Ok(Changes { tracked: tracked?, untracked: untracked? })
    }

    pub(crate) async fn update_index(
        &self,
        repository: &Repository,
        changes: &Changes,
        ignores: Option<&Repository>,
        maximum_untracked_file_bytes: Option<u64>,
        index: Option<&str>,
        excluded: Option<&HashSet<String>>,
    ) -> Result<Vec<String>, OperationError> {
        let candidates = changes.tracked.iter().chain(&changes.untracked).cloned().collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let excluded = match excluded {
            Some(excluded) => excluded.clone(),
            None => match ignores {
                Some(ignores) => self.index_ignored(ignores, &candidates).await?,
                None => HashSet::new(),
            },
        };
        let mut skipped = Vec::new();
        if let Some(maximum) = maximum_untracked_file_bytes.filter(|maximum| *maximum > 0) {
            for item in &changes.untracked {
                if excluded.contains(item) {
                    continue;
                }
                let Ok(metadata) = tokio::fs::metadata(join(&repository.worktree, item)).await else { continue };
                if metadata.is_file() && metadata.len() > maximum {
                    skipped.push(item.clone());
                }
            }
        }
        let skip = skipped.iter().map(String::as_str).collect::<HashSet<_>>();
        let added = candidates
            .iter()
            .filter(|item| !excluded.contains(*item) && !skip.contains(item.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let removed = changes.tracked.iter().filter(|item| excluded.contains(*item)).cloned().collect::<Vec<_>>();
        if !removed.is_empty() {
            self.repository_operation(
                Operation::Refresh,
                repository,
                &["update-index", "--force-remove", "-z", "--stdin"],
                RunOptions {
                    stdin: Some(stdin_records(&removed)),
                    index: index.map(str::to_string),
                    ..Default::default()
                },
            )
            .await?;
        }
        let mut embedded = Vec::new();
        for item in changes.tracked.iter().filter(|item| !excluded.contains(*item)) {
            if tokio::fs::metadata(join(&join(&repository.worktree, item), ".git")).await.is_ok() {
                embedded.push(item.clone());
            }
        }
        if !added.is_empty() {
            let mut records = added;
            records.extend(embedded);
            self.repository_operation_retry(
                Operation::Refresh,
                repository,
                &["update-index", "--add", "--remove", "--replace", "-z", "--stdin"],
                RunOptions {
                    stdin: Some(stdin_records(&records)),
                    index: index.map(str::to_string),
                    ..Default::default()
                },
            )
            .await?;
        }
        Ok(skipped)
    }
}

pub(crate) fn nuls(text: &str) -> Vec<String> {
    text.split('\0').filter(|record| !record.is_empty()).map(str::to_string).collect()
}

pub(crate) fn literal_pathspec(file: &str) -> String {
    format!(":(literal){file}")
}

fn records(text: &str) -> Vec<String> {
    nuls(text)
        .into_iter()
        .map(|record| match record.strip_suffix('/') {
            Some(trimmed) => trimmed.to_string(),
            None => record,
        })
        .collect()
}

fn stdin_records(records: &[String]) -> Vec<u8> {
    format!("{}\0", records.join("\0")).into_bytes()
}
