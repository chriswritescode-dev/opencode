use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::BoxFuture;
use opencode_git::{Git, Repository};
use opencode_local::lock;
use opencode_storage::Database;
use opencode_util::path::realpath;
use opencode_util::time::now_ms;
use serde_json::{Value, json};

use crate::repository::{self, Reference, RemoteReference, Vars};

const REFRESH_INTERVAL_MS: f64 = 86_400_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Daily,
    Force,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Cached,
    Cloned,
    Refreshed,
}

#[derive(Clone, Debug)]
pub struct EnsureInput {
    pub reference: RemoteReference,
    pub refresh: Option<Refresh>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnsureResult {
    pub repository: String,
    pub host: String,
    pub remote: String,
    pub local_path: String,
    pub status: Status,
    pub head: Option<String>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    InvalidBranch { branch: String, message: String },
    #[error("{message}")]
    CloneFailed { repository: String, message: String },
    #[error("{message}")]
    FetchFailed { repository: String, message: String },
    #[error("{message}")]
    CheckoutFailed { repository: String, branch: String, message: String },
    #[error("{message}")]
    ResetFailed { repository: String, message: String },
    #[error("{message}")]
    LockFailed { local_path: String, message: String },
    #[error("{message}")]
    CacheOperation { operation: String, path: String, message: String },
}

pub trait Materializer: Send + Sync {
    fn ensure(&self, input: EnsureInput) -> BoxFuture<'_, Result<EnsureResult, Error>>;
}

pub struct RepositoryCache {
    git: Arc<Git>,
    database: Arc<Database>,
    repos: PathBuf,
    locks: PathBuf,
    vars: Vars,
}

impl RepositoryCache {
    pub fn new(git: Arc<Git>, database: Arc<Database>, repos: PathBuf, locks: PathBuf, vars: Vars) -> Self {
        RepositoryCache { git, database, repos, locks, vars }
    }

    async fn ensure_locked(
        &self,
        input: &EnsureInput,
        repository: &str,
        local_path: &str,
        clone_target: &Reference,
    ) -> Result<EnsureResult, Error> {
        if let Some(parent) = Path::new(local_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| cache_operation("ensure cache directory", local_path, error))?;
        }

        let existing = self.git.discover(local_path).await;
        let origin = match &existing {
            Some(existing) => self.git.remote_get(existing).await.ok().flatten(),
            None => None,
        };
        let origin_reference = origin.as_deref().and_then(|origin| repository::parse(origin, &self.vars));
        let worktree = existing.as_ref().and_then(|_| realpath(Path::new(local_path)));
        let reuse = match (&existing, &worktree, &origin_reference) {
            (Some(existing), Some(worktree), Some(origin_reference)) => {
                existing.worktree == worktree.to_string_lossy() && repository::same(origin_reference, clone_target)
            }
            _ => false,
        };

        if !reuse && let Ok(metadata) = tokio::fs::metadata(local_path).await {
            let removed = if metadata.is_dir() {
                tokio::fs::remove_dir_all(local_path).await
            } else {
                tokio::fs::remove_file(local_path).await
            };
            removed.map_err(|error| cache_operation("remove stale cache", local_path, error))?;
        }

        let now = now_ms();
        let key = format!("repository-cache:{local_path}");
        let previous = match self.database.read(|connection| opencode_storage::kv::get(connection, &key)) {
            Ok(value) => value.and_then(|value| value.get("attemptedAt").and_then(Value::as_f64)),
            Err(error) => {
                log::warn!("repository cache read failed for {key}: {error}");
                None
            }
        };
        let refresh = match input.refresh {
            Some(Refresh::Daily) => previous.is_none_or(|previous| now as f64 - previous >= REFRESH_INTERVAL_MS),
            Some(Refresh::Force) => true,
            None => false,
        };
        let status = if !reuse {
            Status::Cloned
        } else if refresh {
            Status::Refreshed
        } else {
            Status::Cached
        };

        if status != Status::Cached {
            self.record_attempt(&key, now);

            match status {
                Status::Cloned => {
                    let staging = Staging::new(local_path);
                    self.git
                        .clone_repository(&input.reference.remote, &staging.path, input.branch.as_deref(), None)
                        .await
                        .map_err(|error| Error::CloneFailed {
                            repository: repository.to_owned(),
                            message: error.message,
                        })?;
                    tokio::fs::rename(&staging.path, local_path)
                        .await
                        .map_err(|error| cache_operation("move cloned cache", local_path, error))?;
                    staging.keep();
                }
                Status::Refreshed => {
                    self.refresh(existing.as_ref(), input, repository).await?;
                }
                Status::Cached => {}
            }
        }

        let checkout = self.git.discover(local_path).await;
        let head = match &checkout {
            Some(checkout) => self.git.head(checkout).await,
            None => None,
        };
        let branch = match &checkout {
            Some(checkout) => self.git.branch(checkout).await,
            None => None,
        };

        Ok(EnsureResult {
            repository: repository.to_owned(),
            host: input.reference.host.clone(),
            remote: input.reference.remote.clone(),
            local_path: local_path.to_owned(),
            status,
            head,
            branch,
        })
    }

    async fn refresh(&self, existing: Option<&Repository>, input: &EnsureInput, repository: &str) -> Result<(), Error> {
        let Some(existing) = existing else {
            return Err(Error::FetchFailed {
                repository: repository.to_owned(),
                message: "Repository is unavailable".to_owned(),
            });
        };
        let fetch_failed = |error: opencode_git::OperationError| Error::FetchFailed {
            repository: repository.to_owned(),
            message: error.message,
        };

        self.git.fetch_remotes(existing, true).await.map_err(fetch_failed)?;

        if let Some(branch) = &input.branch {
            self.git.fetch_branch(existing, "origin", branch, true).await.map_err(fetch_failed)?;
        }

        let branch = match &input.branch {
            Some(branch) => Some(branch.clone()),
            None => self.git.default_remote_branch(existing).await,
        };
        if let Some(branch) = &branch {
            self.git.checkout_remote_branch(existing, "origin", branch, true).await.map_err(|error| {
                Error::CheckoutFailed {
                    repository: repository.to_owned(),
                    branch: branch.clone(),
                    message: error.message,
                }
            })?;
        }

        let target = match branch {
            Some(branch) => Some(branch),
            None => self.git.branch(existing).await,
        };
        let revision = target.map_or_else(|| "HEAD".to_owned(), |target| format!("origin/{target}"));
        self.git
            .reset_hard(existing, &revision)
            .await
            .map_err(|error| Error::ResetFailed { repository: repository.to_owned(), message: error.message })
    }

    fn record_attempt(&self, key: &str, now: i64) {
        let value = json!({ "attemptedAt": now });
        if let Err(error) = self.database.read(|connection| opencode_storage::kv::set(connection, key, &value)) {
            log::warn!("repository cache write failed for {key}: {error}");
        }
    }
}

impl Materializer for RepositoryCache {
    fn ensure(&self, input: EnsureInput) -> BoxFuture<'_, Result<EnsureResult, Error>> {
        Box::pin(async move {
            if let Some(branch) = &input.branch {
                repository::validate_branch(branch)
                    .map_err(|error| Error::InvalidBranch { branch: branch.clone(), message: error.to_string() })?;
            }

            let repository = input.reference.label.clone();
            let local_path = repository::cache_path(
                &self.repos.to_string_lossy(),
                &Reference::Remote(input.reference.clone()),
                input.branch.as_deref(),
            );
            let key = format!("repository-cache:{local_path}");
            let clone_target = repository::parse(&input.reference.remote, &self.vars)
                .unwrap_or_else(|| Reference::Remote(input.reference.clone()));

            let lock = self.acquire_lock(&local_path, &key).await?;
            let outcome = self.ensure_locked(&input, &repository, &local_path, &clone_target).await;
            let released = self.release_lock(lock, &local_path).await;

            match (outcome, released) {
                (Ok(result), Ok(())) => Ok(result),
                (Ok(_), Err(error)) => Err(error),
                (Err(error), _) => Err(error),
            }
        })
    }
}

impl RepositoryCache {
    async fn acquire_lock(&self, local_path: &str, key: &str) -> Result<lock::Lock, Error> {
        let locks = self.locks.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || lock::acquire(&locks, &key, &lock::Options::default()))
            .await
            .map_err(|error| Error::LockFailed { local_path: local_path.to_owned(), message: error.to_string() })?
            .map_err(|error| Error::LockFailed { local_path: local_path.to_owned(), message: error.to_string() })
    }

    async fn release_lock(&self, lock: lock::Lock, local_path: &str) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || lock.release())
            .await
            .map_err(|error| Error::LockFailed { local_path: local_path.to_owned(), message: error.to_string() })?
            .map_err(|error| Error::LockFailed { local_path: local_path.to_owned(), message: error.to_string() })
    }
}

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Staging {
    path: String,
    kept: bool,
}

impl Staging {
    fn new(local_path: &str) -> Self {
        let target = Path::new(local_path);
        let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staged = format!(".{name}.clone-{}-{}-{sequence}", std::process::id(), now_ms());
        let path = target.with_file_name(staged).to_string_lossy().into_owned();
        Staging { path, kept: false }
    }

    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let path = std::mem::take(&mut self.path);
        let remove = move || {
            if let Err(error) = std::fs::remove_dir_all(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("failed to remove the partial clone {path}: {error}");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(remove)),
            Err(_) => remove(),
        }
    }
}

fn cache_operation(operation: &str, path: &str, error: impl std::fmt::Display) -> Error {
    Error::CacheOperation { operation: operation.to_owned(), path: path.to_owned(), message: error.to_string() }
}
