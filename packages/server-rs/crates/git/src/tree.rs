use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::watch;

use crate::error::{Operation, OperationError};
use crate::index::{literal_pathspec, nuls};
use crate::patch::{MAX_TOTAL_PATCH_BYTES, Patch, chunks_by_file, empty_patch};
use crate::repo::Repository;
use crate::run::RunOutput;
use crate::store::join;
use crate::{Git, RunOptions};

const CHANGED_WHILE_INDEXING_ATTEMPTS: usize = 3;

#[derive(Clone, Debug)]
pub struct CaptureInput {
    pub repository: Repository,
    pub scopes: Vec<String>,
    pub ignores: Option<Repository>,
    pub maximum_untracked_file_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FileDiff {
    pub file: String,
    pub patch: String,
    pub additions: usize,
    pub deletions: usize,
    pub status: FileDiffStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileDiffStatus {
    Added,
    Deleted,
    Modified,
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

struct DiffGroup {
    diffs: Vec<FileDiff>,
    patch_bytes: usize,
    truncated: bool,
}

struct DiffStat {
    binary: bool,
    additions: usize,
    deletions: usize,
}

struct RestorePlan {
    tree: String,
    present: Vec<String>,
    absent: Vec<String>,
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

    pub async fn tree_diff(
        &self,
        repository: &Repository,
        from: &str,
        to: &str,
        context: Option<i32>,
        paths: Option<&[String]>,
    ) -> Result<Vec<FileDiff>, OperationError> {
        if paths.is_some_and(|paths| paths.is_empty()) {
            return Ok(Vec::new());
        }
        let groups = match paths {
            Some(paths) => path_groups(paths).into_iter().map(Some).collect::<Vec<_>>(),
            None => vec![None],
        };
        let mut diffs = Vec::new();
        let mut patch_budget = MAX_TOTAL_PATCH_BYTES;
        for paths in groups {
            let group = self.diff_group(repository, from, to, context, paths.as_deref(), patch_budget).await?;
            diffs.extend(group.diffs);
            patch_budget = if group.truncated { 0 } else { patch_budget.saturating_sub(group.patch_bytes) };
        }
        Ok(diffs)
    }

    async fn diff_group(
        &self,
        repository: &Repository,
        from: &str,
        to: &str,
        context: Option<i32>,
        paths: Option<&[String]>,
        patch_budget: usize,
    ) -> Result<DiffGroup, OperationError> {
        let mut args = vec!["--no-renames", from, to, "--"];
        if let Some(paths) = paths {
            args.extend(paths.iter().map(String::as_str));
        }
        let mut names_args = vec!["--literal-pathspecs", "diff", "--name-status", "-z"];
        names_args.extend(args.iter().copied());
        let mut numbers_args = vec!["--literal-pathspecs", "diff", "--numstat", "-z"];
        numbers_args.extend(args.iter().copied());
        let unified = format!("--unified={}", context.unwrap_or(3));
        let mut patch_args = vec!["--literal-pathspecs", "-c", "core.quotepath=false", "diff", "--no-ext-diff"];
        patch_args.push(unified.as_str());
        patch_args.extend(args.iter().copied());
        let names = self.repository_operation(Operation::Diff, repository, &names_args, RunOptions::default());
        let numbers = self.repository_operation(Operation::Diff, repository, &numbers_args, RunOptions::default());
        let patch = async {
            if patch_budget == 0 {
                return Ok::<RunOutput, OperationError>(RunOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    truncated: true,
                });
            }
            self.repository_operation(
                Operation::Diff,
                repository,
                &patch_args,
                RunOptions { max_output_bytes: Some(patch_budget), ..Default::default() },
            )
            .await
        };
        let (names, numbers, patch) = tokio::join!(names, numbers, patch);
        let names = names?.text();
        let numbers = numbers?.text();
        let patch = patch?;

        let statuses = nuls(&names);
        let mut files = Vec::with_capacity(statuses.len() / 2);
        for record in statuses.as_chunks::<2>().0 {
            if record[1].is_empty() {
                continue;
            }
            let status = if record[0].starts_with('A') {
                FileDiffStatus::Added
            } else if record[0].starts_with('D') {
                FileDiffStatus::Deleted
            } else {
                FileDiffStatus::Modified
            };
            files.push((record[1].clone(), status));
        }

        let mut stats: HashMap<String, DiffStat> = HashMap::new();
        for line in nuls(&numbers) {
            let mut fields = line.split('\t');
            let additions = fields.next().unwrap_or_default();
            let deletions = fields.next().unwrap_or_default();
            let file = fields.collect::<Vec<_>>().join("\t");
            if additions.is_empty() || deletions.is_empty() || file.is_empty() {
                continue;
            }
            let binary = additions == "-" || deletions == "-";
            stats.insert(
                file,
                DiffStat {
                    binary,
                    additions: if binary { 0 } else { additions.parse().unwrap_or(0) },
                    deletions: if binary { 0 } else { deletions.parse().unwrap_or(0) },
                },
            );
        }

        let patch = Patch { text: patch.text(), truncated: patch.truncated };
        let patches = chunks_by_file(&patch, |index| files.get(index).map(|(file, _)| file.clone()));
        let diffs = files
            .iter()
            .map(|(file, status)| {
                let stat = stats.get(file);
                FileDiff {
                    file: file.clone(),
                    patch: match stat {
                        Some(stat) if stat.binary => String::new(),
                        _ => patches.get(file).cloned().unwrap_or_else(|| empty_patch(file)),
                    },
                    additions: stat.map_or(0, |stat| stat.additions),
                    deletions: stat.map_or(0, |stat| stat.deletions),
                    status: *status,
                    more: None,
                }
            })
            .collect();
        Ok(DiffGroup { diffs, patch_bytes: patch.text.len(), truncated: patch.truncated })
    }

    pub async fn tree_restore(
        &self,
        repository: &Repository,
        files: &[(String, String)],
    ) -> Result<(), OperationError> {
        if files.is_empty() {
            return Ok(());
        }
        self.with_lock(&repository.git_directory, async {
            let paths = files.iter().map(|(file, _)| file.as_str()).collect::<Vec<_>>();
            if !can_batch_restore(&paths) {
                for (file, tree) in files {
                    if !self.tree_has_entry(repository, tree, file).await? {
                        remove_path(repository, file).await?;
                        continue;
                    }
                    let pathspec = literal_pathspec(file);
                    self.with_temporary_index(repository, async |index| {
                        self.repository_operation(
                            Operation::Restore,
                            repository,
                            &["checkout", tree, "--", &pathspec],
                            RunOptions { index: Some(index.to_string()), ..Default::default() },
                        )
                        .await
                    })
                    .await?;
                }
                return Ok(());
            }

            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for (file, tree) in files {
                match groups.iter_mut().find(|(existing, _)| existing == tree) {
                    Some((_, group)) => group.push(file.clone()),
                    None => groups.push((tree.clone(), vec![file.clone()])),
                }
            }
            let mut plan = Vec::with_capacity(groups.len());
            for (tree, group) in groups {
                let present = self.paths_in_tree(repository, &tree, &group).await?;
                plan.push(RestorePlan {
                    present: group.iter().filter(|file| present.contains(*file)).cloned().collect(),
                    absent: group.iter().filter(|file| !present.contains(*file)).cloned().collect(),
                    tree,
                });
            }
            let checkouts = plan.iter().filter(|item| !item.present.is_empty()).collect::<Vec<_>>();
            if !checkouts.is_empty() {
                self.with_temporary_index(repository, async |index| {
                    for item in &checkouts {
                        let pathspecs = item.present.iter().map(|file| literal_pathspec(file)).collect::<Vec<_>>();
                        self.repository_operation(
                            Operation::Restore,
                            repository,
                            &["checkout", item.tree.as_str(), "--pathspec-from-file=-", "--pathspec-file-nul"],
                            RunOptions {
                                stdin: Some(format!("{}\0", pathspecs.join("\0")).into_bytes()),
                                index: Some(index.to_string()),
                                ..Default::default()
                            },
                        )
                        .await?;
                    }
                    Ok(())
                })
                .await?;
            }
            for file in plan.iter().flat_map(|item| item.absent.iter()) {
                remove_path(repository, file).await?;
            }
            Ok(())
        })
        .await
    }

    async fn tree_has_entry(&self, repository: &Repository, tree: &str, file: &str) -> Result<bool, OperationError> {
        let pathspec = literal_pathspec(file);
        let output = self
            .repository_operation(
                Operation::Restore,
                repository,
                &["ls-tree", "-z", tree, "--", &pathspec],
                RunOptions::default(),
            )
            .await?;
        let mut text = output.text();
        if text.ends_with('\0') {
            text.pop();
        }
        if text.is_empty() {
            return Ok(false);
        }
        if tree_entry_parts(&text).is_none() {
            return Err(OperationError {
                operation: Operation::Restore,
                directory: Some(repository.worktree.clone()),
                message: format!("Invalid tree entry for {file}"),
                cause: None,
            });
        }
        Ok(true)
    }

    async fn paths_in_tree(
        &self,
        repository: &Repository,
        tree: &str,
        files: &[String],
    ) -> Result<HashSet<String>, OperationError> {
        let pathspecs = files.iter().map(|file| literal_pathspec(file)).collect::<Vec<_>>();
        let length = pathspecs.iter().map(|pathspec| pathspec.len() + 1).sum::<usize>();
        let mut args = Vec::with_capacity(pathspecs.len() + 6);
        args.push("ls-tree");
        if length <= argument_budget() {
            args.extend(["-z", tree, "--"]);
            args.extend(pathspecs.iter().map(String::as_str));
        } else {
            args.extend(["-r", "-t", "-z", "--full-tree", tree]);
        }
        let output = self.repository_operation(Operation::Restore, repository, &args, RunOptions::default()).await?;
        let mut present = HashSet::new();
        for record in nuls(&output.text()) {
            match tree_entry_parts(&record) {
                Some((_, _, _, path)) => {
                    present.insert(path.to_string());
                }
                None => {
                    return Err(OperationError {
                        operation: Operation::Restore,
                        directory: Some(repository.worktree.clone()),
                        message: format!("Invalid tree entry: {record}"),
                        cause: None,
                    });
                }
            }
        }
        Ok(present)
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
            match self.attempt_capture_settled(input).await {
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
                    self.attempt_capture_settled(input).await
                }
            }
        })
        .await
    }

    async fn attempt_capture_settled(&self, input: &CaptureInput) -> Result<String, OperationError> {
        let mut attempt = 1;
        loop {
            match Box::pin(self.attempt_capture(input)).await {
                Err(error) if attempt < CHANGED_WHILE_INDEXING_ATTEMPTS && changed_while_indexing(&error) => {
                    attempt += 1;
                }
                result => return result,
            }
        }
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

fn changed_while_indexing(error: &OperationError) -> bool {
    error.operation == Operation::Refresh
        && error.cause.is_none()
        && error.message.contains("short read while indexing")
}

fn argument_budget() -> usize {
    if cfg!(windows) { 24_000 } else { 128 * 1024 }
}

fn path_groups(paths: &[String]) -> Vec<Vec<String>> {
    let mut sorted = paths.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut length = 0usize;
    for file in sorted {
        let cost = file.len() + 3;
        match groups.last_mut() {
            Some(group) if length + cost <= argument_budget() => {
                group.push(file.clone());
                length += cost;
            }
            _ => {
                groups.push(vec![file.clone()]);
                length = cost;
            }
        }
    }
    groups
}

fn can_batch_restore(files: &[&str]) -> bool {
    let canonical = files.iter().all(|file| {
        file.split('/').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'\\' && byte != b':')
                && !part.ends_with(['.', ' '])
        })
    });
    if !canonical {
        return false;
    }
    let folded = files.iter().map(|file| file.to_lowercase()).collect::<HashSet<_>>();
    if folded.len() != files.len() {
        return false;
    }
    files.iter().all(|file| {
        let parts = file.to_lowercase().split('/').map(str::to_string).collect::<Vec<_>>();
        (1..parts.len()).all(|index| !folded.contains(&parts[..index].join("/")))
    })
}

fn tree_entry_parts(record: &str) -> Option<(&str, &str, &str, &str)> {
    let (header, path) = record.split_once('\t')?;
    let mut fields = header.split(' ');
    let mode = fields.next()?;
    let kind = fields.next()?;
    let oid = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    if mode.is_empty() || !mode.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if kind.is_empty() || !kind.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return None;
    }
    if oid.is_empty() || !oid.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return None;
    }
    Some((mode, kind, oid, path))
}

async fn remove_path(repository: &Repository, file: &str) -> Result<(), OperationError> {
    let path = join(&repository.worktree, file);
    let result = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(&path).await,
        Ok(_) => tokio::fs::remove_file(&path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OperationError {
            operation: Operation::Restore,
            directory: Some(repository.worktree.clone()),
            message: format!("Failed to remove {file}"),
            cause: Some(error.to_string()),
        }),
    }
}

async fn index_fingerprint(path: &str) -> Option<String> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis();
    Some(format!("{}:{}:{mtime}", inode(&metadata), metadata.len()))
}

async fn oversized_stamp(input: &CaptureInput, file: &str) -> Option<String> {
    let maximum = input.maximum_untracked_file_bytes.filter(|maximum| *maximum > 0)?;
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
