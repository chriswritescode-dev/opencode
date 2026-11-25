use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use opencode_config::observer::ObserverHandle;
use opencode_config::service::ConfigService;
use opencode_git::{CaptureInput, FileDiff, Git, Repository};
use opencode_location::location::LocationInfo;
use opencode_location::project::Vcs;
use opencode_location::state::{Options, Registration, State, TransformError};
use opencode_util::path::{Flavor, realpath};
use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};

const PACK_INTERVAL: Duration = Duration::from_secs(10 * 60);
const MAXIMUM_UNTRACKED_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotOperation {
    Capture,
    Files,
    Diff,
    Restore,
}

impl SnapshotOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Files => "files",
            Self::Diff => "diff",
            Self::Restore => "restore",
        }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct SnapshotError {
    pub operation: SnapshotOperation,
    pub message: String,
}

#[derive(Clone, Copy, Debug)]
pub struct DiffInput<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub context: Option<i32>,
    pub paths: Option<&'a [String]>,
}

#[derive(Clone, Copy, Debug)]
pub struct RestoreInput<'a> {
    pub files: &'a [(String, String)],
}

#[derive(Clone, Debug)]
pub struct ResolvedRepository {
    source: Repository,
    worktree: String,
    repository: Repository,
}

pub type RepositoryFuture = futures_util::future::BoxFuture<'static, Result<ResolvedRepository, SnapshotError>>;

pub type RepositoryProvider = Arc<dyn Fn() -> RepositoryFuture + Send + Sync>;

struct SnapshotState {
    enabled: bool,
}

pub struct Editor<'a> {
    state: &'a mut SnapshotState,
}

impl Editor<'_> {
    pub fn configure(&mut self, enabled: bool) {
        self.state.enabled = enabled;
    }
}

struct RepositorySlot {
    started: AtomicBool,
    result: tokio::sync::watch::Sender<Option<Result<ResolvedRepository, SnapshotError>>>,
    provider: RepositoryProvider,
}

pub struct Snapshot {
    git: Arc<Git>,
    directory: String,
    vcs: Option<Vcs>,
    state: State<SnapshotState>,
    configured: Arc<RwLock<Option<bool>>>,
    registration: Mutex<Option<Registration>>,
    observer: Mutex<Option<ObserverHandle>>,
    slot: Arc<RepositorySlot>,
    last_pack_check: Mutex<Option<Instant>>,
}

impl Snapshot {
    pub fn new(git: Arc<Git>, info: &LocationInfo, data: impl Into<PathBuf>) -> Self {
        Self::new_with_repository(git, info, data, |provider| provider)
    }

    pub fn new_with_repository(
        git: Arc<Git>,
        info: &LocationInfo,
        data: impl Into<PathBuf>,
        wrap: impl FnOnce(RepositoryProvider) -> RepositoryProvider,
    ) -> Self {
        let provider =
            default_provider(git.clone(), info.project.directory.clone(), info.project.id.clone(), data.into());
        let slot = Arc::new(RepositorySlot {
            started: AtomicBool::new(false),
            result: tokio::sync::watch::channel(None).0,
            provider: wrap(provider),
        });
        Snapshot {
            git,
            directory: info.directory.clone(),
            vcs: info.vcs.clone(),
            state: State::new(Options {
                name: Some("snapshot".to_owned()),
                initial: Box::new(|| SnapshotState { enabled: true }),
                notify: None,
            }),
            configured: Arc::new(RwLock::new(None)),
            registration: Mutex::new(None),
            observer: Mutex::new(None),
            slot,
            last_pack_check: Mutex::new(None),
        }
    }

    pub async fn capture(&self) -> Option<String> {
        if !self.enabled().await {
            return None;
        }
        match self.capture_inner().await {
            Ok(tree) => Some(tree),
            Err(error) => {
                log::warn!("failed to capture snapshot: {error}");
                None
            }
        }
    }

    pub async fn files(&self, from: &str, to: &str) -> Result<Vec<String>, SnapshotError> {
        let resolved = self.repository().await.map_err(|error| remap(error, SnapshotOperation::Files))?;
        let changed = Box::pin(self.git.tree_files(&resolved.repository, from, to))
            .await
            .map_err(|error| git_failure(SnapshotOperation::Files, error))?;
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let skipped = Box::pin(self.git.index_ignored(&resolved.source, &changed))
            .await
            .map_err(|error| git_failure(SnapshotOperation::Files, error))?;
        Ok(changed.into_iter().filter(|file| !skipped.contains(file)).collect())
    }

    pub async fn diff(&self, input: DiffInput<'_>) -> Result<Vec<FileDiff>, SnapshotError> {
        if input.paths.is_some_and(|paths| paths.is_empty()) {
            return Ok(Vec::new());
        }
        let resolved = self.repository().await.map_err(|error| remap(error, SnapshotOperation::Diff))?;
        let diffs =
            Box::pin(self.git.tree_diff(&resolved.repository, input.from, input.to, input.context, input.paths))
                .await
                .map_err(|error| git_failure(SnapshotOperation::Diff, error))?;
        if diffs.is_empty() {
            return Ok(Vec::new());
        }
        let paths = diffs.iter().map(|diff| diff.file.clone()).collect::<Vec<_>>();
        let skipped = Box::pin(self.git.index_ignored(&resolved.source, &paths))
            .await
            .map_err(|error| git_failure(SnapshotOperation::Diff, error))?;
        Ok(diffs.into_iter().filter(|diff| !skipped.contains(&diff.file)).collect())
    }

    pub async fn restore(&self, input: RestoreInput<'_>) -> Result<(), SnapshotError> {
        if !self.enabled().await {
            return Err(SnapshotError {
                operation: SnapshotOperation::Restore,
                message: "Snapshots are disabled".to_owned(),
            });
        }
        let resolved = self.repository().await.map_err(|error| remap(error, SnapshotOperation::Restore))?;
        let mut planned = Vec::with_capacity(input.files.len());
        for (file, tree) in input.files {
            let absolute = Flavor::NATIVE.resolve(&[&resolved.worktree, file]);
            if !contains(&resolved.worktree, &absolute) {
                return Err(SnapshotError {
                    operation: SnapshotOperation::Restore,
                    message: format!("Path escapes the project: {file}"),
                });
            }
            planned.push((file.clone(), tree.clone()));
        }
        Box::pin(self.git.tree_restore(&resolved.repository, &planned))
            .await
            .map_err(|error| git_failure(SnapshotOperation::Restore, error))?;
        Ok(())
    }

    pub async fn transform(
        &self,
        edit: impl for<'a, 'b> Fn(&'b mut Editor<'a>) + Send + Sync + 'static,
    ) -> Registration {
        self.state
            .transform(move |state| {
                let mut editor = Editor { state };
                edit(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }

    pub async fn observe_config(&self, config: &ConfigService) {
        let slot = self.configured.clone();
        *slot.write().unwrap_or_else(|error| error.into_inner()) =
            opencode_config::entry::latest(&config.entries(), |info| info.snapshots);
        let registration = self
            .state
            .transform(move |state: &mut SnapshotState| {
                let configured = *slot.read().unwrap_or_else(|error| error.into_inner());
                if let Some(configured) = configured {
                    state.enabled = configured;
                }
                Ok::<(), TransformError>(())
            })
            .await;
        *self.registration.lock().unwrap_or_else(|error| error.into_inner()) = Some(registration);
        let slot = self.configured.clone();
        let state = self.state.clone();
        let observer = opencode_config::observer::observe(config, move |entries| {
            *slot.write().unwrap_or_else(|error| error.into_inner()) =
                opencode_config::entry::latest(entries, |info| info.snapshots);
            state.invalidate();
        });
        *self.observer.lock().unwrap_or_else(|error| error.into_inner()) = Some(observer);
    }

    async fn capture_inner(&self) -> Result<String, SnapshotError> {
        let resolved = self.repository().await?;
        let scope = self.scope(&resolved.worktree)?;
        let tree = Box::pin(self.git.tree_capture(CaptureInput {
            repository: resolved.repository.clone(),
            scopes: vec![scope],
            ignores: Some(resolved.source.clone()),
            maximum_untracked_file_bytes: Some(MAXIMUM_UNTRACKED_FILE_BYTES),
        }))
        .await
        .map_err(|error| git_failure(SnapshotOperation::Capture, error))?;
        self.pack_when_due(&resolved.repository);
        Ok(tree)
    }

    async fn enabled(&self) -> bool {
        if !matches!(self.vcs, Some(Vcs::Git { .. })) {
            return false;
        }
        match self.state.get().await {
            Ok(state) => state.enabled,
            Err(error) => {
                log::warn!("failed to read snapshot state: {error}");
                true
            }
        }
    }

    fn scope(&self, worktree: &str) -> Result<String, SnapshotError> {
        let directory = realpath(Path::new(&self.directory))
            .ok_or_else(|| capture_failure(format!("Failed to resolve the location directory: {}", self.directory)))?;
        let directory = directory.to_string_lossy().into_owned();
        if !contains(worktree, &directory) {
            return Err(capture_failure("Location is outside the project"));
        }
        let relative = Flavor::NATIVE.relative(worktree, &directory).replace('\\', "/");
        Ok(if relative.is_empty() { ".".to_owned() } else { relative })
    }

    fn pack_when_due(&self, repository: &Repository) {
        let due = {
            let mut last = self.last_pack_check.lock().unwrap_or_else(|error| error.into_inner());
            let due = last.is_none_or(|instant| instant.elapsed() >= PACK_INTERVAL);
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if !due {
            return;
        }
        let git = self.git.clone();
        let repository = repository.clone();
        tokio::spawn(async move {
            if let Err(error) = git.objects_pack(&repository).await {
                log::warn!("failed to pack snapshot objects: {error}");
            }
        });
    }

    async fn repository(&self) -> Result<ResolvedRepository, SnapshotError> {
        let mut receiver = self.slot.result.subscribe();
        let current = (*receiver.borrow()).clone();
        if let Some(result) = current {
            return result;
        }
        if self.slot.started.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            let provider = self.slot.provider.clone();
            let result = self.slot.result.clone();
            tokio::spawn(async move {
                let value = provider().await;
                result.send_replace(Some(value));
            });
        }
        loop {
            let current = (*receiver.borrow()).clone();
            if let Some(result) = current {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err(capture_failure("Snapshot repository discovery was interrupted"));
            }
        }
    }
}

fn default_provider(git: Arc<Git>, directory: String, project_id: String, data: PathBuf) -> RepositoryProvider {
    Arc::new(move || -> RepositoryFuture {
        let git = git.clone();
        let directory = directory.clone();
        let project_id = project_id.clone();
        let data = data.clone();
        Box::pin(async move { discover(git, &directory, &project_id, &data).await })
    })
}

async fn discover(
    git: Arc<Git>,
    directory: &str,
    project_id: &str,
    data: &Path,
) -> Result<ResolvedRepository, SnapshotError> {
    let Some(source) = git.discover(directory).await else {
        return Err(capture_failure("Project is not a Git repository"));
    };
    let worktree = realpath(Path::new(&source.worktree))
        .ok_or_else(|| capture_failure(format!("Failed to resolve the worktree: {}", source.worktree)))?
        .to_string_lossy()
        .into_owned();
    let git_directory = data.join("snapshot").join(project_id).join(sha1_hex(&worktree));
    let git_directory = git_directory.to_string_lossy().into_owned();
    if Path::new(&git_directory).join("HEAD").exists() {
        return Ok(ResolvedRepository {
            source,
            worktree: worktree.clone(),
            repository: Repository {
                worktree: worktree.clone(),
                git_directory: git_directory.clone(),
                common_directory: git_directory,
            },
        });
    }
    let repository = git
        .create_repository(&worktree, &git_directory, Some(&source))
        .await
        .map_err(|error| git_failure(SnapshotOperation::Capture, error))?;
    Ok(ResolvedRepository { source, worktree, repository })
}

fn sha1_hex(value: &str) -> String {
    let hash = digest(&SHA1_FOR_LEGACY_USE_ONLY, value.as_bytes());
    let bytes: &[u8] = hash.as_ref();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn contains(parent: &str, child: &str) -> bool {
    let relative = Flavor::NATIVE.relative(parent, child);
    relative.is_empty() || !Flavor::NATIVE.outside(&relative)
}

fn capture_failure(message: impl Into<String>) -> SnapshotError {
    SnapshotError { operation: SnapshotOperation::Capture, message: message.into() }
}

fn git_failure(operation: SnapshotOperation, error: impl std::fmt::Display) -> SnapshotError {
    SnapshotError { operation, message: error.to_string() }
}

fn remap(mut error: SnapshotError, operation: SnapshotOperation) -> SnapshotError {
    if error.operation == operation {
        return error;
    }
    error.operation = operation;
    error
}
